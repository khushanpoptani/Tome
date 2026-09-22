# Phase 4 text chat and Context Manager V1

Phase 4 is Tome's first usable text-chat alpha. The macOS client remains the durable owner of chats and messages. The Windows server owns verified model execution, persistent inference jobs/events, partial generated output, and deterministic context manifests. Job retention is not chat retention.

The server and its internal llama.cpp process are unauthenticated. Run Tome only on an explicitly trusted private LAN or tailnet. Never expose either service to the public Internet. Prompts, model data, and telemetry are not uploaded to third parties.

## Runtime provisioning and process boundary

The Windows GUI installer includes the reviewed upstream `ggml-org/llama.cpp` CPU x64 build `b10964`, source revision `b29c606e28a01b1bc8c1351026a0fa6e616bf6c4`. Its original archive is 18,427,629 bytes with SHA-256 `917f39c076402c421224824607397af20f53625a60defc20e8dd22446bf4c5d7`. Installer resources include `llama-server.exe`, CPU/DLL dependencies, a file-by-file SHA-256 manifest, the llama.cpp MIT license, and LLVM OpenMP license.

At startup Tome prefers `TOME_LLAMA_SERVER_PATH` only when an advanced operator explicitly sets it. Otherwise an installed bundled runtime is selected before `PATH`. Every bundled file is checked against the reviewed manifest before execution, and the executable's real `--version` output is reported. An unavailable, altered, timed-out, or incompatible runtime fails visibly; Tome never simulates a response.

Tome starts `llama-server` directly without a shell, passes only a canonical verified model path, binds it to `127.0.0.1` on a dynamically reserved port, disables its web UI, hides its Windows console, waits up to 120 seconds for health, bounds/redacts captured logs, and terminates managed children during unload and shutdown. The private runtime port is never exposed by Axum or firewall rules. Phase 4 uses one CPU inference lane and one resident model; GPU optimization and multi-model scheduling remain later work.

## Inference API

`POST /api/v1/inference` accepts schema version 1:

```json
{
  "schema_version": 1,
  "client_request_id": "stable-client-uuid",
  "model_id": "catalog:qwen2.5-0.5b-instruct-q4-k-m",
  "messages": [
    { "id": "stable-message-uuid", "role": "user", "content": "Hello" }
  ],
  "system_instructions": [
    {
      "id": "stable-instruction-uuid",
      "content": "Be concise.",
      "pinned": true
    }
  ],
  "settings": {
    "temperature": 0.7,
    "max_output_tokens": 512,
    "agent_tool_reserve": 256
  },
  "correlation": {
    "chat_id": "opaque-client-chat-id",
    "user_message_id": "opaque-client-message-id",
    "assistant_message_id": "opaque-client-message-id",
    "client_revision_id": null
  },
  "parent_job_id": null,
  "retry_of_job_id": null
}
```

Messages must start with `user`, alternate `user`/`assistant`, and end with the current user. System content uses `system_instructions`. JSON provides valid UTF-8; IDs, message counts, per-message bytes, total bytes, temperature, output tokens, and reserves are bounded. Defaults are temperature `0.7`, maximum output `512`, and future agent/tool reserve `256`. The server cap is 4,096 output tokens, further constrained by the selected model.

`client_request_id` is the idempotency key. Repeating an identical request returns the original job; reuse with different data returns `idempotency_conflict`. Model identity is never substituted. `GET /api/v1/jobs/{job-id}/inference` returns authoritative request metadata, artifact SHA-256, settings, context manifest, accumulated output/sequence, usage, completion reason, and warnings.

Generic `POST /api/v1/jobs` rejects inference so an unvalidated prompt blob cannot bypass this contract. Retry creates a new linked job and never overwrites the original.

## Context Manager V1

After loading the verified model, Tome asks the same managed llama.cpp runtime to apply its real chat template and tokenize the rendered prompt. The server does not accept a preformatted prompt. Selection is pinned system instructions, the most recent complete prior messages that fit, and the current user message, followed by distinct output-token and future agent/tool reserves.

No message is truncated internally. If pinned instructions plus the current input and reserves do not fit, the job fails with `context_overflow`. Changing the model recalculates everything with the new artifact's tokenizer, template, and context limit. Phase 4 does not summarize, retrieve, call tools, run agents, or include attachments.

