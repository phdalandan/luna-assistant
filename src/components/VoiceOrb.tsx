import { useEffect, useRef } from "react";
import { events, type VoiceState } from "../lib/api";
import { useEvent } from "../lib/hooks";

const SIZE = 160;
const CENTER = SIZE / 2;
/** The circle; ripples spread from it to the edge of the canvas. */
const RADIUS = 50;

/** Waves across the circle: shape, starting offset, flow speed, and relative strength. */
const WAVES = [
  { frequency: 1.5, phase: 0, speed: 0.9, strength: 1 },
  { frequency: 2.2, phase: 2.1, speed: -1.2, strength: 0.7 },
  { frequency: 3, phase: 4.2, speed: 1.6, strength: 0.45 },
];

interface Layers {
  waves: number;
  dots: number;
  ripples: number;
}

/** How visible each layer is in each state. Layers fade between states. */
const TARGETS: Record<VoiceState, Layers> = {
  off: { waves: 1, dots: 0, ripples: 0 },
  listening: { waves: 1, dots: 0, ripples: 0 },
  processing: { waves: 0, dots: 1, ripples: 0 },
  responding: { waves: 1, dots: 0, ripples: 1 },
};

const COLORS: Record<VoiceState, string> = {
  off: "--orb-idle",
  listening: "--orb-listening",
  processing: "--accent",
  responding: "--orb-speaking",
};

/** A circle of flowing waves that peak with the microphone while listening, three orbiting dots
 * while thinking, and ripples while Luna speaks. Only loudness reaches the window, never audio. */
export function VoiceOrb({ state }: { state: VoiceState }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const level = useRef(0);
  const current = useRef(state);
  const wake = useRef<() => void>(() => {});

  useEvent(() =>
    events.onVoiceLevel((value) => {
      level.current = Math.max(level.current, value);
      wake.current();
    }),
  );

  useEffect(() => {
    current.current = state;
    wake.current();
  }, [state]);

  useEffect(() => {
    const element = canvas.current;
    const context = element?.getContext("2d");
    if (!element || !context) return;
    const ratio = window.devicePixelRatio || 1;
    element.width = SIZE * ratio;
    element.height = SIZE * ratio;
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    const styles = getComputedStyle(element);
    const color = (state: VoiceState) =>
      styles.getPropertyValue(COLORS[state]).trim();
    const reduced =
      window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

    const layers: Layers = { ...TARGETS[current.current] };
    let energy = 0;
    let clock = 0;
    let last = performance.now();
    let frame = 0;
    let running = false;

    const step = (now: number) => {
      const elapsed = Math.min((now - last) / 1000, 0.1);
      last = now;
      const state = current.current;
      const target = TARGETS[state];
      const ease = reduced ? 1 : 1 - Math.exp(-elapsed * 8);
      let settled = true;
      for (const key of ["waves", "dots", "ripples"] as const) {
        layers[key] += (target[key] - layers[key]) * ease;
        if (Math.abs(target[key] - layers[key]) < 0.002) {
          layers[key] = target[key];
        } else {
          settled = false;
        }
      }
      const heard =
        state === "listening" || state === "responding" ? level.current : 0;
      // Rises quickly to a peak and settles slowly, like a level meter.
      energy += (heard - energy) * (heard > energy ? 0.35 : 0.08);
      level.current *= 0.9;
      const moving = state !== "off" && !reduced;
      if (moving) clock += elapsed;
      paint(context, {
        layers,
        time: clock,
        energy: reduced ? 0 : energy,
        waveColor: color(state),
        dotColor: color("processing"),
        rippleColor: color("responding"),
      });
      running = moving || !settled || energy > 0.002;
      frame = running ? requestAnimationFrame(step) : 0;
    };
    wake.current = () => {
      if (running) return;
      running = true;
      last = performance.now();
      frame = requestAnimationFrame(step);
    };
    step(last);
    return () => {
      cancelAnimationFrame(frame);
      wake.current = () => {};
    };
  }, []);

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

interface Scene {
  layers: Layers;
  time: number;
  energy: number;
  waveColor: string;
  dotColor: string;
  rippleColor: string;
}

function paint(context: CanvasRenderingContext2D, scene: Scene) {
  const { layers } = scene;
  context.clearRect(0, 0, SIZE, SIZE);
  context.globalAlpha = 0.35;
  context.strokeStyle = scene.waveColor;
  context.lineWidth = 1.5;
  context.beginPath();
  context.arc(CENTER, CENTER, RADIUS, 0, Math.PI * 2);
  context.stroke();
  if (layers.ripples > 0) drawRipples(context, scene);
  if (layers.waves > 0) drawWaves(context, scene);
  if (layers.dots > 0) drawDots(context, scene);
  context.globalAlpha = 1;
}

/** Smooth zigzag lines inside the circle. They flatten at its edge and grow with loudness. */
function drawWaves(context: CanvasRenderingContext2D, scene: Scene) {
  const weight = scene.layers.waves;
  const amplitude = RADIUS * (0.2 + 0.55 * scene.energy) * weight;
  context.save();
  context.beginPath();
  context.arc(CENTER, CENTER, RADIUS - 1, 0, Math.PI * 2);
  context.clip();
  context.strokeStyle = scene.waveColor;
  context.lineCap = "round";
  context.lineJoin = "round";
  for (const wave of WAVES) {
    context.globalAlpha = weight * (0.35 + 0.65 * wave.strength);
    context.lineWidth = 1.2 + wave.strength * 1.3;
    context.beginPath();
    for (let index = 0; index <= 64; index++) {
      const along = index / 64;
      const x = CENTER - RADIUS + along * RADIUS * 2;
      const taper = Math.sin(Math.PI * along) ** 2;
      const phase =
        along * Math.PI * 2 * wave.frequency +
        wave.phase +
        scene.time * wave.speed * 2;
      const shape = Math.sin(phase) * 0.75 + Math.sin(phase * 1.9) * 0.25;
      const y = CENTER + amplitude * wave.strength * taper * shape;
      if (index === 0) context.moveTo(x, y);
      else context.lineTo(x, y);
    }
    context.stroke();
  }
  context.restore();
}

/** Three dots circling the centre while Luna thinks. */
function drawDots(context: CanvasRenderingContext2D, scene: Scene) {
  context.fillStyle = scene.dotColor;
  context.globalAlpha = scene.layers.dots;
  for (let index = 0; index < 3; index++) {
    const angle = scene.time * 3 + (index * Math.PI * 2) / 3;
    const x = CENTER + Math.cos(angle) * RADIUS * 0.45;
    const y = CENTER + Math.sin(angle) * RADIUS * 0.45;
    context.beginPath();
    context.arc(x, y, 4.5, 0, Math.PI * 2);
    context.fill();
  }
}

/** Dashed rings spreading from the circle while Luna speaks, stronger when she is louder. */
function drawRipples(context: CanvasRenderingContext2D, scene: Scene) {
  const reach = CENTER - RADIUS - 2;
  const strength = 0.4 + 0.6 * Math.min(1, scene.energy * 2);
  context.strokeStyle = scene.rippleColor;
  context.setLineDash([3, 4]);
  context.lineDashOffset = -scene.time * 18;
  for (let index = 0; index < 3; index++) {
    const progress = (scene.time * 0.6 + index / 3) % 1;
    context.globalAlpha = scene.layers.ripples * (1 - progress) * strength;
    context.lineWidth = 2 - progress;
    context.beginPath();
    context.arc(CENTER, CENTER, RADIUS + progress * reach, 0, Math.PI * 2);
    context.stroke();
  }
  context.setLineDash([]);
}
