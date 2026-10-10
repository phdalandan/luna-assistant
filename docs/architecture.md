# Architecture

Luna is a single Tauri 2 process plus a bundled llama.cpp server that runs only while a model is loaded. Rust owns all logic; React renders state and forwards user input through typed commands.

## Modules

| Module                             | Location                                           |
| ---------------------------------- | -------------------------------------------------- |
| Desktop UI                         | `src/` (React, TypeScript, Vite)                   |
| Lifecycle and tray                 | `src-tauri/src/lifecycle.rs`                       |
| Commands (frontend boundary)       | `src-tauri/src/commands.rs`                        |
| Settings and persistence           | `src-tauri/src/settings.rs`, `src-tauri/src/db.rs` |
| Conversation history               | `src-tauri/src/history.rs`                         |
| Credentials                        | `src-tauri/src/credentials.rs`                     |
| Home Assistant integration         | `src-tauri/src/home_assistant/`                    |
| Model catalogue and downloads      | `src-tauri/src/models/`, `src-tauri/models.json`   |
| Local inference (llama.cpp)        | `src-tauri/src/inference/`                         |
| Prompting and tool calling         | `src-tauri/src/assistant/`                         |
| Command validation and execution   | `src-tauri/src/actions/`                           |
| Voice (capture, wake word, speech) | `src-tauri/src/voice/`                             |
| Voice and app integration          | `src-tauri/src/listening.rs`                       |

## Request flow

```
text input ─▶ router (Rust) ─┬─▶ recognised and unambiguous: time, undo, "are you sure", on/off/open/close/lock,
                             │   state questions ─▶ validator ─▶ executor ─▶ verified reply (no model)
                             └─▶ otherwise ─▶ llama.cpp (cached instructions, layout, tools + request context)
                                 ─▶ tool calls ─▶ resolver ─▶ validator ─▶ executor ─▶ verified reply
```

- **Router** (`assistant/route.rs`) handles a small grammar of commands, questions, and brightness or temperature settings. It acts only when the words resolve to exactly one entity, or to every matching device in one area for plurals like "kitchen lights". "What about the AC?" or "and the kitchen?" repeats the previous directly answered question for the new subject. A device word alone ("the AC") means every device of that kind: one is acted on, two to four get "Which one: …?", more go to the model. Anything broad or with exceptions goes to the model. Filler words ("sorry", "back", "again", "for me") are ignored unless a device is named with them. A statement about a state ("it's on I think") rechecks Home Assistant and changes nothing. "Check X" and "X status" read any state. An answer to "Which one?" ("first one", "both", a name) completes the original request. A bare device phrase ("the hallway too", "and the kitchen") or a bare value ("actually 22") repeats the previous direct request for it. Temperature questions read climate devices and temperature sensors. The phrasings that must never reach the model are listed in one test, `everyday_requests_are_answered_without_the_model`; a new phrasing that should be instant is added there first.
- **Model** sees two tools, `get_states` and `control`, and never names services. When it only calls `control`, the reply is built from the verified results without a second pass. `get_states` needs a second pass to answer; a read matching more than 25 entities returns counts by type and asks the model to narrow it, never a partial list. Calls that act on the same entity in conflicting ways are rejected before anything runs. Exclusions are rejected unless the request names an exception. Areas and floors may be named by ID or by a name or alias that fits exactly one, so "Front Porch" does not cost a second pass. Tool definitions are kept short (about 1,140 prefix tokens with instructions) because they are evaluated whenever the prompt cache is cold.
- **Prompt layout.** The system message holds the instructions and the floor and area layout, followed by the tools. It only changes when Home Assistant's registries change, so llama.cpp reuses its cache. Per-request facts go in the user message: recently referenced entities with live states, up to 15 relevant entities, states before the last action (only when the request refers back, at most 10), and the Home Assistant time when asked. When a request names a kind of device, room words in entity names do not pull in other kinds. The tool schema keeps its declared property order (`serde_json` `preserve_order`) because llama.cpp's grammar enforces it.
- **Conversation memory** (`assistant/session.rs`) is kept in Rust for 2 minutes after the last request, then the conversation and its history are cleared. The window counts down to the reset. Every launch starts with an empty conversation. Memory holds the entities the last turn referred to with what Luna reported, and states from before the last action. "Revert that" restores those states through the validator, so unlocking or opening still asks for confirmation. Remembered states are never presented as current; replies always read the live cache, and "are you sure" fetches every state from Home Assistant again.
- **Time** comes from a Home Assistant clock sensor such as Time & Date's `sensor.time`, never from the computer. Hidden entities, timestamp sensors, and UTC clocks are ignored. If visible clocks disagree, Luna asks the user to hide the extra ones.
- **Cache.** One WebSocket connection loads the registries and all states once, then `state_changed` events keep states current. Registry events reload only the four registries, alongside event handling, and rebuild metadata over the cached states; a registry event during a reload queues one more reload.
- **Execution.** Service calls for one plan run concurrently. Verification waits on `state_changed` events and stops as soon as every device is confirmed or reports it is moving.
- **Metrics.** Each request logs its route, model passes, prompt and cached tokens, prompt and generation time, service call time, and verification time. Every model tool call and every rejection reason is logged.
- **Reply wording** (`assistant/phrasing.rs`) is built in Rust from verified results, never by another model pass. Acknowledgements rotate in a fixed order kept in conversation memory ("Done.", "You got it.", "Turned off.", "All set."), so wording varies without randomness. A single device the user named, or called "it", is not named again. More than three devices get a count ("Done. All 6 devices are off."). Partial failures always name what did not work. Yes-or-no questions answer from the live state ("Nope, it's closed."). Security-sensitive results after confirmation always name the device.
- **Honest replies.** If the model's action is rejected because the device cannot do it, the reply says so even when another action succeeded. The out-of-scope reply is never used after a tool call. The window warm-up runs only when the model is not already loaded, so it never evicts a conversation in progress.

