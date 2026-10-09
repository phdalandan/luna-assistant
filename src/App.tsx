import { useState } from "react";
import { AssistantView } from "./views/AssistantView";
import { SettingsView } from "./views/SettingsView";

type View = "assistant" | "settings";

const VIEWS: { id: View; label: string }[] = [
  { id: "assistant", label: "Assistant" },
  { id: "settings", label: "Settings" },
];

export function App() {
  const [view, setView] = useState<View>("assistant");

  return (
    <div className="app">
      <nav className="tabs" aria-label="Sections">
        {VIEWS.map(({ id, label }) => (
          <button
            key={id}
            type="button"
            className="tab"
            aria-current={view === id ? "page" : undefined}
            onClick={() => setView(id)}
          >
            {label}
          </button>
        ))}
      </nav>
      <main className="content">
        {view === "assistant" ? <AssistantView /> : <SettingsView />}
      </main>
    </div>
  );
}
