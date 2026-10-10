import { useEffect, useState, type FormEvent } from "react";
import type { ConnectionStatus } from "../bindings/ConnectionStatus";
import { ModelList } from "../components/ModelList";
import { VoiceModels } from "../components/VoiceModels";
import {
  api,
  errorMessage,
  type DiscoveredInstance,
  type Settings,
} from "../lib/api";
import { useModels, useStatus } from "../lib/hooks";

const CONTEXT_LENGTHS = [4096, 8192, 16384, 32768];

const CONNECTION_MESSAGES: Partial<Record<ConnectionStatus, string>> = {
  connecting: "Connecting",
  connected: "Connected",
  reconnecting: "Can't reach Home Assistant. Retrying.",
  authFailed:
    "The access token was rejected. Create a new one in your Home Assistant profile.",
  unsupportedVersion: "Home Assistant 2024.4 or later is required.",
  tokenUnavailable:
    "Luna couldn't read the saved access token. Enter it again.",
};

export function SettingsView() {
  const [saved, setSaved] = useState<Settings | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [token, setToken] = useState("");
  const [hasToken, setHasToken] = useState(false);
  const [discovered, setDiscovered] = useState<DiscoveredInstance[]>([]);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const status = useStatus();
  const { models, error: modelsError } = useModels();

  useEffect(() => {
    Promise.all([api.getSettings(), api.getLaunchAtLogin()])
      .then(([settings, launch]) => {
        setSaved(settings);
        setDraft(settings);
        setLaunchAtLogin(launch);
        if (!settings.homeAssistantUrl) {
          api
            .discoverHomeAssistant()
            .then(setDiscovered)
            .catch((err: unknown) => setError(errorMessage(err)));
        }
      })
      .catch((err: unknown) => setError(errorMessage(err)));
    api
      .hasHomeAssistantToken()
      .then(setHasToken)
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  if (!draft) {
    return error ? <p role="alert">{error}</p> : null;
  }

  const update = (changes: Partial<Settings>) =>
    setDraft({ ...draft, ...changes });
  const changed =
    token.trim() !== "" || JSON.stringify(draft) !== JSON.stringify(saved);
  const connection = status && CONNECTION_MESSAGES[status.homeAssistant];
  const suggestions = discovered.filter(
    (instance) => instance.url !== draft.homeAssistantUrl,
  );

  async function save(event: FormEvent) {
    event.preventDefault();
    if (!draft) return;
    setSaving(true);
    setError(null);
    try {
      const settings = await api.saveSettings(draft, token.trim() || null);
      setSaved(settings);
      setDraft(settings);
      setHasToken(
        settings.homeAssistantUrl !== "" && (hasToken || token.trim() !== ""),
      );
      setToken("");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  }

  function run(action: () => Promise<unknown>) {
    setError(null);
    action().catch((err: unknown) => setError(errorMessage(err)));
  }

  return (
    <form className="settings" onSubmit={save} noValidate>
      <h1>Settings</h1>

      <fieldset>
        <legend>Home Assistant</legend>
        {connection && <p className="status-line">{connection}</p>}
        {suggestions.map((instance) => (
          <button
            key={instance.url}
            type="button"
            className="suggestion"
            onClick={() => update({ homeAssistantUrl: instance.url })}
          >
            <span>{instance.name}</span>
            <span className="model-details">{instance.url}</span>
          </button>
        ))}
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
        <label className="field">
          <span>Access token</span>
          <input
            className="input"
            type="password"
            autoComplete="off"
            placeholder={hasToken ? "Saved" : ""}
            value={token}
            onChange={(e) => setToken(e.target.value)}
          />
        </label>
      </fieldset>

      <fieldset>
        <legend>AI Models</legend>
        {models && <ModelList models={models} engine={status?.engine} />}
        {modelsError && <p className="error">{modelsError}</p>}
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
        <legend>Voice</legend>
        <VoiceModels />
      </fieldset>

      <fieldset>
        <legend>General</legend>
        <label className="toggle">
          <span>Launch at login</span>
          <input
            type="checkbox"
            role="switch"
            checked={launchAtLogin}
            onChange={(e) =>
              run(() =>
                api.setLaunchAtLogin(e.target.checked).then(setLaunchAtLogin),
              )
            }
          />
        </label>
        <div className="toggle">
          <span>Conversation history</span>
          <button
            className="button-link"
            type="button"
            onClick={() => run(api.clearHistory)}
          >
            Clear
          </button>
        </div>
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
