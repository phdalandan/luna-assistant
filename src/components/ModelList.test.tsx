import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { mockBackend, model } from "../test/backend";
import { ModelList } from "./ModelList";

describe("ModelList", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("offers a download with size and quantisation", () => {
    const calls = mockBackend();
    render(<ModelList models={[model()]} engine={{ state: "idle" }} />);
    expect(screen.getByText("Recommended · 5.0 GB · Q4_K_M")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Download" }));
    expect(calls.at(-1)).toEqual({
      cmd: "download_model",
      args: { id: "qwen3-8b" },
    });
  });

  it("shows progress with pause and cancel while downloading", () => {
    mockBackend();
    const downloading = model({
      download: {
        phase: "downloading",
        downloaded: 1_000_000_000,
        error: null,
      },
    });
    render(<ModelList models={[downloading]} engine={{ state: "idle" }} />);
    expect(screen.getByText("1.0 GB of 5.0 GB")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pause" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeTruthy();
  });

  it("explains failed downloads and allows an explicit retry", () => {
    mockBackend();
    const failed = model({
      download: {
        phase: "failed",
        downloaded: 0,
        error: "Not enough disk space.",
      },
    });
    render(<ModelList models={[failed]} engine={{ state: "idle" }} />);
    expect(screen.getByText("Not enough disk space.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
  });

  it("lets installed models be used or deleted, but not the active one", () => {
    mockBackend();
    const models = [
      model({ installed: true, active: true }),
      model({
        id: "gemma-3-12b",
        name: "Gemma 3 12B",
        recommended: false,
        installed: true,
        warning: "Uses more memory and may run slowly on 16GB devices.",
      }),
    ];
    render(
      <ModelList
        models={models}
        engine={{ state: "ready", model: "qwen3-8b" }}
      />,
    );
    expect(screen.getByText("Active ✓")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Delete" })).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Use" })).toBeTruthy();
    expect(
      screen.getByText("Uses more memory and may run slowly on 16GB devices."),
    ).toBeTruthy();
  });

  it("shows a loading state while the active model loads", () => {
    mockBackend();
    render(
      <ModelList
        models={[model({ installed: true, active: true })]}
        engine={{ state: "loading", model: "qwen3-8b" }}
      />,
    );
    expect(screen.getByText("Loading")).toBeTruthy();
  });

  it("shows backend errors", async () => {
    mockBackend({
      delete_model: () => {
        throw { message: "Switch to another model before deleting this one." };
      },
    });
    render(
      <ModelList
        models={[model({ installed: true })]}
        engine={{ state: "idle" }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Switch to another model before deleting this one.",
    );
  });
});
