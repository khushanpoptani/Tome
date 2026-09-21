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
```

Run `pnpm format` to apply repository formatting.

## Distribution builds

Build the Apple Silicon DMG on macOS:

```sh
pnpm build:client
```

The artifact is written below `target/aarch64-apple-darwin/release/bundle/dmg/`.

Build the Windows server executable on 64-bit Windows:

```powershell
pnpm build:server:windows
```

The artifact is written to `target\x86_64-pc-windows-msvc\release\tome-server.exe`.

CI runs both native distribution builds and uploads the unsigned artifacts for inspection. Release signing and publication are intentionally deferred.
