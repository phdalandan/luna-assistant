import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  errorMessage,
  events,
  type ModelInfo,
  type Status,
  type VoiceModelsInfo,
} from "./api";

/** The progress event id for the voice model download. */
const VOICE_DOWNLOAD_ID = "voice";

/** Listens to a backend event for the lifetime of the component. */
export function useEvent(subscribe: () => Promise<() => void>) {
  const initial = useRef(subscribe);
  useEffect(() => {
    let stop: (() => void) | undefined;
    let active = true;
    initial
      .current()
      .then((unlisten) => (active ? (stop = unlisten) : unlisten()))
      .catch((error: unknown) => console.error(error));
    return () => {
      active = false;
      stop?.();
    };
  }, []);
}

export function useStatus(): Status | null {
  const [status, setStatus] = useState<Status | null>(null);
  useEffect(() => {
    api
      .getStatus()
      .then(setStatus)
      .catch((error: unknown) => console.error(error));
  }, []);
  useEvent(() => events.onStatus(setStatus));
  return status;
}

export function useModels() {
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api
      .listModels()
      .then((list) => {
        setModels(list);
        setError(null);
      })
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  useEffect(refresh, [refresh]);
  useEvent(() => events.onModelsChanged(refresh));
  useEvent(() =>
    events.onDownloadProgress(({ id, downloaded }) =>
      setModels(
        (current) =>
          current?.map((model) =>
            model.id === id && model.download
              ? { ...model, download: { ...model.download, downloaded } }
              : model,
          ) ?? null,
      ),
    ),
  );

  return { models, error };
}

export function useVoiceModels() {
  const [voice, setVoice] = useState<VoiceModelsInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api
      .getVoiceModels()
      .then((info) => {
        setVoice(info);
        setError(null);
      })
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  useEffect(refresh, [refresh]);
  useEvent(() => events.onModelsChanged(refresh));
  useEvent(() =>
    events.onDownloadProgress(({ id, downloaded }) => {
      if (id !== VOICE_DOWNLOAD_ID) return;
      setVoice((current) =>
        current?.download
          ? { ...current, download: { ...current.download, downloaded } }
          : current,
      );
    }),
  );

  return { voice, error };
}
