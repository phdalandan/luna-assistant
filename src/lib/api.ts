import { invoke } from "@tauri-apps/api/core";
import type { CommandError } from "../bindings/CommandError";
import type { Settings } from "../bindings/Settings";

export type { Settings };

export const api = {
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) =>
    invoke<Settings>("save_settings", { settings }),
  getLaunchAtLogin: () => invoke<boolean>("get_launch_at_login"),
  setLaunchAtLogin: (enabled: boolean) =>
    invoke<boolean>("set_launch_at_login", { enabled }),
};

const UNKNOWN_ERROR = "Something went wrong. Try again.";

/** Backend errors carry a user-facing message; anything else stays generic. */
export function errorMessage(error: unknown): string {
  if (isCommandError(error)) {
    return error.message;
  }
  console.error(error);
  return UNKNOWN_ERROR;
}

function isCommandError(error: unknown): error is CommandError {
  return (
    typeof error === "object" &&
    error !== null &&
    !(error instanceof Error) &&
    "message" in error &&
    typeof error.message === "string"
  );
}
