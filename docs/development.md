# Development

## Pinned toolchains

- Node.js 24.21.0 (the project also accepts Node 26 for local compatibility)
- pnpm 12.5.1
- Rust 1.97.1 with `rustfmt` and `clippy`
- Tauri CLI 2.11.5 and Tauri Rust crate 2.11.6

The lockfiles are authoritative for transitive dependencies.

## Prerequisites

Install Node.js using `.node-version`, then install the pinned package manager:

```sh
npm install --global pnpm@12.5.1
```

Install Rust through rustup. The checked-in `rust-toolchain.toml` selects the correct toolchain and components automatically.

For macOS client development, install Xcode and its command-line tools. For the Windows server artifact, use 64-bit Windows with Visual Studio Build Tools and the Desktop development with C++ workload.

## Setup

```sh
pnpm install --frozen-lockfile
cargo fetch --locked
cp .env.example .env
```

The example environment file contains no secrets. Keep local secrets out of Git.

## Everyday commands

```sh
pnpm dev:client        # run the macOS client in development mode
pnpm check             # formatting, linting, tests, and type checking
pnpm build:web         # build only the web frontend
pnpm build:server      # build the server for the current host
pnpm build:server-dashboard:web # build the Windows dashboard frontend
```

Run `pnpm format` to apply repository formatting.

## Running the Phase 1 server

The default configuration is intentionally loopback-only:

```sh
cargo run -p tome-server
```

To listen on a private LAN interface, choose the interface address explicitly rather than using a wildcard:

```sh
TOME_NETWORK_MODE=lan TOME_BIND_ADDRESS=192.168.1.20 cargo run -p tome-server
```

For Tailscale, use the machine's `100.64.0.0/10` address (or Tailscale IPv6 address) and set `TOME_NETWORK_MODE=tailscale`. See [Phase 1 server and jobs](phase-1-server.md) for all configuration, safety constraints, and API details.

## Phase 2 model runtime

Tome detects `llama-server` on `PATH`. Set `TOME_LLAMA_SERVER_PATH` to an explicit reviewed executable when it is installed elsewhere. The executable is probed with a three-second timeout and is only used for verified GGUF models. Model files, resumable partials, and recoverable deletions live beside the configured database under `models/`, `model-downloads/`, and `model-trash/`. See [Phase 2 model management](phase-2-model-management.md).

## Distribution builds

Build the Apple Silicon DMG on macOS:

```sh
pnpm build:client
```

The artifact is written below `target/aarch64-apple-darwin/release/bundle/dmg/`.

Build the Windows server NSIS installer on 64-bit Windows:

```powershell
pnpm build:server:windows
```

The primary artifact is written below `target\x86_64-pc-windows-msvc\release\bundle\nsis\` as `*-setup.exe`. The raw `tome-server.exe` console binary is an internal developer artifact, not the Windows product.

CI runs both native distribution builds, validates the NSIS package and Windows GUI subsystem, and uploads the unsigned installer for inspection. Release signing and publication are intentionally deferred. Installation, firewall, first-run, and uninstall behavior are documented in [Windows server installation](windows-server.md).
