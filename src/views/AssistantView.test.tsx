import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Interaction } from "../lib/api";
import { mockBackend, model, status } from "../test/backend";
import { AssistantView } from "./AssistantView";

const active = () => [model({ installed: true, active: true })];

function interaction(overrides: Partial<Interaction> = {}): Interaction {
  return {
    id: 1,
    createdAt: Date.now(),
    request: "Turn off the kitchen light",
    response: "The kitchen light is off.",
    results: ["Kitchen Light is off."],
    awaitingConfirmation: false,
    ...overrides,
  };
}

describe("AssistantView", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("asks for a model download on first run", async () => {
    mockBackend();
    render(<AssistantView />);
    expect(
      await screen.findByText("Download an AI model to get started."),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Download" })).toBeTruthy();
    expect(screen.queryByLabelText("Message")).toBeNull();
  });

  it("asks to choose a model when one is installed but none is active", async () => {
    mockBackend({ list_models: () => [model({ installed: true })] });
    render(<AssistantView />);
    expect(
      await screen.findByText("Choose an AI model to get started."),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Use" })).toBeTruthy();
  });

  it("uses the cloud provider without a local model", async () => {
    const calls = mockBackend({
      get_status: () => ({ ...status, inference: "cloud" }),
      ask: () => interaction(),
    });
    render(<AssistantView />);
    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "Turn off the kitchen light" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(await screen.findByText("The kitchen light is off.")).toBeTruthy();
    expect(
      screen.queryByText("Download an AI model to get started."),
    ).toBeNull();
    expect(calls.some((call) => call.cmd === "prepare_assistant")).toBe(false);
  });

  it("sends requests and shows responses with verified results", async () => {
    mockBackend({ list_models: active, ask: () => interaction() });
    render(<AssistantView />);
    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "Turn off the kitchen light" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(await screen.findByText("The kitchen light is off.")).toBeTruthy();
    expect(screen.getByText("Kitchen Light is off.")).toBeTruthy();
  });

  it("prepares the model when shown and when focused again", async () => {
    const calls = mockBackend({ list_models: active });
    render(<AssistantView />);
    await screen.findByLabelText("Message");
    const prepared = () =>
      calls.filter((call) => call.cmd === "prepare_assistant").length;
    expect(prepared()).toBe(1);
    fireEvent.focus(window);
    expect(prepared()).toBe(2);
  });

  it("asks for confirmation before sensitive actions", async () => {
    const pending = interaction({
      request: "Unlock the front door",
      response: "Unlock Front Door?",
      results: [],
      awaitingConfirmation: true,
    });
    const calls = mockBackend({
      list_models: active,
      list_interactions: () => [pending],
      confirm_action: () => ({
        ...pending,
        response: "Front Door is unlocked.",
        awaitingConfirmation: false,
      }),
    });
    render(<AssistantView />);
    fireEvent.click(await screen.findByRole("button", { name: "Confirm" }));
    expect(await screen.findByText("Front Door is unlocked.")).toBeTruthy();
    expect(calls.find((call) => call.cmd === "confirm_action")?.args).toEqual({
      id: 1,
      confirmed: true,
    });
    expect(screen.queryByRole("button", { name: "Confirm" })).toBeNull();
  });

  it("shows friendly errors", async () => {
    mockBackend({
      list_models: active,
      ask: () => {
        throw { message: "Unable to load this model." };
      },
    });
    render(<AssistantView />);
    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "hi" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Unable to load this model.",
    );
  });

  it("turns listening on and off from the microphone control", async () => {
    const calls = mockBackend({
      list_models: active,
      get_status: () => ({
        ...status,
        voice: { state: "listening", problem: null },
      }),
    });
    render(<AssistantView />);
    const control = await screen.findByRole("button", { name: "Listening" });
    expect(control.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(control);
    expect(calls.find((call) => call.cmd === "set_listening")?.args).toEqual({
      enabled: false,
    });
  });

  it("explains why listening stopped", async () => {
    mockBackend({
      list_models: active,
      get_status: () => ({
        ...status,
        voice: {
          state: "off",
          problem: "The microphone was disconnected.",
        },
      }),
    });
    render(<AssistantView />);
    expect(
      await screen.findByText("The microphone was disconnected."),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Microphone off" })).toBeTruthy();
  });

  it("counts down to when the conversation resets", async () => {
    mockBackend({
      list_models: active,
      get_status: () => ({
        ...status,
        conversationEndsAt: Date.now() + 90_500,
      }),
    });
    render(<AssistantView />);
    expect(await screen.findByText("Conversation resets in 1:31")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Luna" })).toBeNull();
  });

  it("leaves listening failures to the voice status instead of repeating them", async () => {
    mockBackend({
      list_models: active,
      set_listening: () => {
        throw { message: "Download the voice models in Settings first." };
      },
    });
    render(<AssistantView />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Microphone off" }),
    );
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
