# Luna engineering rules

Luna is a privacy-first, local voice assistant for Home Assistant. It is a background app with a tray or menu bar entry, not a smart home dashboard. Read `docs/architecture.md` before changing structure.

## Product boundaries

- Never build device pages, inventories, grids, cards, or per-device controls.
- Users never need entity IDs. Discovery and resolution happen internally.
- The LLM proposes structured actions only. Rust validates and executes them through a fixed set of tools. The model never calls arbitrary Home Assistant services, APIs, or system commands.
- Validate entity existence, supported actions, parameter types, permissions, exclusions, and safety before execution.
- Security-sensitive actions (locks, garage doors, alarms, and similar) require explicit confirmation.
- Ask for clarification when a request is ambiguous or unsafe. Never guess.
- Report success only after execution is verified. Never present proposed actions as completed.
- Never invent entity IDs or assume devices, floors, or rooms exist.
- Everything runs locally by default. Send requests, home data, or spoken audio to a cloud provider only when the user chose Cloud for that feature in Settings, and only what that feature needs. The wake word detector never leaves the device.
- Never expose credentials to the model. Store tokens only in the OS credential store. Never log them.
- Only the wake-word detector runs continuously. Speech recognition and inference start on demand. The LLM is not loaded while idle.
- Prefer event-driven code. No unnecessary polling.
- Never persist microphone recordings by default.
- Never commit secrets, personal Home Assistant data, recordings, or model weights.

## Copywriting rules

- NEVER use em dashes anywhere in the application's copy.
- Use succinct, natural language. Prefer one sentence whenever possible.
- Avoid unnecessary explanations.
- Do not add subtitles, helper text, descriptions, or tooltips unless they provide meaningful information.
- Never add text that merely restates a label. A field labelled "Device name" does not need helper text explaining that it is the device's name.
- Use clear, action-oriented language.
- Avoid excessive technical terminology in user-facing copy.
- Error messages must help users understand what happened and what they can do next.
- NEVER display raw exceptions, stack traces, internal error codes, or developer-oriented error messages to users.
- Detailed technical information belongs in structured application logs.
- Do not add confirmation messages for every minor interaction.
- Avoid marketing-style language inside the application.
- Do not overexplain straightforward functionality.
- Never display internal reasoning, raw tool calls, or technical statistics in the normal interface.

## Code quality rules

- Write clean, readable, maintainable code. Prefer explicit, straightforward implementations.
- Follow Rust and TypeScript best practices.
- Maintain clear separation of concerns. Use descriptive names. Keep functions focused.
- Avoid massive files, deeply nested logic, and overly complex components.
- Avoid duplicated business logic.
- Do not introduce unnecessary dependencies, frameworks, databases, or services.
- Do not create abstractions without a demonstrated need. Do not overengineer.
- Do not add speculative functionality.
- Remove dead code rather than leaving unused implementations.
- Prefer fixing the underlying problem over introducing workarounds.
- Do not suppress errors to make functionality appear successful. Do not silently ignore failures.
- Keep code formatting consistent (`cargo fmt`, Prettier).
- Write tests for critical application behaviour.
- Avoid unnecessary code comments. Code should be self-explanatory whenever possible.
- When comments are necessary, keep each comment to a maximum of 2 lines.
- Never write lengthy explanatory comments or comments that restate what the code makes obvious.
- Keep platform-specific code isolated behind clear interfaces.
- Avoid circular dependencies.
- Avoid global mutable state unless absolutely necessary.
- Prefer typed errors and explicit error handling.
- Do not introduce unnecessary architectural layers.
- Do not create generic utility functions for one-time operations without a clear benefit.

## Architecture rules

- Rust owns system operations, background services, audio, Home Assistant, LLM orchestration, validation, business logic, and persistence.
- React is for presentation and interaction only. Do not move logic into React because it is easier.
- Types shared with the frontend are defined in Rust and exported with `ts-rs` to `src/bindings`. Never hand-edit them.
- Commands return `CommandError { message }` with a user-facing message. Log the technical error in Rust.
- Local inference runs only through the bundled llama.cpp server in `src-tauri/src/inference/`; cloud inference only through the OpenAI and Anthropic providers in `src-tauri/src/inference/cloud/`. Never add another inference backend without approval.
- Model-specific behaviour lives only in the catalogue (`src-tauri/models.json`). Never hardcode it elsewhere.
- Never bundle model weights. Never download a model without an explicit user action. Never switch models automatically.
- Downloaded models must be verified against the catalogue SHA-256 before they count as installed.
- No microservices, RAG, vector databases, agent frameworks, or fine-tuning without a demonstrated requirement.

## Strict fallback policy

NEVER automatically introduce fallback implementations or alternative execution paths.

If the primary implementation has a problem:

1. Investigate the root cause.
2. Attempt to fix the intended implementation.
3. Explain the issue if a fallback appears necessary.
4. Request explicit approval before implementing any fallback.

Do not silently substitute another AI model, inference backend, audio engine, connection method, mock data, hardcoded results, or reduced functionality.

Normal error handling is allowed: retrying a temporarily disconnected Home Assistant connection and reporting unavailable services. Alternative implementations that mask an unresolved underlying issue require approval. Mocks belong in tests only.

## Engineering workflow

- Inspect existing code before changing it. Follow the established architecture.
- Make focused, reviewable changes. Explain significant architectural decisions briefly.
- Do not rewrite working components without a concrete reason.
- Do not add unrelated improvements while implementing a feature.
- Run relevant checks after changes (see below).
- Never claim something works without testing it. Never claim a platform-specific feature works unless it was tested on that platform.
- Document unresolved issues accurately.
- Ask before making significant architectural changes.
- Do not automatically implement optional enhancements or features beyond the approved scope.
- Keep communication succinct. Avoid lengthy summaries.

## CI rules

- `.github/workflows/build.yml` uses `workflow_dispatch` only. Never add other triggers without explicit approval.
- Never publish a release automatically. Releases are created as drafts.

## Checks

Build the bundled runtime first if it is missing: `npm run runtime`.

```sh
npm run format:check && npm run lint && npm run typecheck && npm test
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`cargo test` regenerates `src/bindings`. Commit any changes it produces.
