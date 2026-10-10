import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { CommandError } from "../bindings/CommandError";
import type { DiscoveredInstance } from "../bindings/DiscoveredInstance";
import type { Interaction } from "../bindings/Interaction";
import type { ModelInfo } from "../bindings/ModelInfo";
import type { Settings } from "../bindings/Settings";
import type { Status } from "../bindings/Status";
import type { VoiceModelsInfo } from "../bindings/VoiceModelsInfo";
import type { VoiceState } from "../bindings/VoiceState";
import type { VoiceStatus } from "../bindings/VoiceStatus";

export type {
  DiscoveredInstance,
  Interaction,
  ModelInfo,
  Settings,
  Status,
  VoiceModelsInfo,
  VoiceState,
  VoiceStatus,
};

export interface DownloadProgress {
  id: string;
  downloaded: number;
}

export const api = {
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings, token: string | null) =>
    invoke<Settings>("save_settings", { settings, token }),
  hasHomeAssistantToken: () => invoke<boolean>("has_home_assistant_token"),
  discoverHomeAssistant: () =>
    invoke<DiscoveredInstance[]>("discover_home_assistant"),
  getStatus: () => invoke<Status>("get_status"),
  listModels: () => invoke<ModelInfo[]>("list_models"),
  downloadModel: (id: string) => invoke<null>("download_model", { id }),
  pauseDownload: (id: string) => invoke<null>("pause_download", { id }),
  cancelDownload: (id: string) => invoke<null>("cancel_download", { id }),
  deleteModel: (id: string) => invoke<null>("delete_model", { id }),
  selectModel: (id: string) => invoke<null>("select_model", { id }),
  prepareAssistant: () => invoke<null>("prepare_assistant"),
  ask: (text: string) => invoke<Interaction>("ask", { text }),
  cancelRequest: () => invoke<null>("cancel_request"),
  confirmAction: (id: number, confirmed: boolean) =>
    invoke<Interaction>("confirm_action", { id, confirmed }),
  listInteractions: () => invoke<Interaction[]>("list_interactions"),
  clearHistory: () => invoke<null>("clear_history"),
  getLaunchAtLogin: () => invoke<boolean>("get_launch_at_login"),
  setLaunchAtLogin: (enabled: boolean) =>
    invoke<boolean>("set_launch_at_login", { enabled }),
  setListening: (enabled: boolean) =>
    invoke<null>("set_listening", { enabled }),
  getVoiceModels: () => invoke<VoiceModelsInfo>("get_voice_models"),
  downloadVoiceModels: () => invoke<null>("download_voice_models"),
  cancelVoiceDownload: () => invoke<null>("cancel_voice_download"),
};

export const events = {
  onStatus: (handler: (status: Status) => void) =>
    listen<Status>("status-changed", (event) => handler(event.payload)),
  onConversationCleared: (handler: () => void) =>
    listen("conversation-cleared", () => handler()),
  onInteractionsChanged: (handler: () => void) =>
    listen("interactions-changed", () => handler()),
  onModelsChanged: (handler: () => void) =>
    listen("models-changed", () => handler()),
  onDownloadProgress: (handler: (progress: DownloadProgress) => void) =>
    listen<DownloadProgress>("download-progress", (event) =>
      handler(event.payload),
    ),
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
