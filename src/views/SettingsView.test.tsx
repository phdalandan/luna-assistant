import { clearMocks } from "@tauri-apps/api/mocks";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { mockBackend } from "../test/backend";
import { SettingsView } from "./SettingsView";

describe("SettingsView", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("loads settings, status, and models", async () => {
    mockBackend();
    render(<SettingsView />);
    expect(
      await screen.findByDisplayValue("http://homeassistant.local:8123"),
    ).toBeTruthy();
    expect(await screen.findByText("Connected")).toBeTruthy();
    expect(await screen.findByText("Qwen3 8B")).toBeTruthy();
    expect(screen.getByPlaceholderText("Saved")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  it("saves a new access token without showing the saved one", async () => {
    const calls = mockBackend({
      save_settings: (args) => (args as { settings: unknown }).settings,
    });
    render(<SettingsView />);
    fireEvent.change(await screen.findByLabelText("Access token"), {
      target: { value: "new-token" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(calls.some((call) => call.cmd === "save_settings")).toBe(true),
    );
    const save = calls.find((call) => call.cmd === "save_settings");
    expect((save?.args as { token: string }).token).toBe("new-token");
    await waitFor(() =>
      expect(
        (screen.getByLabelText("Access token") as HTMLInputElement).value,
      ).toBe(""),
    );
  });

  it("suggests discovered instances when no address is set", async () => {
    mockBackend({
      get_settings: () => ({
        homeAssistantUrl: "",
        activeModel: null,
        contextLength: 4096,
      }),
      discover_home_assistant: () => [
        { name: "Home", url: "http://10.0.0.2:8123" },
      ],
    });
    render(<SettingsView />);
    fireEvent.click(await screen.findByRole("button", { name: /Home/ }));
    expect(screen.getByDisplayValue("http://10.0.0.2:8123")).toBeTruthy();
  });

  it("shows the backend's message when saving fails", async () => {
    mockBackend({
      save_settings: () => {
        throw {
          message:
            "Enter a Home Assistant address like http://homeassistant.local:8123.",
        };
      },
    });
    render(<SettingsView />);
    fireEvent.change(
      await screen.findByDisplayValue("http://homeassistant.local:8123"),
      {
        target: { value: "nope" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Enter a Home Assistant address like http://homeassistant.local:8123.",
    );
  });

  it("stays usable when the credential store is unavailable", async () => {
    mockBackend({
      has_home_assistant_token: () => {
        throw {
          message: "Luna couldn't access your saved access token. Try again.",
        };
      },
    });
    render(<SettingsView />);
    expect(
      await screen.findByDisplayValue("http://homeassistant.local:8123"),
    ).toBeTruthy();
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Luna couldn't access your saved access token. Try again.",
    );
  });

  it("downloads the speech models only when asked", async () => {
    const calls = mockBackend();
    render(<SettingsView />);
    const voice = await screen.findByLabelText("Speech models");
    expect(voice.textContent).toContain("78 MB");
    expect(calls.some((call) => call.cmd === "download_voice_models")).toBe(
      false,
    );
    fireEvent.click(within(voice).getByRole("button", { name: "Download" }));
    expect(calls.some((call) => call.cmd === "download_voice_models")).toBe(
      true,
    );
  });

  it("saves a new wake word with the form", async () => {
    const calls = mockBackend({
      save_settings: (args) => (args as { settings: unknown }).settings,
    });
    render(<SettingsView />);
    fireEvent.change(await screen.findByLabelText("Wake word"), {
      target: { value: "Jarvis" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(
        calls.find((call) => call.cmd === "save_settings")?.args,
      ).toMatchObject({ settings: { wakeWord: "Jarvis" } }),
    );
  });
});
