import { afterEach, describe, expect, it, vi } from "vitest";
import { errorMessage } from "./api";

describe("errorMessage", () => {
  afterEach(() => vi.restoreAllMocks());

  it("returns the message from a backend error", () => {
    expect(errorMessage({ message: "Enter a model name." })).toBe(
      "Enter a model name.",
    );
  });

  it("never shows raw errors to the user", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    expect(errorMessage(new TypeError("x is undefined"))).not.toContain(
      "undefined",
    );
    expect(errorMessage("invalid args `settings` for command")).toBe(
      "Something went wrong. Try again.",
    );
  });
});
