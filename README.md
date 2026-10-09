# Luna

Luna is an open-source, privacy-first voice assistant for [Home Assistant](https://www.home-assistant.io/). It runs entirely on your computer: speech recognition, wake word detection, and language understanding never leave your machine.

Say "It's too bright in the living room" or "Turn off everything downstairs except the hallway light" and Luna works out which devices you mean, checks the action is allowed, carries it out, and confirms the result.

> **Status:** early development. Stage 1 (desktop shell, tray, settings) is in place. Home Assistant control, the AI pipeline, and voice are not implemented yet.

## Requirements

- Windows 10/11 (x64) or macOS 11+ (Apple Silicon)
- 16 GB of memory
- [Ollama](https://ollama.com) with a local model, for example `ollama pull qwen3:8b`
- A Home Assistant instance on your network

## Install

Download the latest installer from [Releases](../../releases) or from a workflow run's artifacts.

Development builds are unsigned:

- **macOS:** Gatekeeper blocks unsigned, unnotarised apps. Right-click Luna in Applications and choose **Open**, or run `xattr -dr com.apple.quarantine /Applications/Luna.app`.
- **Windows:** SmartScreen shows "Windows protected your PC". Choose **More info**, then **Run anyway**.

## Configuration

Open Luna from the tray or menu bar and go to **Settings**:

- **Home Assistant address**, for example `http://homeassistant.local:8123`
- **Ollama address**, default `http://127.0.0.1:11434`
- **Model**, any model installed in Ollama, for example `qwen3:8b` or `gemma3:12b`
- **Context length**
- **Launch at login**

Settings are stored locally in SQLite in the app data directory. Access tokens will be stored in the operating system's credential store.

Closing the window keeps Luna running in the tray. Use **Quit Luna** from the tray menu to exit.

## Development

Prerequisites: Node.js 22, Rust (stable), and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```sh
npm ci
npm run tauri dev
```

Checks:

```sh
npm run format:check && npm run lint && npm run typecheck && npm test
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`cargo test` also regenerates the TypeScript types in `src/bindings`.

## Build

Local installer for the current platform:

```sh
npm run tauri build
```

CI builds run only when started manually: **Actions > Build > Run workflow**. Enter a version (`MAJOR.MINOR.PATCH`) and optionally enable **Create a draft GitHub Release**. Artifacts are named `Luna-<version>-macos-arm64.dmg` and `Luna-<version>-windows-x64-setup.exe`.

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

[Apache-2.0](LICENSE)
