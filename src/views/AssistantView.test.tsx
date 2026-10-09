import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Interaction } from "../lib/api";
import { mockBackend, model } from "../test/backend";
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
});
