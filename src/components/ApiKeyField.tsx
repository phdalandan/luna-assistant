import { useId } from "react";

interface Props {
  label: string;
  value: string;
  onChange: (key: string) => void;
  saved: boolean;
  onRemove: () => void;
}

/** A password field for an API key. A saved key is never shown, only that one exists. */
export function ApiKeyField({
  label,
  value,
  onChange,
  saved,
  onRemove,
}: Props) {
  const id = useId();
  return (
    <div className="field">
      <div className="field-row">
        <label htmlFor={id}>{label}</label>
        {saved && (
          <button className="button-link" type="button" onClick={onRemove}>
            Remove
          </button>
        )}
      </div>
      <input
        id={id}
        className="input"
        type="password"
        autoComplete="off"
        spellCheck={false}
        placeholder={saved ? "Saved" : ""}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  );
}
