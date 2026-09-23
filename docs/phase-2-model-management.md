# Phase 2 model management

Phase 2 adds model administration without adding chat or inference. The server remains an unauthenticated, single-user service for an explicitly trusted LAN or tailnet. Never forward its port, expose it through a public tunnel, or add a Public-profile firewall rule.

## Readiness and inventory

After network setup, the Windows dashboard and connected macOS client request the same `/api/v1` hardware, job, and inventory contracts. Both surfaces provide an explicit **Download a model** flow. The checked-in catalog and setup-profile endpoints remain available for compatibility and internal testing, but the user interface is not catalog-first and does not present first-run profile cards.

The server becomes model-ready only when it has a verified, runtime-compatible text-output model. The first such model is selected as the fallback/default. Changing the default is transactional. Removing the default selects the oldest remaining verified text model or clears the setting. This readiness flag does not claim that prompt inference exists.

## Hardware and runtime discovery

`GET /api/v1/hardware` returns schema version 1, CPU architecture/brand/features, logical CPU count, total and available RAM, model-filesystem free space, GPU identity, GPU memory when Windows reports it, and runtime availability. The bounded Windows GPU probe uses `Win32_VideoController`; its `AdapterRAM` value is described as firmware-reported capacity rather than live usable allocation. Apple unified memory and unsupported probes report VRAM as unknown.

The Phase 2 production runtime boundary supports an external `llama-server` executable only. Tome checks `TOME_LLAMA_SERVER_PATH` and then `PATH`, with a three-second version probe. The reviewed Windows x64 CPU baseline is upstream `ggml-org/llama.cpp` build `b10964`, source revision `b29c606e28a01b1bc8c1351026a0fa6e616bf6c4`, artifact `llama-b10964-bin-win-cpu-x64.zip` (18,427,629 bytes, SHA-256 `917f39c076402c421224824607397af20f53625a60defc20e8dd22446bf4c5d7`, MIT license). Tome does not download or bundle it in Phase 2 and reports the actual probed runtime version rather than assuming this baseline. Load starts the runtime on loopback, waits up to ten seconds for its health endpoint, and records loaded state only after success. Stop/restart resets stale loaded/in-use flags and terminates child processes. Catalog-only formats never report load success.

## Provider resolution and the legacy catalog

Model discovery is constrained to the Hugging Face model API. A query can be a friendly name, an exact `owner/repository` identifier, or an Ollama-style alias. For example, `llama3.2:3b` is normalized to a search for Llama 3.2 3B GGUF artifacts; it does not claim that Tome can import or download from Ollama. Search results contain only single-file GGUF candidates and show the exact repository, immutable commit, filename, parameters when discoverable, quantization, byte size, license/access state, context, tokenizer/chat-template availability, capabilities, runtime status, and disk/RAM estimates. Missing provider metadata is displayed as unknown.

The user selects and confirms an exact artifact. The download request sends only the provider, repository, immutable revision, and artifact path; the server resolves that tuple again and constructs the download URL itself. Arbitrary URLs are never accepted. Private or gated repositories are reported as credential-required and cannot be downloaded because Tome intentionally has no provider-token storage.

The checked-in catalog is retained as a legacy internal source:

The checked-in source of truth is `apps/server/catalog/v1.json`, currently catalog version `2026-09-22.1`. Every entry pins a full upstream revision and artifact path, byte count, SHA-256, license/access state, format, quantization, runtime, context/tokenizer/chat-template metadata, capabilities, and resource estimates. A catalog change requires review, a new `catalog_version`, verified upstream metadata, and a migration assessment. Existing installed rows retain catalog provenance, so catalog replacement does not silently mutate installations.

Current primary sources and selection rationale:

