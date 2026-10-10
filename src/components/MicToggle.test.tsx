import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { mockBackend, status } from "../test/backend";
import { MicToggle } from "./MicToggle";

describe("MicToggle", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("turns listening off when it is on", async () => {
    const calls = mockBackend({
      get_status: () => ({
        ...status,
        voice: { state: "listening", problem: null },
      }),
    });
    render(<MicToggle />);
    const control = await screen.findByRole("switch", { name: "Listening" });
    expect(control.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(control);
    expect(calls.find((call) => call.cmd === "set_listening")?.args).toEqual({
      enabled: false,
    });
  });

  it("leaves listening failures to the voice status instead of repeating them", async () => {
    const calls = mockBackend({
      set_listening: () => {
        throw { message: "Download the voice models in Settings first." };
      },
    });
    render(<MicToggle />);
    const control = await screen.findByRole("switch", {
      name: "Microphone off",
    });
    expect(control.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(control);
    expect(calls.find((call) => call.cmd === "set_listening")?.args).toEqual({
      enabled: true,
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
