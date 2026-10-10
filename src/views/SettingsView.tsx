import { useEffect, useState, type FormEvent } from "react";
import type { ConnectionStatus } from "../bindings/ConnectionStatus";
import { ApiKeyField } from "../components/ApiKeyField";
import { CloudSettings } from "../components/CloudSettings";
import { ModeControl } from "../components/ModeControl";
import { ModelPicker } from "../components/ModelList";
import { VoiceModels } from "../components/VoiceModels";
import {
  api,
  errorMessage,
  type CloudModelOption,
  type CloudProvider,
  type DiscoveredInstance,
  type InferenceMode,
  type MicrophoneOption,
  type Settings,
  type VoiceOption,
} from "../lib/api";
import { useModels, useStatus } from "../lib/hooks";

const CONTEXT_LENGTHS = [4096, 8192, 16384, 32768];

type ApiKeys = Partial<Record<CloudProvider, string>>;

/** Keys the user typed, without blanks. */
function typedKeys(keys: ApiKeys): ApiKeys {
  return Object.fromEntries(
    Object.entries(keys)
      .map(([provider, key]) => [provider, key.trim()])
      .filter(([, key]) => key !== ""),
  );
}

const CONNECTION_BADGES: Partial<
  Record<ConnectionStatus, { label: string; tone: string }>
> = {
  connecting: { label: "Connecting", tone: "neutral" },
  connected: { label: "Connected", tone: "success" },
  reconnecting: { label: "Reconnecting", tone: "warning" },
};

