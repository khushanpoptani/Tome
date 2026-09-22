# Phase 1 server, networking, and persistent jobs

Phase 1 provides a single-user server foundation for simultaneous private LAN and Tailscale use. The Windows product is a Tauri desktop host with a dashboard and system-tray lifecycle; the console binary remains a developer tool. It has no app-level login. Do not place it on the public internet, forward its port from a router, or expose it through a public tunnel.

## Installed Windows host

Install the NSIS package and launch **Tome Server** from the Start menu. First-run setup selects the port, private LAN and Tailscale access, Windows Firewall rules, the data directory, and optional launch at login. Administrator approval is required for the per-machine install and whenever firewall rules change.

The dashboard reports stopped, starting, running, and error states; every eligible loopback, private LAN, and Tailscale address; active listeners; firewall and Tailscale health; recent server events; and copyable diagnostics. Closing the window keeps the server in the system tray. **Quit and stop server** in the tray gracefully stops listeners and job dispatch before exiting.

Tome enumerates interfaces and binds a separate listener to each enabled, explicit trusted address. Loopback is always included. Private LAN and Tailscale can be enabled together. The host never binds `0.0.0.0`, `::`, or a detected public address. When adapters or addresses change, the dashboard asks for a restart rather than silently broadening access.

The installer creates inbound TCP rules scoped to the installed executable and default port: `LocalSubnet` on the Windows Private profile for LAN, and `100.64.0.0/10` for Tailscale IPv4. The dashboard recreates those rules for a changed port. Uninstall removes both rules and launch-at-login registration, then explicitly asks whether local settings, job history, and other local data should also be removed. See [Windows server installation](windows-server.md) for the operator workflow.

## Configuration and network safety

Configuration comes from environment variables. The server never reads a checked-in secret file and does not log request bodies or configuration secrets.

| Variable                  | Default        | Meaning                                            |
| ------------------------- | -------------- | -------------------------------------------------- |
| `TOME_NETWORK_MODE`       | `loopback`     | `loopback`, `lan`, or `tailscale`                  |
| `TOME_BIND_ADDRESS`       | `127.0.0.1`    | Exact local interface address to listen on         |
| `TOME_PORT`               | `7331`         | TCP port, 1–65535                                  |
| `TOME_DATABASE_PATH`      | `tome.sqlite3` | SQLite database path                               |
| `TOME_TEMP_DIRECTORY`     | `tome-temp`    | Directory inspected for orphan temporary artifacts |
| `TOME_JOB_RETENTION_DAYS` | `30`           | Terminal-job retention before startup cleanup      |
| `RUST_LOG`                | `info`         | Structured log filter                              |

The selected mode and bind address must agree:

- `loopback` accepts only loopback addresses.
- `lan` accepts loopback, private or link-local IPv4, and IPv6 unique-local addresses. IPv6 link-local addresses are omitted because a safe bind also requires an adapter scope identifier.
- `tailscale` accepts `100.64.0.0/10` and `fd7a:115c:a1e0::/48`.
- Wildcard and public addresses always fail validation.

Bind to the machine's exact LAN or Tailscale address. A firewall should restrict the port to the intended private network. The API permits browser access only from the packaged Tauri origins and the local Vite development origins; CORS is defense in depth, not authentication.

The environment variables above configure only the developer console host, which has one explicit listener. Startup logs identify the effective address, mode, protocol version, and the fact that authentication is absent. `Ctrl+C` and `SIGTERM` initiate graceful HTTP shutdown. Jobs left in `running` are durably interrupted after the dispatcher stops.

## Protocol and APIs

The API base is `/api/v1`; protocol version `1` is independent of the server package version. Clients send `X-Tome-Protocol-Version`. An unsupported value returns HTTP 426 with the accepted range. Omission remains allowed for manual diagnostics.

| Method and path                 | Purpose                                                    |
| ------------------------------- | ---------------------------------------------------------- |
| `GET /api/v1/health`            | Liveness and protocol version                              |
| `GET /api/v1/version`           | Server, API, and protocol versions                         |
| `GET /api/v1/capabilities`      | Network mode, feature flags, and registered job types      |
| `POST /api/v1/jobs`             | Create an idempotent queued job                            |
| `GET /api/v1/jobs`              | List jobs; optional `state` and `limit` query parameters   |
| `GET /api/v1/jobs/{id}`         | Retrieve a job                                             |
| `POST /api/v1/jobs/{id}/cancel` | Cancel a queued or running job                             |
| `POST /api/v1/jobs/{id}/retry`  | Create a linked retry of a terminal job                    |
| `POST /api/v1/jobs/cleanup`     | Remove terminal jobs finished before an RFC 3339 timestamp |
| `GET /api/v1/events`            | Replay events after `after_event_id`                       |
| `GET /api/v1/events/ws`         | WebSocket replay/live stream after `after_event_id`        |

