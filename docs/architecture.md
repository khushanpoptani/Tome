# Architecture decisions

## Stage 0 scope

Stage 0 establishes reproducible development and CI foundations. It intentionally does not implement chat storage, attachments, networking, model execution, job processing, authentication, or product UI.

## Phase 1 scope

Phase 1 adds the unauthenticated, single-user server connection and durable job foundation described in [Phase 1 server and jobs](phase-1-server.md). It deliberately does not add model management, inference, chat, attachments, OCR, transcription, or client-owned product storage. The registered job type names are protocol reservations, not implemented product behavior.

## Phase 2 scope

Phase 2 implements hardware/runtime discovery, a pinned model catalog, first-run setup, resumable verified model downloads, a durable model registry, a truthful runtime adapter boundary, and matching local/remote model-management UIs. `model_download` is now executed by the Phase 1 job engine. Model inference, prompts, chats, attachments, OCR execution, transcription execution, and resident-model scheduling remain out of scope. See [Phase 2 model management](phase-2-model-management.md).

## Approved stack

- **Desktop client:** Tauri 2 with a React and TypeScript frontend.
- **Server:** a Tauri 2 Windows desktop host and system tray wrapping a reusable Rust/Axum runtime for versioned HTTP and WebSocket APIs.
- **Shared implementation language:** Rust for native client capabilities, server behavior, and future shared domain types.
- **Server persistence:** SQLite stores durable jobs and their ordered, replayable event histories. Schema changes use explicit migrations.
- **Model integration:** the first production adapter detects an external `llama-server` executable and loads verified GGUF files on loopback. Tome does not simulate successful loads and does not bundle a runtime in Phase 2. Other catalog formats are explicitly catalog-only.
- **Desktop-owned data:** the client remains the authority for local JSON chat data and compressed attachments. Formats and migration rules are deferred to the storage stage.

## Initial distribution targets

| Component      | Target                                       | Artifact          |
| -------------- | -------------------------------------------- | ----------------- |
| Desktop client | Apple Silicon macOS (`aarch64-apple-darwin`) | `.dmg`            |
| Server         | 64-bit Windows (`x86_64-pc-windows-msvc`)    | NSIS `-setup.exe` |

The Windows server must be built on Windows with the MSVC toolchain. The DMG must be built on macOS. Windows uses a per-machine NSIS installer; code signing, notarization, update delivery, and minimum supported OS versions remain release decisions.

The checked-in application icon is a development placeholder required for native bundling, not a final brand decision.

## Repository layout

```text
apps/
  client/             React frontend and Tauri shell
    src-tauri/        Native macOS client crate
  server/             Reusable Axum service, job runtime, and developer CLI
  server-dashboard/   Windows Tauri host, dashboard, tray, and NSIS packaging
docs/                 Architecture and development documentation
.github/workflows/    Continuous integration
```
