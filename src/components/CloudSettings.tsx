import type { CloudModelOption, CloudProvider, Settings } from "../lib/api";
import { ApiKeyField } from "./ApiKeyField";

const PROVIDERS: Record<CloudProvider, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
};

interface Props {
  settings: Settings;
  update: (changes: Partial<Settings>) => void;
  models: CloudModelOption[];
  apiKey: string;
  setApiKey: (provider: CloudProvider, key: string) => void;
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
  const model =
    provider === "openai" ? settings.openaiModel : settings.anthropicModel;

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
          onChange={(e) =>
            update({ cloudProvider: e.target.value as CloudProvider })
          }
        >
          {Object.entries(PROVIDERS).map(([id, name]) => (
            <option key={id} value={id}>
              {name}
            </option>
          ))}
        </select>
      </label>
      <ApiKeyField
        label="API key"
        value={apiKey}
        onChange={(key) => setApiKey(provider, key)}
        saved={savedKeys.includes(provider)}
        onRemove={() => removeKey(provider)}
      />
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
