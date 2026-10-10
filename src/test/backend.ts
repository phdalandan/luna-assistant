import { mockIPC } from "@tauri-apps/api/mocks";
import type { InvokeArgs } from "@tauri-apps/api/core";
import type { ModelInfo, Settings, Status } from "../lib/api";

export const settings: Settings = {
  homeAssistantUrl: "http://homeassistant.local:8123",
  activeModel: null,
  contextLength: 4096,
  listening: false,
};

export const status: Status = {
  homeAssistant: "connected",
  engine: { state: "idle" },
  voice: { state: "off", problem: null },
};

export function model(overrides: Partial<ModelInfo> = {}): ModelInfo {
  return {
    id: "qwen3-8b",
    name: "Qwen3 8B",
    quantization: "Q4_K_M",
    size: 5_027_783_488,
    recommended: true,
    warning: null,
    memoryWarning: false,
    installed: false,
    active: false,
    download: null,
    ...overrides,
  };
}

export type Handler = (args: InvokeArgs | undefined) => unknown;

/** Mocks every backend command with sensible defaults and records invocations. */
export function mockBackend(overrides: Record<string, Handler> = {}) {
  const calls: { cmd: string; args: InvokeArgs | undefined }[] = [];
  const defaults: Record<string, Handler> = {
    get_settings: () => settings,
    get_launch_at_login: () => false,
    has_home_assistant_token: () => true,
    discover_home_assistant: () => [],
    get_status: () => status,
    list_models: () => [model()],
    list_interactions: () => [],
    get_voice_models: () => ({
      installed: false,
      size: 78_000_000,
      download: null,
    }),
    prepare_assistant: () => null,
    "plugin:event|listen": () => 1,
    "plugin:event|unlisten": () => undefined,
  };
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    const handler = overrides[cmd] ?? defaults[cmd];
    return handler?.(args);
  });
  return calls;
}