const CONNECTION_MESSAGES: Partial<Record<ConnectionStatus, string>> = {
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
  const [apiKeys, setApiKeys] = useState<ApiKeys>({});
  const [savedKeys, setSavedKeys] = useState<CloudProvider[]>([]);
  const [cloudModels, setCloudModels] = useState<CloudModelOption[]>([]);
  const [discovered, setDiscovered] = useState<DiscoveredInstance[]>([]);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [voices, setVoices] = useState<VoiceOption[]>([]);
  const [microphones, setMicrophones] = useState<MicrophoneOption[]>([]);
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
    api
      .listVoices()
      .then(setVoices)
      .catch((err: unknown) => setError(errorMessage(err)));
    api
      .listMicrophones()
      .then(setMicrophones)
      .catch((err: unknown) => setError(errorMessage(err)));
    api
      .savedApiKeys()
      .then(setSavedKeys)
      .catch((err: unknown) => setError(errorMessage(err)));
    api
      .listCloudModels()
      .then(setCloudModels)
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  if (!draft) {
    return error ? <p role="alert">{error}</p> : null;
  }

  const update = (changes: Partial<Settings>) =>
    setDraft({ ...draft, ...changes });
  const typed = typedKeys(apiKeys);
  const changed =
    token.trim() !== "" ||
    Object.keys(typed).length > 0 ||
    JSON.stringify(draft) !== JSON.stringify(saved);
  const setApiKey = (provider: CloudProvider, key: string) =>
    setApiKeys((keys) => ({ ...keys, [provider]: key }));
  const badge = status && CONNECTION_BADGES[status.homeAssistant];
  const connection = status && CONNECTION_MESSAGES[status.homeAssistant];
  const microphoneMissing =
    draft.microphone !== null &&
    !microphones.some((microphone) => microphone.id === draft.microphone);
  const suggestions = discovered.filter(
    (instance) => instance.url !== draft.homeAssistantUrl,
  );

  async function save(event: FormEvent) {
    event.preventDefault();
    if (!draft) return;
    setSaving(true);
    setError(null);
    try {
      const settings = await api.saveSettings(
        draft,
        token.trim() || null,
        typed,
      );
      setSaved(settings);
      setDraft(settings);
      setHasToken(
        settings.homeAssistantUrl !== "" && (hasToken || token.trim() !== ""),
      );
      setToken("");
      const added = Object.keys(typed) as CloudProvider[];
      setSavedKeys((keys) => [
        ...keys.filter((key) => !added.includes(key)),
        ...added,
      ]);
      setApiKeys({});
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

  /** Switches immediately, like choosing a model, rather than waiting for Save. */
  function switchMode(inference: InferenceMode) {
    if (!draft) return;
    setSaved((current) => current && { ...current, inference });
    setDraft({ ...draft, inference });
    run(() => api.setInferenceMode(inference));
  }

  function switchSpeechRecognition(speechRecognition: InferenceMode) {
    if (!draft) return;
    setSaved((current) => current && { ...current, speechRecognition });
    setDraft({ ...draft, speechRecognition });
    run(() => api.setSpeechRecognition(speechRecognition));
  }

  function removeKey(provider: CloudProvider) {
    run(() =>
      api
        .removeApiKey(provider)
        .then(() =>
          setSavedKeys((keys) => keys.filter((key) => key !== provider)),
        ),
    );
  }

  return (
    <form className="settings" onSubmit={save} noValidate>
      <h1>Settings</h1>

      <fieldset>
        <legend className="legend-row">
          <span>Home Assistant</span>
          {badge && (
            <span className="badge" data-tone={badge.tone}>
              {badge.label}
            </span>
          )}
        </legend>
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
        <input
          className="input"
          type="url"
          aria-label="Address"
          placeholder="http://homeassistant.local:8123"
          value={draft.homeAssistantUrl}
          onChange={(e) => update({ homeAssistantUrl: e.target.value })}
        />
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
        <ModeControl
          label="AI location"
          value={draft.inference}
          onChange={switchMode}
        />
        {draft.inference === "local" ? (
          <>
            {models && (
              <ModelPicker models={models} engine={status?.engine}>
                <label className="field">
                  <span>Context length</span>
                  <select
                    className="input"
                    value={draft.contextLength}
                    onChange={(e) =>
                      update({ contextLength: Number(e.target.value) })
                    }
                  >
                    {CONTEXT_LENGTHS.map((length) => (
                      <option key={length} value={length}>
                        {length.toLocaleString()} tokens
                      </option>
                    ))}
                  </select>
                </label>
              </ModelPicker>
            )}
            {modelsError && <p className="error">{modelsError}</p>}
          </>
        ) : (
          <CloudSettings
            settings={draft}
            update={update}
            models={cloudModels}
            apiKey={apiKeys[draft.cloudProvider] ?? ""}
            setApiKey={setApiKey}
            savedKeys={savedKeys}
            removeKey={removeKey}
          />
        )}
      </fieldset>

      <fieldset>
        <legend>Speech</legend>
        <VoiceModels />
        <label className="field">
          <span>Voice</span>
          <select
            className="input"
            value={draft.voice}
            onChange={(e) => update({ voice: e.target.value })}
          >
            {voices.map((voice) => (
              <option key={voice.id} value={voice.id}>
                {voice.name} ({voice.accent})
              </option>
            ))}
          </select>
        </label>
        <div className="field">
          <span>Speech recognition</span>
          <ModeControl
            label="Speech recognition"
            value={draft.speechRecognition}
            onChange={switchSpeechRecognition}
          />
        </div>
        {draft.speechRecognition === "cloud" && (
          <ApiKeyField
            label="OpenAI API key"
            value={apiKeys.openai ?? ""}
            onChange={(key) => setApiKey("openai", key)}
            saved={savedKeys.includes("openai")}
            onRemove={() => removeKey("openai")}
          />
        )}
        <label className="field">
          <span>Wake phrase</span>
          <input
            className="input"
            autoComplete="off"
            spellCheck={false}
            value={draft.wakeWord}
            onChange={(e) => update({ wakeWord: e.target.value })}
          />
        </label>
      </fieldset>

      <fieldset>
        <legend>Microphone</legend>
        <select
          className="input"
          aria-label="Microphone"
          value={draft.microphone ?? ""}
          onChange={(e) => update({ microphone: e.target.value || null })}
        >
          <option value="">System default</option>
          {microphones.map((microphone) => (
            <option key={microphone.id} value={microphone.id}>
              {microphone.name}
            </option>
          ))}
          {microphoneMissing && (
            <option value={draft.microphone ?? ""}>Not connected</option>
          )}
        </select>
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
