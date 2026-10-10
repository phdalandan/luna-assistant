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
import { mockBackend, settings } from "../test/backend";
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

  it("shows the connection as a badge beside the heading", async () => {
    mockBackend();
    render(<SettingsView />);
    const badge = await screen.findByText("Connected");
    expect(badge.getAttribute("data-tone")).toBe("success");
    expect(badge.closest("legend")?.textContent).toContain("Home Assistant");
    expect(screen.getByLabelText("Address")).toBeTruthy();
  });

  it("chooses a voice and saves it with the form", async () => {
    const calls = mockBackend({
      save_settings: (args) => (args as { settings: unknown }).settings,
    });
    render(<SettingsView />);
    const voice = await screen.findByLabelText("Voice");
    await screen.findByRole("option", { name: "George (UK)" });
    fireEvent.change(voice, { target: { value: "bm_george" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(
        calls.find((call) => call.cmd === "save_settings")?.args,
      ).toMatchObject({ settings: { voice: "bm_george" } }),
    );
  });

  it("switches to cloud immediately and shows the cloud settings", async () => {
    const calls = mockBackend();
    render(<SettingsView />);
    expect(await screen.findByText("Qwen3 8B")).toBeTruthy();
    const ai = within(screen.getByRole("radiogroup", { name: "AI location" }));
    const local = ai.getByRole("radio", { name: "Local" });
    expect(local.getAttribute("aria-checked")).toBe("true");

    fireEvent.click(ai.getByRole("radio", { name: "Cloud" }));

    expect(
      calls.find((call) => call.cmd === "set_inference_mode")?.args,
    ).toEqual({ mode: "cloud" });
    expect(screen.queryByText("Qwen3 8B")).toBeNull();
    expect(screen.queryByLabelText("Context length")).toBeNull();
    expect(
      screen.getByText(
        "Commands and relevant home data are sent to your selected provider.",
      ),
    ).toBeTruthy();
    expect(
      await screen.findByRole("option", { name: "GPT-6 Luna" }),
    ).toBeTruthy();
    expect(
      screen.queryByRole("option", { name: "Claude Haiku 5.5" }),
    ).toBeNull();
    expect(screen.getByRole("button", { name: "Save" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  it("saves an API key for the selected provider without showing it again", async () => {
    const calls = mockBackend({
      get_settings: () => ({ ...settings, inference: "cloud" }),
      save_settings: (args) => (args as { settings: unknown }).settings,
    });
    render(<SettingsView />);
    fireEvent.change(await screen.findByLabelText("Provider"), {
      target: { value: "anthropic" },
    });
    await screen.findByRole("option", { name: "Claude Sonnet 5" });
    fireEvent.change(screen.getByLabelText("Model"), {
      target: { value: "claude-sonnet-5" },
    });
    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "secret-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(
        calls.find((call) => call.cmd === "save_settings")?.args,
      ).toMatchObject({
        apiKeys: { anthropic: "secret-key" },
        settings: {
          cloudProvider: "anthropic",
          anthropicModel: "claude-sonnet-5",
        },
      }),
    );
    const key = screen.getByLabelText("API key") as HTMLInputElement;
    await waitFor(() => expect(key.value).toBe(""));
    expect(key.type).toBe("password");
    expect(key.placeholder).toBe("Saved");
  });

  it("sends speech to OpenAI only after switching speech recognition to cloud", async () => {
    const calls = mockBackend({
      save_settings: (args) => (args as { settings: unknown }).settings,
    });
    render(<SettingsView />);
    const speech = within(
      await screen.findByRole("radiogroup", { name: "Speech recognition" }),
    );
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();

    fireEvent.click(speech.getByRole("radio", { name: "Cloud" }));

    expect(
      calls.find((call) => call.cmd === "set_speech_recognition")?.args,
    ).toEqual({ mode: "cloud" });
    expect(calls.some((call) => call.cmd === "set_inference_mode")).toBe(false);
    expect(
      screen.getByText("What you say after the wake word is sent to OpenAI."),
    ).toBeTruthy();
    fireEvent.change(screen.getByLabelText("OpenAI API key"), {
      target: { value: "openai-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(
        calls.find((call) => call.cmd === "save_settings")?.args,
      ).toMatchObject({ apiKeys: { openai: "openai-key" } }),
    );
  });

  it("removes a saved API key", async () => {
    const calls = mockBackend({
      get_settings: () => ({ ...settings, inference: "cloud" }),
      saved_api_keys: () => ["openai"],
    });
    render(<SettingsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove" }));
    expect(calls.find((call) => call.cmd === "remove_api_key")?.args).toEqual({
      provider: "openai",
    });
    await waitFor(() =>
      expect(
        (screen.getByLabelText("API key") as HTMLInputElement).placeholder,
      ).toBe(""),
    );
  });
});
