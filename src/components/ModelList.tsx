import { useState, type ReactNode } from "react";
import type { EngineStatus } from "../bindings/EngineStatus";
import { api, errorMessage, type ModelInfo } from "../lib/api";
import { formatSize } from "../lib/format";

interface Props {
  models: ModelInfo[];
  engine: EngineStatus | undefined;
}

function useRun() {
  const [error, setError] = useState<string | null>(null);
  const run = (action: () => Promise<unknown>) => {
    setError(null);
    action().catch((err: unknown) => setError(errorMessage(err)));
  };
  return { error, run };
}

export function ModelList({ models, engine }: Props) {
  const { error, run } = useRun();

  return (
    <div className="models">
      {models.map((model) => (
        <ModelRow key={model.id} model={model} engine={engine} run={run} />
      ))}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** One dropdown for every model, with the chosen model's details and actions below it. */
export function ModelPicker({
  models,
  engine,
  children,
}: Props & { children?: ReactNode }) {
  const { error, run } = useRun();
  const [chosen, setChosen] = useState<string | null>(null);
  const selected =
    models.find((model) => model.id === chosen) ??
    models.find((model) => model.active) ??
    models.find((model) => model.recommended) ??
    models[0];
  if (!selected) {
    return null;
  }

  return (
    <div className="models">
      <div className="field-pair">
        <label className="field">
          <span>Model</span>
          <select
            className="input"
            value={selected.id}
            onChange={(e) => setChosen(e.target.value)}
          >
            {models.map((model) => (
              <option key={model.id} value={model.id}>
                {model.name}
              </option>
            ))}
          </select>
        </label>
        {children}
      </div>
      <ModelRow model={selected} engine={engine} run={run} named={false} />
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

interface RowProps {
  model: ModelInfo;
  engine: EngineStatus | undefined;
  run: (action: () => Promise<unknown>) => void;
}

function ModelRow({
  model,
  engine,
  run,
  named = true,
}: RowProps & { named?: boolean }) {
  const details = [
    model.recommended && "Recommended",
    formatSize(model.size),
    model.quantization,
  ].filter(Boolean);

  return (
    <div className="model" aria-label={model.name}>
      <div className="model-header">
        <div>
          {named && <p className="model-name">{model.name}</p>}
          <p className="model-details">{details.join(" · ")}</p>
        </div>
        <ModelActions model={model} engine={engine} run={run} />
      </div>
      <DownloadProgress model={model} />
      {model.warning && <p className="model-note">{model.warning}</p>}
      {model.memoryWarning && !model.download && (
        <p className="model-note">May not fit in available memory.</p>
      )}
    </div>
  );
}

function ModelActions({ model, engine, run }: RowProps) {
  const download = model.download;
  const engineModel = engine && "model" in engine ? engine.model : null;

  if (model.installed && model.active) {
    if (engineModel === model.id && engine?.state === "loading") {
      return <span className="model-state">Loading</span>;
    }
    if (engineModel === model.id && engine?.state === "failed") {
      return (
        <button
          className="button-secondary"
          type="button"
          onClick={() => run(() => api.selectModel(model.id))}
        >
          Try again
        </button>
      );
    }
    return <span className="model-state">Active ✓</span>;
  }
  if (model.installed) {
    return (
      <div className="model-actions">
        <button
          className="button-link"
          type="button"
          onClick={() => run(() => api.deleteModel(model.id))}
        >
          Delete
        </button>
        <button
          className="button-secondary"
          type="button"
          onClick={() => run(() => api.selectModel(model.id))}
        >
          Use
        </button>
      </div>
    );
  }
  switch (download?.phase) {
    case "downloading":
      return (
        <div className="model-actions">
          <button
            className="button-link"
            type="button"
            onClick={() => run(() => api.cancelDownload(model.id))}
          >
            Cancel
          </button>
          <button
            className="button-secondary"
            type="button"
            onClick={() => run(() => api.pauseDownload(model.id))}
          >
            Pause
          </button>
        </div>
      );
    case "verifying":
      return <span className="model-state">Verifying</span>;
    case "paused":
    case "failed":
      return (
        <div className="model-actions">
          <button
            className="button-link"
            type="button"
            onClick={() => run(() => api.cancelDownload(model.id))}
          >
            Cancel
          </button>
          <button
            className="button-secondary"
            type="button"
            onClick={() => run(() => api.downloadModel(model.id))}
          >
            {download.phase === "paused" ? "Resume" : "Try again"}
          </button>
        </div>
      );
    default:
      return (
        <button
          className="button-secondary"
          type="button"
          onClick={() => run(() => api.downloadModel(model.id))}
        >
          Download
        </button>
      );
  }
}

function DownloadProgress({ model }: { model: ModelInfo }) {
  const download = model.download;
  if (!download || download.phase === "verifying") {
    return null;
  }
  if (download.phase === "failed") {
    return <p className="model-note error">{download.error}</p>;
  }
  const label = `${formatSize(download.downloaded)} of ${formatSize(model.size)}`;
  return (
    <div className="download">
      <progress
        className="progress"
        max={model.size}
        value={download.downloaded}
        aria-label={`${model.name} download`}
      />
      <p className="model-details">
        {download.phase === "paused" ? `Paused · ${label}` : label}
      </p>
    </div>
  );
}
