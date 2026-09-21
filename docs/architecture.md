# Architecture decisions

## Stage 0 scope

Stage 0 establishes reproducible development and CI foundations. It intentionally does not implement chat storage, attachments, networking, model execution, job processing, authentication, or product UI.

## Approved stack

- **Desktop client:** Tauri 2 with a React and TypeScript frontend.
- **Server:** a native Rust executable. Axum is the selected HTTP framework for the stage that introduces server APIs; it is not installed before it is needed.
- **Shared implementation language:** Rust for native client capabilities, server behavior, and future shared domain types.
- **Server persistence:** SQLite is the selected store for persistent server jobs once jobs are implemented.
- **Model integration:** future model execution will sit behind an adapter compatible with local llama.cpp/Ollama-style runtimes. Stage 0 does not select or bundle a model runtime.
- **Desktop-owned data:** the client remains the authority for local JSON chat data and compressed attachments. Formats and migration rules are deferred to the storage stage.

## Initial distribution targets

| Component      | Target                                       | Artifact |
| -------------- | -------------------------------------------- | -------- |
| Desktop client | Apple Silicon macOS (`aarch64-apple-darwin`) | `.dmg`   |
| Server         | 64-bit Windows (`x86_64-pc-windows-msvc`)    | `.exe`   |

The Windows server must be built on Windows with the MSVC toolchain. The DMG must be built on macOS. Code signing, notarization, installers, update delivery, and minimum supported OS versions are release decisions and are not part of Stage 0.

The checked-in application icon is a development placeholder required for native bundling, not a final brand decision.

## Repository layout

```text
apps/
  client/             React frontend and Tauri shell
    src-tauri/        Native macOS client crate
  server/             Windows server executable crate
docs/                 Architecture and development documentation
.github/workflows/    Continuous integration
```
