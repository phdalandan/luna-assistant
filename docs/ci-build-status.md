# CI build status

State of `.github/workflows/build.yml` as of 2026-10-10. The workflow runs only on `workflow_dispatch`, and every run costs GitHub minutes. Verify locally whatever you can before asking for a run.

## Runs so far

| Run | Result                                                                   | Cause                                                                      |
| --- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------- |
| #1  | Cancelled                                                                | Not investigated.                                                          |
| #2  | Failed on both platforms at "Lint Rust"                                  | Clippy 1.99 `chunks_exact_to_as_chunks` in `src-tauri/src/voice/speak.rs`. |
| #3  | Windows failed at "Check generated TypeScript bindings", macOS cancelled | All 18 files in `src/bindings` reported as modified on Windows.            |

## Fixed

- **Clippy (`5188777`).** Replaced `chunks_exact(4)` with `as_chunks::<4>()`. Run #3 then passed "Lint Rust" and "Test Rust" on Windows.
- **Bindings check (`c3b480d`).** Windows runners check out with `core.autocrlf=true`, so the files arrive with CRLF line endings. ts-rs then rewrites them with LF, and `git status` reports them as modified. The fix is `.gitattributes`: `src/bindings/** text eol=lf`.
  - Reproduced on macOS by cloning with `-c core.autocrlf=true` and copying in the LF bindings: every file showed as modified.
  - With `.gitattributes` in place, the same test showed nothing modified.
  - Not yet confirmed on a Windows runner.

## Not yet verified

These steps have never run in CI on either platform, because every run so far failed or was cancelled before reaching them:

- Import Windows signing certificate (only runs when the `WINDOWS_CERTIFICATE` secret is set)
- Build installer (`npx tauri build`)
- Collect installer
- Upload artifact
- Create draft release

Nothing in this list has been tested. Their behaviour on Windows can only be confirmed by a Windows run.

## Local checks before a run

Run these on macOS or Linux before asking for a CI run:

```sh
npm run format:check && npm run lint && npm run typecheck && npm test
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

CI uses the latest stable Rust (1.99 at the time of run #2). An older local toolchain can miss newer Clippy lints, so run `rustup update stable` first.

The macOS installer build can also be tested locally, which costs no CI minutes:

```sh
npx tauri build --target aarch64-apple-darwin --bundles dmg --config '{"version":"0.1.0"}'
```