Create body example:

```json
{
  "idempotency_key": "client-generated-stable-request-id",
  "job_type": "inference",
  "input": {},
  "parent_job_id": null
}
```

Submitting the same key and identical request returns the original job with `duplicate: true`. Reusing a key for different input, type, or linkage returns HTTP 409. Job IDs are UUIDv7 values. Retry jobs have their own idempotency key and retain `retry_of_job_id`; optional workflow relationships use `parent_job_id`.

Errors use one envelope, including validation, not-found, state conflict, idempotency conflict, and protocol mismatch responses:

```json
{
  "error": {
    "code": "invalid_request",
    "message": "human-readable explanation",
    "request_id": "0199...",
    "details": {}
  }
}
```

## Job state and persistence

The state machine is:

```text
queued ──► running ──► completed
  │           ├──────► failed
  │           ├──────► cancelled
  │           └──────► interrupted
  └──────────────────► cancelled
```

Terminal jobs are preserved across restarts. Queued jobs remain queued and can be claimed after restart. Startup atomically moves any `running` job to `interrupted`, preserves its last progress, and appends one interruption event. Repeating recovery is idempotent because only `running` rows qualify.

Every state write and its event commit in one SQLite transaction. Events have monotonic integer IDs and are retained with the job until explicit or configured cleanup. Progress is in the inclusive range 0–1. Retention cleanup removes terminal jobs and their events; it never removes queued/running jobs.

The server registers these future types: `inference`, `model_download`, `model_verification`, `model_load`, `model_unload`, `attachment_processing`, `ocr`, and `transcription`. All capability entries say `implemented: false`. The Phase 1 dispatcher claims them and records a `job_type_unimplemented` failure. No model or attachment work occurs.

At startup the server only discovers files prefixed `tome-job-` in its temporary directory. It emits `server.warning` and leaves them untouched. Ownership validation and deletion belong to the attachment phase.

## Events and reconnect

The durable event vocabulary is `job.created`, `job.queued`, `job.started`, `job.progress`, `job.completed`, `job.failed`, `job.cancelled`, `job.interrupted`, and `server.warning`.

Connect to `/api/v1/events/ws?after_event_id=N`. The server subscribes to live delivery before reading the database, replays every retained event greater than `N`, and then sends live events. It filters duplicates by event ID. If a receiver lags, it fills the gap from SQLite. Clients persist the last processed event ID and reuse it when reconnecting. The HTTP events endpoint provides the same cursor semantics for diagnostics or catch-up.

## Client boundary

The desktop screen stores one connection-bootstrap profile in WebView local storage: name, LAN/Tailscale mode, host, and port. A connection test retrieves capabilities, checks protocol compatibility, retries one transient network failure, and presents disconnected, connecting, reconnecting, connected, or error state.

The packaged desktop client uses Tauri's native HTTP plugin so private plain-HTTP connections work consistently on macOS and Windows. Its capability permits `http://` destinations because the host is user-configured; profile validation rejects wildcard destinations, and the UI makes the private-network-only boundary explicit. No HTTPS trust override or certificate bypass is enabled.

This is deliberately not the future client persistence layer. Phase 1 stores no chat, model, attachment, credential, or product data and has no background connection manager.

## Validation

Server tests cover configuration safety, simultaneous listeners, clean stop/restart, migrations, idempotency conflicts, cancellation, state/event transactions, restart recovery, progress preservation, HTTP capability/error behavior, and WebSocket replay plus live delivery. Dashboard tests cover address grouping and Tailscale setup states. Client tests cover profile storage/validation, address construction, state transitions, retries, capability retrieval, and protocol mismatch reporting. Run the repository-prescribed suite with `pnpm check`; use `pnpm build:server-dashboard:web` for local dashboard validation and `pnpm build:server:windows` on Windows for the NSIS installer.
