# Contributing

Tome is developed in separately reviewed stages. Keep each change within the active stage and do not combine later product work with development-environment changes.

## Before opening a pull request

1. Install the pinned toolchains and dependencies from `docs/development.md`.
2. Run `pnpm format`.
3. Run `pnpm check` on macOS for the complete local workspace check.
4. Build the platform artifact when changing platform-specific configuration.
5. Describe validation results and known limitations in the pull request.

Pull requests require human review. Authors must not merge their own stage pull requests.