Before generation the server persists a deterministic manifest containing strategy/version, model/artifact identity, tokenizer/template identities, runtime version, context limit, exact input count, exact marginal category/message counts, both reserves, included IDs, excluded IDs/reasons, truncation decisions, and warnings.

## Streaming, stopping, and recovery

The durable stream adds `inference.context_prepared`, `generation_started`, `text_delta`, `output_checkpoint`, `usage_updated`, `stopped`, `completed`, `failed`, and `interrupted` events. Each delta/checkpoint carries a monotonically increasing `output_sequence`. Deltas are time/size coalesced and transactionally appended; full checkpoints are periodic and bounded. Replay retains the Phase 1 global `event_id`. Clients ignore duplicate sequences and replace local output with newer authoritative checkpoints, so live and replay paths converge.

Cancellation changes the job terminal state transactionally. The runtime stream polls cancellation at a bounded interval, drops the request promptly, and preserves committed output as `stopped`. A completion/cancel race has one durable terminal state. On restart, running jobs become `interrupted`; manifest/output remain queryable and queued jobs remain queued.

The client atomically saves user and placeholder assistant messages before submission. It stores stable correlation, renders deltas in memory, writes debounced partial files through Phase 3 native storage, and reconciles terminal output into chat JSON. On restart it loads local partials and reconciles with the server when reachable. Regenerate uses `retry_of_job_id`; edit-and-resend keeps the original branch and links the new job through `parent_job_id`.

## Client and exports

The prompt dock supports multiline input (`Shift+Enter`), send (`Enter`), verified compatible model selection, temperature `0–2`, Send, Stop, Regenerate, and Edit & resend. It presents queued, loading model, preparing context, generating, stopped, completed, failed, interrupted, reconnecting, and offline states. Attach is disabled because attachments are Phase 6.

Assistant messages retain model/artifact, settings, job/state, sequence, usage, completion reason, context warnings, and retry linkage. Local browsing/export remains available offline; sending needs a connected server.

Each interaction exports Markdown or schema-versioned JSON. Online JSON includes the local pair and authoritative job, manifest, events, output, usage, and terminal state. Offline export is labeled `local_only`. Exports use the Phase 3 external-save boundary and redact credential-shaped fields, Authorization/Bearer lines, and common signed/token query values. Complete ZIP export remains Phase 8.

## Limits and errors

| Limit              |         Value |
| ------------------ | ------------: |
| Messages           |           256 |
| One message        | 256 KiB UTF-8 |
| Structured request |         4 MiB |
| Output setting     |  4,096 tokens |
| Persisted output   |        16 MiB |
| Inference event    |       128 KiB |
| Replay page        |  1,000 events |

Validation/model/context errors are terminal until the request or installation changes. Offline/timeouts and interrupted jobs are retryable with a linked job. Tome never silently lowers settings, switches models, truncates inside a message, or returns synthetic text.

## Manual validation

1. Install the Windows NSIS artifact on a clean x64 VM and confirm Hardware reports verified bundled `b10964` without `PATH`, environment variables, or a console.
2. Install a compatible Qwen GGUF and connect the Apple Silicon client over private LAN and Tailscale.
3. Send with defaults, reconnect mid-stream, and confirm final text has no gaps or duplicates.
4. Switch models between prompts and confirm the manifest names the selected artifact without fallback.
5. Force overflow with pinned/current content; confirm no inference starts and no partial message truncation occurs.
6. Stop generation; confirm partial output survives app restarts and exports as stopped.
7. Restart the server while generating; confirm interrupted output/manifest reconcile locally.
8. Regenerate and edit/resend; confirm originals remain and new jobs have retry/parent links.
9. Export Markdown/JSON online and offline; confirm credentials, signed queries, and unsafe paths are absent.
10. Confirm Axum rejects public/wildcard binds and the llama child listens only on loopback.

## Phase boundary

Phase 4 excludes summarization/retrieval (Phase 5), attachment execution (Phase 6), GPU and multi-model scheduling (Phase 7), and complete ZIP export or remote administration (Phase 8).
