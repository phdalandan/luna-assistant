# Architecture

Luna is a single Tauri 2 process. Rust owns all logic; React renders state and forwards user input through typed commands.

## Modules

| Module                           | Location                                           | Status  |
| -------------------------------- | -------------------------------------------------- | ------- |
| Desktop UI                       | `src/` (React, TypeScript, Vite)                   | Stage 1 |
| Lifecycle and tray               | `src-tauri/src/lifecycle.rs`                       | Stage 1 |
| Commands (frontend boundary)     | `src-tauri/src/commands.rs`                        | Stage 1 |
| Settings and persistence         | `src-tauri/src/settings.rs`, `src-tauri/src/db.rs` | Stage 1 |
| Errors                           | `src-tauri/src/error.rs`                           | Stage 1 |
| Home Assistant integration       | `src-tauri/src/home_assistant/`                    | Stage 2 |
| Local AI and tool calling        | `src-tauri/src/assistant/`                         | Stage 2 |
| Command validation and execution | `src-tauri/src/actions/`                           | Stage 2 |
| Audio capture and wake word      | `src-tauri/src/audio/`                             | Stage 3 |
| Speech-to-text                   | `src-tauri/src/speech/`                            | Stage 3 |

Modules are created when they have real code, not in advance.

## Request flow (Stages 2 and 3)

```
wake word ─▶ speech capture + VAD ─▶ whisper.cpp ─▶ transcript
text input ───────────────────────────────────────▶ transcript
transcript ─▶ context builder (relevant entities only) ─▶ Ollama (tools + JSON schema)
           ─▶ proposed actions ─▶ resolver (areas, floors, exclusions)
           ─▶ validator (existence, capabilities, params, permissions, safety)
           ─▶ confirmation if sensitive ─▶ executor (HA WebSocket) ─▶ state verification
           ─▶ concise response
```

Discovery, interpretation, resolution, validation, execution, and verification are separate functions so each can be tested with mocked Home Assistant data.

## Key decisions

- **Typed boundary.** Rust types are exported to `src/bindings` with `ts-rs` (dev dependency only). CI fails if they are stale.
- **Errors.** Commands return `CommandError { message }`. The message is written for users; the technical error is logged in Rust.
- **Persistence.** SQLite through `rusqlite` with the bundled SQLite build. Schema migrations use `PRAGMA user_version`. Settings are a single JSON row so new fields get defaults without migrations. Corrupt data is reported, never silently reset.
- **Credentials.** Home Assistant tokens will use the OS credential store (Keychain, Windows Credential Manager) in Stage 2. They never enter SQLite, logs, or prompts.
- **Background behaviour.** Closing the window hides it. Quit is in the tray menu. Launch at login passes `--hidden` so Luna starts in the tray. A second launch focuses the running instance.
- **LLM lifecycle.** Ollama is external. Luna only calls it on demand and can request `keep_alive` limits so the model unloads when idle.
- **Model settings.** Endpoint, model, and context length live in `Settings`. Model-specific request options (for example disabling Qwen3 thinking) will live in one place in the assistant module.

## Compatibility risks

- **macOS microphone in the background.** Requires `NSMicrophoneUsageDescription` and a signed app for a stable TCC permission. Unsigned builds may re-prompt after every update.
- **Windows microphone privacy.** Desktop apps can be blocked by "Let desktop apps access your microphone". Luna must detect and explain this.
- **Device changes.** CPAL does not emit device-change events on all hosts. Disconnection surfaces as a stream error, which must trigger re-selection.
- **whisper.cpp builds.** Needs CMake and a C++ toolchain in CI. Metal on macOS, CPU (AVX2) on Windows.
- **Tray behaviour differs.** macOS shows the menu on click; Windows opens the window on left click and the menu on right click.
- **Qwen3 thinking mode.** Ollama exposes `think: false`; older Ollama versions ignore it. Luna should require a minimum Ollama version rather than parse thinking output.
- **Linux.** Not a target. Linux builds are used only for local development checks.

## Wake-word evaluation

Requirements: offline, Windows and macOS, low CPU and memory, licence compatible with Apache-2.0 distribution, no account or key, fixed phrase without custom training.

| Engine                       | Code licence   | Model licence                     | Notes                                                                                                                                                                            |
| ---------------------------- | -------------- | --------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Porcupine (Picovoice)        | Apache-2.0 SDK | Proprietary                       | Requires an AccessKey and account. Rejected.                                                                                                                                     |
| openWakeWord                 | Apache-2.0     | Pretrained models CC BY-NC-SA 4.0 | Non-commercial models conflict with open distribution. Custom phrases need training. Rejected for bundled models.                                                                |
| microWakeWord                | Apache-2.0     | Apache-2.0                        | Fixed phrases ("okay nabu", "hey jarvis", "hey mycroft", "alexa"). Built for TFLite Micro on ESP32; desktop use needs a TFLite runtime in Rust.                                  |
| sherpa-onnx keyword spotting | Apache-2.0     | Apache-2.0 (k2-fsa KWS models)    | Small streaming zipformer, ONNX Runtime, C API with Rust bindings. Open-vocabulary keywords, so a fixed phrase like "hey luna" needs no training, but accuracy must be measured. |
| Rustpotter                   | Apache-2.0     | User-created                      | Pure Rust, very light. Accuracy relies on recorded samples; maintenance is slow.                                                                                                 |
| Snowboy, Mycroft Precise     | Apache-2.0     | Varies                            | Unmaintained. Rejected.                                                                                                                                                          |

**Recommendation:** sherpa-onnx keyword spotting with one fixed phrase, chosen after a Stage 3 benchmark of CPU usage, memory, false accepts per hour, and miss rate on both platforms. microWakeWord with "hey jarvis" is the alternative if accuracy is insufficient; switching requires approval under the fallback policy. Silero VAD (MIT) runs only after the wake word fires.

## Licence audit

Luna is Apache-2.0. Direct and planned dependencies:

| Component                                        | Licence                      | Bundled                               |
| ------------------------------------------------ | ---------------------------- | ------------------------------------- |
| Tauri, plugins (autostart, log, single-instance) | Apache-2.0 / MIT             | Yes                                   |
| React, Vite, TypeScript                          | MIT / Apache-2.0             | Yes (React)                           |
| rusqlite, SQLite                                 | MIT, public domain           | Yes                                   |
| serde, thiserror, url, log                       | Apache-2.0 / MIT             | Yes                                   |
| ts-rs                                            | MIT                          | No (dev only)                         |
| CPAL                                             | Apache-2.0                   | Stage 3                               |
| whisper.cpp                                      | MIT                          | Stage 3                               |
| Whisper model weights                            | MIT                          | Downloaded by the user, not bundled   |
| sherpa-onnx, ONNX Runtime                        | Apache-2.0, MIT              | Stage 3                               |
| Ollama                                           | MIT                          | External runtime                      |
| Qwen3                                            | Apache-2.0                   | Downloaded through Ollama             |
| Gemma 3                                          | Gemma Terms of Use (not OSI) | Downloaded through Ollama by the user |

Model weights are never distributed with Luna, so model licences apply to the user's own download.
