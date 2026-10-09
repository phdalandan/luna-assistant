import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Settings } from "../lib/api";
import { SettingsView } from "./SettingsView";

const stored: Settings = {
  homeAssistantUrl: "",
  ollamaUrl: "http://127.0.0.1:11434",
  model: "qwen3:8b",
  contextLength: 8192,
};

function mockBackend(onSave: (settings: Settings) => Settings): {
  saved: Settings[];
} {
  const calls = { saved: [] as Settings[] };
  mockIPC((cmd, args) => {
    switch (cmd) {
      case "get_settings":
        return stored;
      case "get_launch_at_login":
        return false;
      case "save_settings": {
        const settings = (args as { settings: Settings }).settings;
        calls.saved.push(settings);
        return onSave(settings);
      }
    }
  });
  return calls;
}

describe("SettingsView", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("loads stored settings", async () => {
    mockBackend((settings) => settings);
    render(<SettingsView />);
    expect(await screen.findByDisplayValue("qwen3:8b")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  it("saves changes", async () => {
    const calls = mockBackend((settings) => settings);
    render(<SettingsView />);
    fireEvent.change(await screen.findByDisplayValue("qwen3:8b"), {
      target: { value: "gemma3:12b" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.saved).toHaveLength(1));
    expect(calls.saved[0]?.model).toBe("gemma3:12b");
  });

  it("shows the backend's message when saving fails", async () => {
    mockBackend(() => {
      throw { message: "Enter a model name." };
    });
    render(<SettingsView />);
    fireEvent.change(await screen.findByDisplayValue("qwen3:8b"), {
      target: { value: "" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Enter a model name.",
    );
  });
});
