# Contributing

Thanks for helping with Luna.

## Before you start

- Read [CLAUDE.md](CLAUDE.md). Its engineering, copywriting, and fallback rules apply to every contribution.
- Read [docs/architecture.md](docs/architecture.md).
- For anything beyond a small fix, open an issue first so the approach can be agreed.

## Workflow

1. Fork and create a branch.
2. Make a focused change. Avoid unrelated refactors.
3. Add or update tests for behaviour you change.
4. Run all checks:

   ```sh
   npm run format:check && npm run lint && npm run typecheck && npm test
   cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
   ```

5. Commit regenerated `src/bindings` files if `cargo test` changed them.
6. Open a pull request describing what changed and how you tested it, including which platforms.

## Guidelines

- Business logic belongs in Rust. React handles presentation.
- Do not add dependencies without a concrete need. Mention the licence in your pull request.
- Never commit secrets, Home Assistant tokens or configuration, recordings, or model weights.
- User-facing text must follow the copywriting rules. No em dashes.

By contributing, you agree that your contributions are licensed under the Apache-2.0 licence.
