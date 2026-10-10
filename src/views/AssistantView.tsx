import { useEffect, useRef, useState, type FormEvent } from "react";
import { ModelList } from "../components/ModelList";
import {
  api,
  errorMessage,
  events,
  type Interaction,
  type Status,
  type VoiceState,
} from "../lib/api";
import { formatTime } from "../lib/format";
import { useEvent, useModels, useStatus } from "../lib/hooks";

export function AssistantView() {
  const status = useStatus();
  const { models } = useModels();
  const [interactions, setInteractions] = useState<Interaction[]>([]);
  const [pending, setPending] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const end = useRef<HTMLDivElement>(null);
  const stopped = useRef(false);

  useEffect(() => {
    api
      .listInteractions()
      .then(setInteractions)
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  useEvent(() => events.onConversationCleared(() => setInteractions([])));
  useEvent(() =>
    events.onInteractionsChanged(() => {
      api
        .listInteractions()
        .then(setInteractions)
        .catch((err: unknown) => setError(errorMessage(err)));
    }),
  );

  useEffect(() => {
    end.current?.scrollIntoView?.({ block: "end" });
  }, [interactions, pending, error]);

  const activeModelId = models?.find((model) => model.active)?.id;
  useEffect(() => {
    if (!activeModelId) return;
    const prepare = () => {
      api
        .prepareAssistant()
        .catch((err: unknown) => setError(errorMessage(err)));
    };
    prepare();
    window.addEventListener("focus", prepare);
    return () => window.removeEventListener("focus", prepare);
  }, [activeModelId]);

  if (!models) {
    return null;
  }
  const voice = status?.voice.state ?? "off";
  const header = (
    <Header
      voice={voice}
      toggle={() =>
        api
          .setListening(voice === "off")
          .catch((err: unknown) => setError(errorMessage(err)))
      }
    />
  );
  const problem = status?.voice.problem && (
    <p className="detail" role="status">
      {status.voice.problem}
    </p>
  );
  const activeModel = models.find((model) => model.active);
  if (!activeModel) {
    const installed = models.filter((model) => model.installed);
    const shown =
      installed.length > 0
        ? installed
        : models.filter((model) => model.recommended);
    return (
      <section className="assistant">
        {header}
        {problem}
        <div className="onboarding">
          <p className="status">
            {installed.length > 0
              ? "Choose an AI model to get started."
              : "Download an AI model to get started."}
          </p>
          <ModelList models={shown} engine={status?.engine} />
        </div>
      </section>
    );
  }

  const replace = (updated: Interaction) =>
    setInteractions((current) =>
      current.map((interaction) =>
        interaction.id === updated.id ? updated : interaction,
      ),
    );

  async function submit(event: FormEvent) {
    event.preventDefault();
    const request = text.trim();
    if (!request || pending) return;
    setText("");
    setError(null);
    setPending(request);
    stopped.current = false;
    try {
      const interaction = await api.ask(request);
      setInteractions((current) => [...current, interaction]);
    } catch (err) {
      if (!stopped.current) {
        setError(errorMessage(err));
      }
    } finally {
      setPending(null);
    }
  }

  function stop() {
    stopped.current = true;
    api.cancelRequest().catch((err: unknown) => setError(errorMessage(err)));
  }

  function respond(interaction: Interaction, confirmed: boolean) {
    setError(null);
    api
      .confirmAction(interaction.id, confirmed)
      .then(replace)
      .catch((err: unknown) => setError(errorMessage(err)));
  }

  const empty = interactions.length === 0 && !pending && !error;
  const busy = voice === "processing" || voice === "responding";

  return (
    <section className="assistant">
      {header}
      {problem}
      {empty ? (
        <div className="presence">
          <div
            className="orb"
            data-state={busy ? "thinking" : "idle"}
            aria-hidden="true"
          />
          <StatusLine status={status} />
        </div>
      ) : (
        <div className="transcript" aria-live="polite">
          {interactions.map((interaction) => (
            <Exchange
              key={interaction.id}
              interaction={interaction}
              respond={respond}
            />
          ))}
          {pending && (
            <div className="exchange">
              <p className="message message-user">{pending}</p>
              <div
                className="orb orb-small"
                data-state="thinking"
                aria-label="Working"
              />
            </div>
          )}
          {error && (
            <p className="message message-luna error" role="alert">
              {error}
            </p>
          )}
          <div ref={end} />
        </div>
      )}

      <form className="composer" onSubmit={submit}>
        <input
          className="input"
          aria-label="Message"
          placeholder="Ask Luna"
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        {pending ? (
          <button className="button-secondary" type="button" onClick={stop}>
            Stop
          </button>
        ) : (
          <button
            className="button-secondary"
            type="submit"
            disabled={!text.trim()}
          >
            Send
          </button>
        )}
      </form>
    </section>
  );
}

const VOICE_LABELS: Record<VoiceState, string> = {
  off: "Microphone off",
  listening: "Listening",
  processing: "Processing",
  responding: "Responding",
};

function Header({ voice, toggle }: { voice: VoiceState; toggle: () => void }) {
  return (
    <header className="assistant-header">
      <h1>Luna</h1>
      <button
        className="chip"
        type="button"
        data-state={voice}
        aria-pressed={voice !== "off"}
        onClick={toggle}
      >
        {VOICE_LABELS[voice]}
      </button>
    </header>
  );
}

function StatusLine({ status }: { status: Status | null }) {
  if (status?.engine.state === "loading") {
    return <p className="detail">Loading the AI model</p>;
  }
  if (status?.engine.state === "failed") {
    return <p className="detail">Unable to load this model.</p>;
  }
  if (status && status.homeAssistant !== "connected") {
    return (
      <p className="detail">
        Connect Home Assistant in Settings to control your home.
      </p>
    );
  }
  return null;
}

interface ExchangeProps {
  interaction: Interaction;
  respond: (interaction: Interaction, confirmed: boolean) => void;
}

function Exchange({ interaction, respond }: ExchangeProps) {
  return (
    <div className="exchange">
      <p className="message message-user">{interaction.request}</p>
      <div className="message message-luna">
        <p>{interaction.response}</p>
        {interaction.results.map((result) => (
          <p key={result} className="result">
            {result}
          </p>
        ))}
        {interaction.awaitingConfirmation && (
          <div className="model-actions">
            <button
              className="button-link"
              type="button"
              onClick={() => respond(interaction, false)}
            >
              Cancel
            </button>
            <button
              className="button-secondary"
              type="button"
              onClick={() => respond(interaction, true)}
            >
              Confirm
            </button>
          </div>
        )}
        <time className="time">{formatTime(interaction.createdAt)}</time>
      </div>
    </div>
  );
}
