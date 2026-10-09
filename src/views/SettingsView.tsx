import { useEffect, useState, type FormEvent } from "react";
import { api, errorMessage, type Settings } from "../lib/api";

const CONTEXT_LENGTHS = [2048, 4096, 8192, 16384, 32768, 65536, 131072];

export function SettingsView() {
  const [saved, setSaved] = useState<Settings | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([api.getSettings(), api.getLaunchAtLogin()])
      .then(([settings, launch]) => {
        setSaved(settings);
        setDraft(settings);
        setLaunchAtLogin(launch);
      })
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  if (!draft) {
    return error ? <p role="alert">{error}</p> : null;
  }

  const update = (changes: Partial<Settings>) =>
    setDraft({ ...draft, ...changes });

  const changed = JSON.stringify(draft) !== JSON.stringify(saved);

  async function save(event: FormEvent) {
    event.preventDefault();
    if (!draft) return;
    setSaving(true);
    setError(null);
    try {
      const settings = await api.saveSettings(draft);
      setSaved(settings);
      setDraft(settings);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  }

  async function toggleLaunchAtLogin(enabled: boolean) {
    setError(null);
    try {
      setLaunchAtLogin(await api.setLaunchAtLogin(enabled));
    } catch (err) {
      setError(errorMessage(err));
    }
  }

  return (
    <form className="settings" onSubmit={save} noValidate>
      <h1>Settings</h1>

      <fieldset>
        <legend>Home Assistant</legend>
        <label className="field">
          <span>Address</span>
          <input
            className="input"
            type="url"
            placeholder="http://homeassistant.local:8123"
            value={draft.homeAssistantUrl}
            onChange={(e) => update({ homeAssistantUrl: e.target.value })}
          />
        </label>
      </fieldset>

      <fieldset>
        <legend>AI</legend>
        <label className="field">
          <span>Ollama address</span>
          <input
            className="input"
            type="url"
            value={draft.ollamaUrl}
            onChange={(e) => update({ ollamaUrl: e.target.value })}
          />
        </label>
        <label className="field">
          <span>Model</span>
          <input
            className="input"
            value={draft.model}
            onChange={(e) => update({ model: e.target.value })}
          />
        </label>
        <label className="field">
          <span>Context length</span>
          <select
            className="input"
            value={draft.contextLength}
            onChange={(e) => update({ contextLength: Number(e.target.value) })}
          >
            {CONTEXT_LENGTHS.map((length) => (
              <option key={length} value={length}>
                {length.toLocaleString()} tokens
              </option>
            ))}
          </select>
        </label>
      </fieldset>

      <fieldset>
        <legend>General</legend>
        <label className="toggle">
          <span>Launch at login</span>
          <input
            type="checkbox"
            role="switch"
            checked={launchAtLogin}
            onChange={(e) => toggleLaunchAtLogin(e.target.checked)}
          />
        </label>
      </fieldset>

      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}

      <button className="button" type="submit" disabled={!changed || saving}>
        Save
      </button>
    </form>
  );
}
