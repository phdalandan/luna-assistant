import type { CloudModelOption, CloudProvider, Settings } from "../lib/api";

const PROVIDERS: Record<CloudProvider, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
};

interface Props {
  settings: Settings;
  update: (changes: Partial<Settings>) => void;
  models: CloudModelOption[];
  apiKey: string;
  setApiKey: (key: string) => void;
  savedKeys: CloudProvider[];
  removeKey: (provider: CloudProvider) => void;
}

/** Provider, API key, and model for cloud inference. Keys are saved with the form. */
export function CloudSettings({
  settings,
  update,
  models,
  apiKey,
  setApiKey,
  savedKeys,
  removeKey,
}: Props) {
  const provider = settings.cloudProvider;
  const saved = savedKeys.includes(provider);
  const model =
    provider === "openai" ? settings.openaiModel : settings.anthropicModel;

  function chooseProvider(next: CloudProvider) {
    // A typed key belongs to the provider it was typed for.
    setApiKey("");
    update({ cloudProvider: next });
  }

  function chooseModel(id: string) {
    update(
      provider === "openai" ? { openaiModel: id } : { anthropicModel: id },
    );
  }

  return (
    <>
      <label className="field">
        <span>Provider</span>
        <select
          className="input"
          value={provider}
          onChange={(e) => chooseProvider(e.target.value as CloudProvider)}
        >
          {Object.entries(PROVIDERS).map(([id, name]) => (
            <option key={id} value={id}>
              {name}
            </option>
          ))}
        </select>
      </label>
      <div className="field">
        <div className="field-row">
          <label htmlFor="api-key">API key</label>
          {saved && (
            <button
              className="button-link"
              type="button"
              onClick={() => removeKey(provider)}
            >
              Remove
            </button>
          )}
        </div>
        <input
          id="api-key"
          className="input"
          type="password"
          autoComplete="off"
          spellCheck={false}
          placeholder={saved ? "Saved" : ""}
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
        />
      </div>
      <label className="field">
        <span>Model</span>
        <select
          className="input"
          value={model}
          onChange={(e) => chooseModel(e.target.value)}
        >
          {models
            .filter((option) => option.provider === provider)
            .map((option) => (
              <option key={option.id} value={option.id}>
                {option.name}
              </option>
            ))}
        </select>
      </label>
      <p className="status-line">
        Commands and relevant home data are sent to your selected provider.
      </p>
    </>
  );
}