## Voice

Voice is off until the user turns on listening, which needs the speech models (downloaded in Settings) and an AI model. The state is a Rust state machine (`voice/conversation.rs`), independent of the UI: `Passive`, `WakeDetected`, `CapturingCommand`, `Processing`, `Responding`, `AwaitingFollowUp`.

```
microphone (CPAL, mono) ─▶ resample to 16 kHz ─▶ 5 s rolling buffer ─▶ wake word (sherpa-onnx KWS, "Luna")
  on wake: load VAD + whisper ─▶ replay the buffer into the VAD ─▶ capture until 0.7 s of silence
  ─▶ whisper.cpp transcript ─▶ addressing check ─▶ same request path as typed text ─▶ spoken reply (OS voice)
  ─▶ follow-ups without the name for 20 s (8 s of silence ends it) ─▶ passive: VAD, whisper, and audio released
```

- **Passive.** Only the wake word spotter runs. Audio lives in a fixed 5 second ring buffer that overwrites itself; nothing is transcribed, logged, or stored. Speech detection and whisper are not loaded.
- **Wake word.** "Luna" by default; the user can choose any name of one to three words (letters and apostrophes) in Settings, and listening restarts with it. The keyword model is open-vocabulary, so no training is needed: `voice/keyword.rs` spells the name in the model's word pieces with the model's own sentencepiece unigram scores (`bpe.model`), matching the official tokenizer on every name tested. The addressing check uses the same name and accepts a one-letter mishearing for names of four or more letters ("Lunar" for "Luna"); shorter names must match exactly. Short or common words trigger more often.
- **Wake word anywhere.** The spotter is streaming and fires wherever the name is said. The ring buffer keeps the speech before it, so "Turn off the lights, Luna" is captured whole. Speech separated from the wake word by more than a second of silence is left out.
- **Addressing** (`voice/address.rs`). The transcript must use the name to address Luna: at the start ("Luna, …", "Hey Luna …"), at the end ("…, Luna?"), or set off by commas ("Could you, Luna, …"). "I saw Luna at the park" is ignored. The name is removed before the request is handled. When the name sits between two sentences, the one that reads as a home request is used. The name alone gets "Yes?" and waits for the request.
- **Follow-ups** (`assistant/relevance.rs`). During the conversation window, speech without the name is handled only if it is clearly for Luna: a recognised request, an action or question about "it", or a device, room, or floor in this home. Everything else is dropped without logging its text and never extends the window. "Thanks" is answered and ends the conversation; "never mind" ends it silently. "Yes" and "no" answer a pending confirmation. No model pass is used to classify speech.
- **Context.** Spoken requests use the same conversation memory as typed ones (referenced devices, last action, pending "Which one?"), so "make it 50%" and "revert that" work by voice. The voice window (20 s) is separate from that memory (2 minutes).
- **Timing.** Buffer 5 s, conversation window 20 s, silence timeout 8 s, false-wake check 1.5 s, longest request 15 s. Defaults are in `voice::Timing` and should be tuned with real use.
- **Half duplex.** While Luna transcribes, thinks, or speaks, microphone audio is dropped at the source, so she never hears herself and audio never queues up.
- **Stopping.** Turning listening off (window or tray) closes the microphone immediately, cancels any request so its reply is never spoken, and waits for the voice thread. A disconnected or denied microphone stops listening and explains why. Quitting stops voice first.
- **Speech output** uses the operating system's voice through the `tts` crate (WinRT on Windows, AVFoundation on macOS). It must report when it finishes speaking; otherwise listening does not start.