- Qwen publishes official Apache-2.0 GGUF repositories for [Qwen2.5 0.5B Instruct](https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/tree/9217f5db79a29953eb74d5343926648285ec7e67) and [Qwen2.5 1.5B Instruct](https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/tree/91cad51170dc346986eccefdc2dd33a9da36ead9). Their Q4_K_M artifacts are the executable Minimal and Recommended defaults.
- [all-MiniLM-L6-v2](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/tree/1110a243fdf4706b3f48f1d95db1a4f5529b4d41) is a small Apache-2.0 embedding default.
- [Florence-2 Base](https://huggingface.co/microsoft/Florence-2-base/tree/5ca5edf5bd017b9919c05d08aebef5e4c7ac3bac) is one MIT artifact covering the vision and OCR catalog roles without a redundant second download.
- [Whisper Tiny](https://huggingface.co/openai/whisper-tiny/tree/169d4a4341b33bc18d8881c4b69c2e104e1cc0af) is the smallest official multilingual Apache-2.0 transcription checkpoint.
- [Qwen3Guard Gen 0.6B](https://huggingface.co/Qwen/Qwen3Guard-Gen-0.6B/tree/fada3b2f655b89601929198343c94cd2f64d93cc) is the optional Apache-2.0 safety entry.
- The runtime boundary follows the official [llama.cpp server](https://github.com/ggml-org/llama.cpp/blob/b29c606e28a01b1bc8c1351026a0fa6e616bf6c4/tools/server/README.md) and its pinned [b10964 release](https://github.com/ggml-org/llama.cpp/releases/tag/b10964). Runtime distribution is deliberately separate from catalog artifact identity.

Embedding, vision/OCR, transcription, and safety entries are **catalog-only** in Phase 2. Downloading them does not imply executable support. The One-model-per-supported-capability profile therefore contains only the recommended text model today.

## Downloads, verification, and recovery

`POST /api/v1/model-downloads` creates an idempotent Phase 1 `model_download` job. The public UI supplies an exact Hugging Face selection, an idempotency key, and explicit license acceptance. A legacy catalog ID remains accepted for compatibility. The server re-resolves provider metadata and persists the resolved immutable artifact in the durable job input, so restart/resume does not depend on a later search. Secrets, Authorization headers, arbitrary URLs, and signed URLs are not accepted or persisted.

Downloads use deterministic `.part` files beneath `model-downloads/`, 15-second connect and 30-second stalled-read timeouts, at most five redirects restricted to approved Hugging Face delivery hosts, a bounded six-hour overall request, disk preflight, expected content length, byte ceiling, and HTTP Range resume. Servers that ignore Range cause a safe restart from byte zero. Progress and terminal states use the existing transactional job/event history. Pause preserves the partial, resume requeues it, retry creates a linked job, cancel never registers it, and a server restart requeues only downloads interrupted by that restart.

Before atomic rename and registration, Tome validates the exact byte count, GGUF version/count structure, and provider-reported SHA-256. It inspects bounded GGUF metadata and uses discovered name, architecture/tokenizer, context, chat template, and file type to refine the final registered specifications. Corrupt, non-GGUF, size-mismatched, or hash-mismatched files remain unregistered. A stable hash of provider/repository/revision/artifact prevents duplicate installation, and an already completed matching artifact is registered idempotently.

## Storage and deletion safety

SQLite migration `0002_models.sql` adds durable artifact provenance/capabilities/compatibility, download checkpoints, license acceptance records, and singleton default-model state. No credentials are stored. Inventory export is ordered by stable model ID and contains metadata only.

All generated path components reject separators, dot components, NUL bytes, and oversized names. Parent directories and existing deletion targets are canonicalized beneath configured roots, so symlink/reparse-point escapes are rejected. Phase 2 downloads only single-file, non-archive artifacts. Deletion requires the exact model ID, refuses loaded/in-use/unverified targets, and moves the file to `model-trash/` for operator recovery rather than immediately erasing it.

## API summary

In addition to Phase 1 routes:

| Method and path                   | Purpose                                             |
| --------------------------------- | --------------------------------------------------- |
| `GET /api/v1/hardware`            | Versioned hardware/runtime/storage capabilities     |
| `POST /api/v1/hardware/refresh`   | Repeat bounded probes                               |
| `GET /api/v1/model-catalog`       | Pinned catalog and profiles                         |
| `GET /api/v1/model-setup`         | Profile compatibility and estimates                 |
| `GET /api/v1/model-search?q=...`  | Resolve compatible Hugging Face GGUF candidates     |
| `POST /api/v1/model-downloads`    | Idempotent persistent download job                  |
| `POST /api/v1/jobs/{id}/pause`    | Pause a model download                              |
| `POST /api/v1/jobs/{id}/resume`   | Resume a paused/interrupted download                |
| `GET /api/v1/models`              | Authoritative installed inventory/readiness/default |
| `GET /api/v1/models/export`       | Deterministic credential-free JSON inventory        |
| `PUT /api/v1/models/default`      | Select verified text fallback                       |
| `POST /api/v1/models/{id}/load`   | Truthful runtime load                               |
| `POST /api/v1/models/{id}/unload` | Unload when not referenced                          |
| `DELETE /api/v1/models/{id}`      | Confirmed recoverable deletion                      |

The macOS client persists only the Phase 1 connection profile and last event cursor. It replays WebSocket events after reconnect and refreshes authoritative inventory; it does not create chat storage.

## Troubleshooting and limitations

- **Runtime unavailable:** install a reviewed llama.cpp build and make `llama-server` visible on `PATH`, or set `TOME_LLAMA_SERVER_PATH`, then refresh hardware.
- **Low disk:** free space under the filesystem containing the configured data directory. The estimate includes artifact-specific overhead.
- **Partial download:** use Resume. If the upstream server does not honor Range, Tome restarts that artifact safely.
- **Hash failure:** search again and confirm the provider artifact metadata remains current. Never bypass verification.
- **Gated/private repository:** use a public artifact. Tome does not accept or store Hugging Face tokens.
- **Catalog-only:** the artifact is intentionally not loadable in Phase 2.
- **Offline client:** reconnect to the same private address; the saved event cursor and authoritative refresh recover progress.
- **Known boundary:** Phase 2 does not bundle llama.cpp, implement inference, execute vision/OCR/transcription/safety models, manage repository credentials, extract archives, or schedule multiple resident models.
