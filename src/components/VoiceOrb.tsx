import { useEffect, useRef } from "react";
import { events, type VoiceState } from "../lib/api";
import { useEvent } from "../lib/hooks";

const SIZE = 160;
const SPACING = 5;
/** Ripples around the outline: how many, how far they reach, and how fast they travel. */
const RIPPLES = [
  { count: 3, reach: 0.05, speed: 0.7 },
  { count: 5, reach: 0.04, speed: -1.1 },
  { count: 7, reach: 0.03, speed: 1.6 },
];

/** Colour token, flow speed, and how strongly sound reshapes the outline, for each state. */
const LOOKS: Record<VoiceState, { color: string; speed: number }> = {
  off: { color: "--orb-idle", speed: 0 },
  listening: { color: "--orb-listening", speed: 0.4 },
  processing: { color: "--accent", speed: 0.9 },
  responding: { color: "--orb-speaking", speed: 1 },
};

/** A halftone blob whose outline peaks with the microphone while listening and with Luna's voice
 * while replying. Only loudness reaches the window, never audio. */
export function VoiceOrb({ state }: { state: VoiceState }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const level = useRef(0);

  useEvent(() =>
    events.onVoiceLevel((value) => {
      level.current = Math.max(level.current, value);
    }),
  );

  useEffect(() => {
    const element = canvas.current;
    const context = element?.getContext("2d");
    if (!element || !context) return;
    const ratio = window.devicePixelRatio || 1;
    element.width = SIZE * ratio;
    element.height = SIZE * ratio;
    context.setTransform(ratio, 0, 0, ratio, 0, 0);

    const look = LOOKS[state];
    context.fillStyle = getComputedStyle(element)
      .getPropertyValue(look.color)
      .trim();
    const still =
      look.speed === 0 ||
      window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    let energy = 0;
    let frame = 0;
    const started = performance.now();

    const draw = (now: number) => {
      const seconds = (now - started) / 1000;
      const target = loudness(state, seconds, level.current);
      // Rises quickly to a peak and settles slowly, like a level meter.
      energy += (target - energy) * (target > energy ? 0.35 : 0.08);
      level.current *= 0.9;
      paint(context, seconds * look.speed, energy);
      if (!still) frame = requestAnimationFrame(draw);
    };
    draw(started);
    return () => cancelAnimationFrame(frame);
  }, [state]);

  return (
    <canvas
      ref={canvas}
      className="orb"
      data-state={state}
      style={{ width: SIZE, height: SIZE }}
      aria-hidden="true"
    />
  );
}

/** The microphone while listening, Luna's own voice while replying, a rhythm while thinking. */
function loudness(state: VoiceState, seconds: number, measured: number) {
  switch (state) {
    case "listening":
    case "responding":
      return measured;
    case "processing":
      return 0.2 + 0.1 * Math.sin(seconds * 4);
    case "off":
      return 0;
  }
}

/** The outline's distance from the centre at an angle, as a fraction of the canvas radius. */
function outline(angle: number, time: number, energy: number) {
  let radius = 0.62 + energy * 0.06;
  for (const { count, reach, speed } of RIPPLES) {
    radius +=
      reach * (0.6 + energy * 4) * Math.sin(count * angle + time * speed * 3);
  }
  return Math.min(radius, 0.98);
}

/** Dots inside the outline, largest at the centre and fading towards the edge. */
function paint(
  context: CanvasRenderingContext2D,
  time: number,
  energy: number,
) {
  const half = SIZE / 2;
  context.clearRect(0, 0, SIZE, SIZE);
  for (let y = SPACING / 2; y < SIZE; y += SPACING) {
    for (let x = SPACING / 2; x < SIZE; x += SPACING) {
      const dx = x - half;
      const dy = y - half;
      const distance = Math.hypot(dx, dy) / half;
      const edge = outline(Math.atan2(dy, dx), time, energy);
      if (distance > edge) continue;
      const depth = 1 - distance / edge;
      const size = Math.min(1, 0.25 + depth * 1.1);
      context.beginPath();
      context.arc(x, y, (SPACING / 2) * size, 0, Math.PI * 2);
      context.fill();
    }
  }
}
