import type { InferenceMode } from "../lib/api";

const MODES: { mode: InferenceMode; label: string }[] = [
  { mode: "local", label: "Local" },
  { mode: "cloud", label: "Cloud" },
];

interface Props {
  label: string;
  value: InferenceMode;
  onChange: (mode: InferenceMode) => void;
}

/** A Local / Cloud segmented control. */
export function ModeControl({ label, value, onChange }: Props) {
  return (
    <div className="segmented" role="radiogroup" aria-label={label}>
      {MODES.map(({ mode, label }) => (
        <button
          key={mode}
          className="segment"
          type="button"
          role="radio"
          aria-checked={value === mode}
          onClick={() => mode !== value && onChange(mode)}
        >
          {label}
        </button>
      ))}
    </div>
  );
}
