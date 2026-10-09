# Architecture

Luna is a single Tauri 2 process plus a bundled llama.cpp server that runs only while a model is loaded. Rust owns all logic; React renders state and forwards user input through typed commands.

## Modules

| Module                           | Location                                           |
| -------------------------------- | -------------------------------------------------- |
| Desktop UI                       | `src/` (React, TypeScript, Vite)                   |
| Lifecycle and tray               | `src-tauri/src/lifecycle.rs`                       |
| Commands (frontend boundary)     | `src-tauri/src/commands.rs`                        |
| Settings and persistence         | `src-tauri/src/settings.rs`, `src-tauri/src/db.rs` |
| Conversation history             | `src-tauri/src/history.rs`                         |
| Credentials                      | `src-tauri/src/credentials.rs`                     |
| Home Assistant integration       | `src-tauri/src/home_assistant/`                    |
| Model catalogue and downloads    | `src-tauri/src/models/`, `src-tauri/models.json`   |
| Local inference (llama.cpp)      | `src-tauri/src/inference/`                         |
| Prompting and tool calling       | `src-tauri/src/assistant/`                         |
| Command validation and execution | `src-tauri/src/actions/`                           |

Voice modules (audio capture, wake word, speech-to-text) are created in Stage 3.

## Request flow

```
text input ─▶ context builder (floors, areas, relevant entities) ─▶ llama.cpp (tools, constrained JSON)
           ─▶ tool call ─▶ resolver (floors, areas, exclusions) ─▶ validator (existence, features,
              values, protection, confirmation) ─▶ executor (Home Assistant WebSocket)
           ─▶ verification against state_changed events ─▶ concise reply and result lines
```

The model only sees two tools, `get_states` and `control`. It never names services. Rust maps each validated action to a fixed Home Assistant service, removes exclusions before anything runs, leaves out locks, alarms, and doors unless named directly, and asks for confirmation before unlocking or opening them. Success is reported only for states Home Assistant confirms.

## Local inference

**Approach: llama.cpp `llama-server` as a bundled sidecar process.** Luna starts it on `127.0.0.1` with a random port and a per-launch API key, loads one GGUF model, sends OpenAI-compatible chat requests, and stops the process to unload.

Why a sidecar rather than a Rust binding:

- llama.cpp's server implements chat templates, tool-call parsing, and grammar-constrained tool arguments for both supported model families. Bindings expose only the core C API, so Luna would have to reimplement those per model.
- Unloading is exact: stopping the process returns all model memory to the OS.
- A crash in native inference code cannot take Luna down.
- Packaging is one self-contained executable per platform, signed by Tauri as an external binary.

Runtime details:

- **Version:** llama.cpp tag `b11517`, commit `8a1a9b5126126e5228b95fa909d4b08fac65e8b3`, built from source by `scripts/build-llama-server.sh`, which verifies the commit before building. Static libraries, no network features, no web UI.
- **macOS (Apple Silicon):** Metal with the shader library embedded. All layers run on the GPU.
- **Windows x64:** CPU backend for AVX2 processors (Intel 2013 and later, AMD 2015 and later), statically linked C runtime, no OpenMP dependency.
- **One model at a time.** Loads are serialised; switching stops the previous server before starting the next. The model loads on first use and unloads after 5 minutes idle, so Luna holds no model memory while idle.
- **Process lifetime.** Quitting unloads the model. On Windows the server is in a job object that the OS kills with Luna. On macOS SIGTERM triggers a clean quit, and any server left by a crash is stopped on the next launch (process ID recorded, name checked before stopping).
- **Cancellation.** Dropping the HTTP request makes the server cancel generation. Execution of validated actions is never cancelled midway.
- **Thinking.** Qwen3 receives `enable_thinking: false` through the chat template. Any reasoning text that still appears is stripped.

Optional GPU acceleration on Windows (Vulkan or CUDA) was not implemented. It would add one runtime per backend, larger installers, and driver-dependent failures. It should be evaluated separately with measurements on real hardware.

## Models

Weights are never bundled. Users download them in Settings; nothing downloads without a click.

