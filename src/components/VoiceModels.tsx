import { useState } from "react";
import { api, errorMessage } from "../lib/api";
import { formatSize } from "../lib/format";
import { useVoiceModels } from "../lib/hooks";

export function VoiceModels() {
  const { voice, error: loadError } = useVoiceModels();
  const [error, setError] = useState<string | null>(null);

  if (!voice) {
    return loadError ? <p className="error">{loadError}</p> : null;
  }
  const run = (action: () => Promise<unknown>) => {
    setError(null);
    action().catch((err: unknown) => setError(errorMessage(err)));
  };
  const download = voice.download;

  return (
    <div className="model" aria-label="Speech models">
      <div className="model-header">
        <div>
          <p className="model-name">Speech models</p>
          <p className="model-details">{formatSize(voice.size)}</p>
        </div>
        {voice.installed ? (
          <span className="model-state">Installed ✓</span>
        ) : download?.phase === "downloading" ? (
          <button
            className="button-link"
            type="button"
            onClick={() => run(api.cancelVoiceDownload)}
          >
            Cancel
          </button>
        ) : download?.phase === "verifying" ? (
          <span className="model-state">Verifying</span>
        ) : (
          <button
            className="button-secondary"
            type="button"
            onClick={() => run(api.downloadVoiceModels)}
          >
            {download?.phase === "failed" ? "Try again" : "Download"}
          </button>
        )}
      </div>
      {download?.phase === "downloading" && (
        <div className="download">
          <progress
            className="progress"
            max={voice.size}
            value={download.downloaded}
            aria-label="Speech models download"
          />
          <p className="model-details">
            {`${formatSize(download.downloaded)} of ${formatSize(voice.size)}`}
          </p>
        </div>
      )}
      {download?.phase === "failed" && (
        <p className="model-note error">{download.error}</p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
