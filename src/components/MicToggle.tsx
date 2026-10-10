import { api, type VoiceState } from "../lib/api";
import { useStatus } from "../lib/hooks";

const VOICE_LABELS: Record<VoiceState, string> = {
  off: "Microphone off",
  listening: "Listening",
  processing: "Processing",
  responding: "Responding",
};

/** Turns listening on or off and shows what voice is doing. */
export function MicToggle() {
  const status = useStatus();
  if (!status) {
    return null;
  }
  const voice = status.voice.state;
  const on = voice !== "off";
  // Failures are shown through the voice status, so they are not repeated here.
  const toggle = () =>
    api.setListening(!on).catch((err: unknown) => console.error(err));

  return (
    <button
      className="mic-toggle"
      type="button"
      role="switch"
      data-state={voice}
      aria-checked={on}
      aria-label={VOICE_LABELS[voice]}
      title={VOICE_LABELS[voice]}
      onClick={toggle}
    >
      <span className="mic-knob">
        {on ? <MicrophoneIcon /> : <MicrophoneOffIcon />}
      </span>
    </button>
  );
}

/** Tabler "microphone" icon. */
function MicrophoneIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 2m0 3a3 3 0 0 1 3 -3h0a3 3 0 0 1 3 3v5a3 3 0 0 1 -3 3h0a3 3 0 0 1 -3 -3z" />
      <path d="M5 10a7 7 0 0 0 14 0" />
      <path d="M8 21l8 0" />
      <path d="M12 17l0 4" />
    </svg>
  );
}

/** Tabler "microphone-off" icon. */
function MicrophoneOffIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3 3l18 18" />
      <path d="M9 5a3 3 0 0 1 6 0v5a3 3 0 0 1 -.13 .874m-2 2a3 3 0 0 1 -3.87 -2.872v-1" />
      <path d="M5 10a7 7 0 0 0 10.846 5.85m2 -2a6.967 6.967 0 0 0 1.152 -3.85" />
      <path d="M8 21l8 0" />
      <path d="M12 17l0 4" />
    </svg>
  );
}