| Model       | File                         | Source (pinned revision)               | Size    | SHA-256     | Licence            |
| ----------- | ---------------------------- | -------------------------------------- | ------- | ----------- | ------------------ |
| Qwen3 8B    | `Qwen3-8B-Q4_K_M.gguf`       | `Qwen/Qwen3-8B-GGUF@7c41481`           | 5.03 GB | `d98cdcbd…` | Apache-2.0         |
| Gemma 3 12B | `gemma-3-12b-it-Q4_K_M.gguf` | `ggml-org/gemma-3-12b-it-GGUF@ec0cbab` | 7.30 GB | `7bb69bff…` | Gemma Terms of Use |

Full hashes and URLs are in `src-tauri/models.json`. Qwen3 comes from the Qwen team's official repository. Google's own Gemma GGUF repositories require signing in to accept the licence, so Gemma comes from `ggml-org`, the llama.cpp maintainers. It is ungated and published under the same Gemma Terms of Use. Luna does not redistribute Gemma; users download it directly and are bound by Google's terms and prohibited use policy.

Downloads go to `<app data>/models/downloads/*.part`, resume with HTTP range requests, check free disk space first, and are verified with SHA-256 before moving into place. A `.verified` manifest records the checksum; only a model with a matching manifest and size counts as installed. Partial files never appear as installed models.

Memory: Luna estimates `file size + KV cache × context + 768 MB` and warns when that exceeds currently available memory. The default context is 4,096 tokens. Gemma 3 12B carries a standing warning about 16 GB devices.

## Key decisions

- **Typed boundary.** Rust types are exported to `src/bindings` with `ts-rs` (dev dependency only). CI fails if they are stale.
- **Errors.** Commands return `CommandError { message }`. The message is written for users; the technical error is logged in Rust.
- **Persistence.** SQLite through `rusqlite`. Schema migrations use `PRAGMA user_version`. Settings are a single JSON row so new fields get defaults without migrations. Corrupt data is reported, never silently reset. Model files live in their own directory.
- **Credentials.** The Home Assistant token lives in the OS credential store (Keychain, Windows Credential Manager). It never enters SQLite, logs, prompts, or the frontend.
- **Background behaviour.** Closing the window hides it. Quit is in the tray menu. Launch at login passes `--hidden` so Luna starts in the tray.
- **Model configuration.** Everything model-specific (sampling, thinking, memory estimates) lives in the catalogue. Adding a GGUF model means adding a catalogue entry.

## Compatibility risks

- **Windows CPU speed.** An 8B model on CPU answers in seconds to tens of seconds depending on the processor. Measure on real hardware before promising response times.
- **Processors without AVX2.** The Windows runtime will not start on them. Luna reports that the model could not load.
- **macOS microphone in the background.** Requires `NSMicrophoneUsageDescription` and a signed app for a stable TCC permission.
- **Windows microphone privacy.** Desktop apps can be blocked by "Let desktop apps access your microphone". Luna must detect and explain this.
- **Device changes.** CPAL does not emit device-change events on all hosts.
- **Tray behaviour differs.** macOS shows the menu on click; Windows opens the window on left click and the menu on right click.
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

Luna is Apache-2.0. Direct dependencies:

| Component                                                   | Licence                      | Bundled                    |
| ----------------------------------------------------------- | ---------------------------- | -------------------------- |
| Tauri, plugins (autostart, log, single-instance)            | Apache-2.0 / MIT             | Yes                        |
| React, Vite, TypeScript                                     | MIT / Apache-2.0             | Yes (React)                |
| llama.cpp (`llama-server`, ggml)                            | MIT                          | Yes                        |
| rusqlite, SQLite                                            | MIT, public domain           | Yes                        |
| tokio, tokio-util, futures-util, reqwest, tokio-tungstenite | MIT / Apache-2.0             | Yes                        |
| rustls, ring, rustls-platform-verifier                      | Apache-2.0 / MIT / ISC       | Yes                        |
| keyring, mdns-sd, sysinfo, fs4, sha2, windows-sys           | MIT / Apache-2.0             | Yes                        |
| serde, thiserror, url, log                                  | MIT / Apache-2.0             | Yes                        |
| ts-rs, wiremock                                             | MIT / Apache-2.0             | No (tests only)            |
| Qwen3 8B weights                                            | Apache-2.0                   | No, downloaded by the user |
| Gemma 3 12B weights                                         | Gemma Terms of Use (not OSI) | No, downloaded by the user |
| Planned: CPAL, whisper.cpp, sherpa-onnx, ONNX Runtime       | Apache-2.0, MIT              | Stage 3                    |