### Speech models

All three are downloaded together in Settings, verified against the catalogue SHA-256, and never bundled. The wake word archive is checksummed, then only the five files the catalogue names (model, tokens, and vocabulary) are extracted.

| Model                  | Source                                                     | Size    | Licence    |
| ---------------------- | ---------------------------------------------------------- | ------- | ---------- |
| KWS zipformer 3.3M     | k2-fsa sherpa-onnx `kws-models` release (gigaspeech, int8) | 17.6 MB | Apache-2.0 |
| Silero VAD             | k2-fsa sherpa-onnx `asr-models` release                    | 0.6 MB  | MIT        |
| Whisper base.en (q5_1) | `ggerganov/whisper.cpp@5359861`                            | 59.7 MB | MIT        |

The k2-fsa models are published only as GitHub release assets, so the catalogue also trusts `github.com/k2-fsa/sherpa-onnx/releases/download/`. Checksums apply as for every model.

### Native libraries

- **sherpa-onnx 1.13.8** (wake word, VAD, ONNX Runtime) is linked statically from the official `no-tts` release archives. The full archives include espeak-ng (GPL-3.0) for sherpa's own speech synthesis, which Luna does not use, and the `sherpa-onnx-sys` build script links them unconditionally and downloads them unverified. `.cargo/config.toml` replaces that build script's output (`links = "sherpa-onnx"`), and `scripts/fetch-sherpa-onnx.mjs` (part of `npm run runtime`) downloads and checks the pinned archive. Windows uses the `MD` (dynamic C runtime) build.
- **whisper.cpp 1.8.3** is compiled in-process by `whisper-rs-sys` through CMake, with bindings generated by bindgen, which needs libclang (LLVM) on the build machine. Portable AVX2 code on x86, Metal on Apple Silicon. The `cmake` crate drops optimisation from release flags with the Visual Studio generator, so `.cargo/config.toml` sets them. Unoptimised, a transcription took 17 s instead of 1.3 s.

### Measurements

Windows 11, AMD Ryzen 9 6900HS (8 cores, 16 threads), release build, synthetic voices (Windows David and Zira, 16 kHz). Not yet measured on macOS.

| What                                   | Result                                                                                                   |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| Passive listening, real microphone     | 4.5% of one core over 60 s; 97 MB resident for the voice thread, microphone, wake word, and voice output |
| Wake word spotting alone               | 33 ms of one core per second of audio; model +24 MB                                                      |
| Wake word, 17 recordings with "Luna"   | Detected in every one: name at the start, middle, end, or alone                                          |
| Wake word, 9 recordings without it     | No detections ("lunar eclipse", "tuna", "lunch", a 10 s paragraph)                                       |
| Mention ("I saw Luna at the park")     | Wake word fires; the addressing check ignores it                                                         |
| Whisper base.en q5_1                   | Loaded in 150 ms, +66 MB, released when the conversation ends; 1.2 to 1.5 s per utterance (2 to 11 s)    |
| Stop while processing, microphone loss | Voice thread finished 45 to 48 ms later; the cancelled reply was not spoken                              |

From the end of a request, Luna waits 0.7 s of silence, then transcribes (about 1.3 s) before handling it. End-to-end latency with the app and Home Assistant has not been measured.

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
- **One model at a time.** Loads are serialised; switching stops the previous server before starting the next. The model loads when Luna's window is shown or focused (and evaluates the shared prompt prefix), or on first use, and unloads after 5 minutes idle, so Luna holds no model memory while idle.
- **Process lifetime.** Quitting unloads the model. On Windows the server is in a job object that the OS kills with Luna. On macOS SIGTERM triggers a clean quit, and any server left by a crash is stopped on the next launch (process ID recorded, name checked before stopping).
- **Cancellation.** Dropping the HTTP request makes the server cancel generation. Execution of validated actions is never cancelled midway.
- **Saved prompt.** The server runs with `--slot-save-path` in `<app data>/prompt-cache`. After the shared prompt (instructions, layout, tools) is first evaluated, its KV cache (about 180 MB) is saved under a SHA-256 of the model, context length, system message, and tools. The next load restores it instead of evaluating it again (measured on an M2: ready in 1.1 s instead of 10.3 s). Only the current file is kept. A missing or unusable file means the prompt is evaluated as usual.
- **Thinking.** Qwen3 receives `enable_thinking: false` through the chat template, which prefills an empty think block. Verified with raw output (`--reasoning-format none`): 23 generated tokens with thinking off versus 395 with it on for the same question. Any reasoning text that still appears is stripped.
- **Output cap.** Replies are limited to 256 tokens; tool calls and one-sentence answers need far fewer.

