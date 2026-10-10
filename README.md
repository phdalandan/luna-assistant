# Luna

Luna is an open-source, privacy-first voice assistant for [Home Assistant](https://www.home-assistant.io/). By default it runs entirely on your computer: speech recognition, wake word detection, and language understanding never leave your machine. You can choose a cloud model from OpenAI or Anthropic instead.

Say "It's too bright in the living room" or "Turn off everything downstairs except the hallway light" and Luna works out which devices you mean, checks the action is allowed, carries it out, and confirms the result.

> **Status:** early development. The text assistant works with Home Assistant and a built-in local AI engine. Voice is not implemented yet.

## Requirements

- Windows 10/11 (x64, AVX2 processor) or macOS 11+ (Apple Silicon)
- 16 GB of memory
- About 6 GB of free disk space for the recommended model
- Home Assistant 2024.4 or later on your network

Nothing else needs to be installed. Luna includes its own AI engine ([llama.cpp](https://github.com/ggml-org/llama.cpp)).

## Install

Download the latest installer from [Releases](../../releases) or from a workflow run's artifacts.

Development builds are unsigned:

- **macOS:** Gatekeeper blocks unsigned, unnotarised apps. Right-click Luna in Applications and choose **Open**, or run `xattr -dr com.apple.quarantine /Applications/Luna.app`.
- **Windows:** SmartScreen shows "Windows protected your PC". Choose **More info**, then **Run anyway**.

## First run

1. Open Luna from the tray or menu bar.
2. Download the recommended model (Qwen3 8B, 5.0 GB). Nothing downloads until you choose to.
3. Select **Use** once it is installed.
4. In **Settings**, pick your Home Assistant instance (found automatically on your network) or enter its address, then paste a [long-lived access token](https://www.home-assistant.io/docs/authentication/#your-account-profile).

## AI models

| Model       | Download | Notes                                                 |
| ----------- | -------- | ----------------------------------------------------- |
| Qwen3 8B    | 5.0 GB   | Recommended for everyday commands.                    |
| Gemma 3 12B | 7.3 GB   | Uses more memory and may run slowly on 16 GB devices. |

Both use Q4_K_M quantisation and download from Hugging Face. Files are verified with SHA-256 and stored in Luna's app data folder (`~/Library/Application Support/io.github.phdalandan.luna/models` on macOS, `%APPDATA%\io.github.phdalandan.luna\models` on Windows). Downloads can be paused and resumed. Gemma 3 is subject to the [Gemma Terms of Use](https://ai.google.dev/gemma/terms).

The model loads when you first ask something and unloads after 5 minutes of inactivity. Context length defaults to 4,096 tokens and can be raised in Settings.

## Privacy

With Local selected (the default), requests, Home Assistant data, and AI processing stay on your computer, and Luna connects to the internet only to download a model you chose. With Cloud selected, the text of a request and the home data needed to interpret it (room and floor names, relevant devices and their states, and the last few turns of the conversation) are sent to the provider you chose. Luna still controls Home Assistant itself; the provider never connects to it. Audio stays on your computer unless you also choose Cloud for speech recognition: then what you say after the wake word is sent to OpenAI for transcription. The wake word is always detected locally. The Home Assistant token and API keys are stored in the macOS Keychain or Windows Credential Manager.

Closing the window keeps Luna running in the tray. Use **Quit Luna** from the tray menu to exit.

## Development

Prerequisites: Node.js 22, Rust (stable), CMake, and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```sh
npm ci
npm run tauri dev
```

The first run builds the bundled llama.cpp server (a few minutes), downloads the pinned sherpa-onnx libraries, and builds the voice helper. Each is rebuilt only when its pinned version or source changes. On Windows this needs Git Bash on the `PATH`.

Checks:

```sh
npm run format:check && npm run lint && npm run typecheck && npm test
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`cargo test` also regenerates the TypeScript types in `src/bindings`. Tests that need a real model are ignored by default:

```sh
LUNA_TEST_MODEL=/path/to/Qwen3-8B-Q4_K_M.gguf cargo test -- --ignored
```

## Build

Local installer for the current platform:

```sh
npm run tauri build
```

CI builds run only when started manually: **Actions > Build > Run workflow**. Each run builds the pinned llama.cpp server for its platform (cached between runs) and bundles it into the installer. No model weights are bundled. Enter a version (`MAJOR.MINOR.PATCH`) and optionally enable **Create a draft GitHub Release**. Artifacts are named `Luna-<version>-macos-arm64.dmg` and `Luna-<version>-windows-x64-setup.exe`.

Release creation makes a draft release targeting the exact commit that was built. Review it and publish it manually. Existing releases and tags are never overwritten.

### Code signing

Signing is optional and enabled when these repository secrets exist:

| Platform           | Secrets                                                                                   |
| ------------------ | ----------------------------------------------------------------------------------------- |
| macOS signing      | `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` |
| macOS notarisation | `APPLE_ID`, `APPLE_PASSWORD` (app-specific password), `APPLE_TEAM_ID`                     |
| Windows            | `WINDOWS_CERTIFICATE` (base64 .pfx), `WINDOWS_CERTIFICATE_PASSWORD`                       |

Without them, builds are unsigned. Windows certificates issued after June 2023 are stored on hardware tokens or cloud HSMs and cannot be exported as .pfx; those need a custom `signCommand` (for example Azure Trusted Signing).

## Architecture

See [docs/architecture.md](docs/architecture.md) for modules, design decisions, the wake word evaluation, and the dependency licence audit.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CLAUDE.md](CLAUDE.md). Security issues: [SECURITY.md](SECURITY.md).

## Licence

[Apache-2.0](LICENSE), except the voice helper in `voice-helper/`, which is [GPL-3.0-or-later](voice-helper/LICENSE) because it links espeak-ng. It is built and shipped as a separate program.