Optional GPU acceleration on Windows (Vulkan or CUDA) was not implemented. It would add one runtime per backend, larger installers, and driver-dependent failures. It should be evaluated separately with measurements on real hardware.

## Models

Weights are never bundled. Users download them in Settings; nothing downloads without a click.

| Model       | File                         | Source (pinned revision)               | Size    | SHA-256     | Licence            |
| ----------- | ---------------------------- | -------------------------------------- | ------- | ----------- | ------------------ |
| Qwen3 8B    | `Qwen3-8B-Q4_K_M.gguf`       | `Qwen/Qwen3-8B-GGUF@7c41481`           | 5.03 GB | `d98cdcbd…` | Apache-2.0         |
| Qwen3 4B    | `Qwen3-4B-Q4_K_M.gguf`       | `Qwen/Qwen3-4B-GGUF@bc64014`           | 2.50 GB | `7485fe6f…` | Apache-2.0         |
| Gemma 3 12B | `gemma-3-12b-it-Q4_K_M.gguf` | `ggml-org/gemma-3-12b-it-GGUF@ec0cbab` | 7.30 GB | `7bb69bff…` | Gemma Terms of Use |

Full hashes and URLs are in `src-tauri/models.json`. Both Qwen3 models come from the Qwen team's official repositories. The 4B model is for slower CPUs: about half the memory and compute of the 8B. Google's own Gemma GGUF repositories require signing in to accept the licence, so Gemma comes from `ggml-org`, the llama.cpp maintainers. It is ungated and published under the same Gemma Terms of Use. Luna does not redistribute Gemma; users download it directly and are bound by Google's terms and prohibited use policy.

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
- **macOS microphone in the background.** `Info.plist` provides `NSMicrophoneUsageDescription`. A signed app is needed for a stable TCC permission. Untested.
- **Windows microphone privacy.** Desktop apps can be blocked by "Let desktop apps access your microphone". Luna explains this when CPAL reports access denied; whether Windows reports denial or delivers silence has not been tested.
- **macOS voice.** The Metal whisper build, the AVFoundation voice, and its completion callback have not been built or run on macOS.
- **Wake word accuracy.** Measured only with synthetic voices. False accepts per hour and miss rate with real voices, accents, and background noise are unknown. "Luna" is a short keyword, and custom wake words were checked only with "Jarvis"; the score and threshold in the catalogue may need tuning.
- **Build machines** need CMake and libclang (LLVM) for whisper.cpp. GitHub's Windows and macOS runners include both.
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

**Chosen:** sherpa-onnx keyword spotting with the name alone ("Luna" by default, or the user's choice), so it can be said anywhere in a sentence. Measured results are in Voice. microWakeWord remains the alternative if accuracy is insufficient with real voices; switching requires approval under the fallback policy. Silero VAD (MIT) runs only after the wake word fires.

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
| CPAL, tts, windows (WinRT speech)                           | Apache-2.0 / MIT             | Yes                        |
| sherpa-onnx (no-tts), ONNX Runtime, kaldi-native-fbank      | Apache-2.0 / MIT             | Yes                        |
| whisper.cpp, ggml / whisper-rs                              | MIT / Unlicense              | Yes                        |
| tar, bzip2 (libbz2-rs-sys)                                  | MIT / Apache-2.0             | Yes                        |
| ts-rs, wiremock                                             | MIT / Apache-2.0             | No (tests only)            |
| Qwen3 8B weights                                            | Apache-2.0                   | No, downloaded by the user |
| Gemma 3 12B weights                                         | Gemma Terms of Use (not OSI) | No, downloaded by the user |
| KWS zipformer, Silero VAD, Whisper base.en weights          | Apache-2.0, MIT, MIT         | No, downloaded by the user |
