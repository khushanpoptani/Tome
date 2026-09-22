# Phase 3 client foundation and local storage

Phase 3 establishes the macOS desktop client as the permanent owner of user settings, server profiles, chats, attachment foundations, partial-response checkpoints, and export-cache metadata. The Rust service behind Tauri commands is authoritative. The WebView does not use `localStorage` for product state or event cursors.

The connected server still owns models, durable jobs, and job event replay. It does not gain a chat-history API. A local chat may retain a server job ID, parent/retry link, profile ID, and last-known state as a reference only; server retention and local chat retention are independent.

## Managed data layout

Tauri resolves the OS application-data directory for `com.khushanpoptani.tome`, and the storage service canonicalizes it before use:

```text
<app-data>/
  settings.json[.tmp|.bak]
  profiles.json[.tmp|.bak]
  chats/<chat-uuid>.json[.tmp|.bak]
  attachments/
    index.json
    content/<attachment-uuid>.original|.gz
    partial/
  partials/
    index.json
    content/
  export-cache/
    index.json
    content/
  quarantine/<record-kind>-<source-name>-<uuid>.bad
```

Chat files are authoritative; there is no required chat index. List and search independently validate each file, so one bad chat does not block the shell or other chats. Generated installers, local records, caches, and managed content must never be committed.

Every generated path component is a canonical lowercase UUID. Supplied paths, separators, dot components, NUL characters, absolute paths, root/home targets, and symlink escapes are rejected. Attachment commands accept bytes plus metadata rather than trusting a source path. User-created portable exports are outside the managed root and are never candidates for delete-all.

## Versioned schemas

All top-level files use `schemaVersion: 1`. Unknown top-level settings and chat fields are preserved when reasonable. Readers inspect the version before typed deserialization. Version `0` or missing-version JSON enters the explicit migration path; a version newer than this build supports fails closed and the original is retained.

### Settings and profiles

`settings.json` contains stable default/last-used profile IDs, per-profile default model IDs, neutral temperature (`0.7`, accepted range `0–2`), attachment mode, `system`/`light`/`dark` appearance, future context-display flags, partial-response flags, and bounded reconnection preferences. Mutable profile names are never foreign keys.

`profiles.json` contains a version and ordered profiles. Each profile has a stable UUID, display name, `lan` or `tailscale` mode, host, port, timestamps, durable last event cursor, and last compatibility result. No credential, token, Authorization header, or signed URL field exists. Public numeric IP destinations and wildcard addresses are rejected by the connection layer. The client never automatically connects on launch, so a stale endpoint cannot be contacted without a fresh user action.

### Chats, messages, and jobs

Each `chats/<id>.json` contains the matching stable chat UUID, title, timestamps, optional stable server-profile/model references, ordered messages, optional job references, and attachment IDs. Messages have stable UUIDs, role, content, timestamp, optional parent message, and attachment IDs. Phase 3 creates empty chats and imports valid records; Phase 4 will populate real prompt/response messages.

Duplicate or noncanonical chat/message IDs, a filename/record ID mismatch, invalid references, oversized content, malformed JSON, and unsupported versions are isolated. Lists sort by descending `updatedAt`, then stable ID. Search is local, case-insensitive, bounded to 256 query characters, and covers title plus message content.

Job references contain server job/profile IDs, optional message/parent/retry IDs, last-known state, and timestamp. They are links, not a copy of server history.

### Attachments, partial responses, and export cache

`attachments/index.json` tracks original name, declared/detected media type, original/stored sizes, SHA-256, storage mode, managed relative path, timestamps, chat/message references, future processing status, and optional derivative metadata.

- `optimized` writes a bounded, chunk-fed gzip copy. It does not claim semantic media normalization.
- `preserve_originals` writes the original bytes and reserves derivative metadata.
- `metadata_only` stores no managed content copy.

Phase 3 limits an attachment command to 64 MiB and makes no deduplication claim. Simple explicit copies avoid unsafe reference counts. Deleting a chat removes attachment records and managed files that reference it. There is no attach/send UI, upload, extraction, OCR, transcription, or model routing.

Cleanup is reference-first: a chat deletion validates its attachment, partial-response, and export-cache indexes, removes records that name that chat, then removes only their validated managed relative paths. A failed index is quarantined rather than guessed at. Unpublished `.tmp` files and unreferenced partial copies remain inside the managed root for deterministic recovery or delete-all; they are counted as storage use and are never followed outside the root. A future maintenance pass may quarantine them only after it can prove they are unreferenced.

`partials/index.json` defines response/chat/message/job IDs, byte length, managed relative path, and update time for Phase 4. `export-cache/index.json` defines cache/chat IDs, managed relative path, size, and creation time. Neither foundation implies generation or advanced export exists yet.

## Atomic writes and recovery

Every managed JSON/content write uses this sequence in the destination directory:

1. Create or truncate `<name>.tmp`.
2. Write the bounded payload and call `sync_all`.
3. Remove the one-generation old `.bak`, then rename the valid primary to `.bak`.
4. Atomically rename `.tmp` to the primary.
5. On Unix, best-effort sync the parent directory. If promotion fails, restore the backup when the primary is absent.

The same-directory temporary avoids cross-filesystem rename behavior. The primary-to-backup then temporary-to-primary sequence also works on Windows, where replacing an existing target with `rename` is not portable. Only one backup generation is retained.

On startup/read, candidates are evaluated deterministically: valid primary, valid temporary, then valid backup. A recovered candidate is rewritten through the atomic algorithm. A stale temporary is removed when the primary is valid. A newer unsupported primary is never replaced by an older backup. If no candidate is valid, all candidates move to quarantine and a content-free diagnostic identifies only the record kind and safe local ID. Settings/profile/index failures yield safe defaults; chat failures affect only that chat.

Migration mutates an in-memory JSON value first. The original is not removed until validation and atomic promotion succeed, so migration failures and restart recovery remain retryable.

## Profile and connection behavior

Connections can be created, edited, duplicated, deleted, tested, and selected by stable ID. LAN and Tailscale modes remain unauthenticated and private-network only. A test requests `/api/v1/capabilities`, sends protocol version `1`, retries one transient failure, and distinguishes disconnected, connecting, reconnecting, connected, offline, incompatible, and error states. Successful compatibility metadata and the replay cursor are saved natively.

Deleting a profile clears default/last-used settings and its default-model preference. Chats retain the stable reference and display the profile as missing; chat content is not deleted. Model management continues to use the Phase 2 APIs for the active connection.

## Import and export

Import accepts one UTF-8 Tome JSON object up to 16 MiB, runs migration and validation, ignores external path intent, and writes a native chat record. A colliding chat ID receives a new UUID and preserves the source ID only as inert metadata. Import does not copy attachments in Phase 3.

Export serializes one validated chat as readable JSON and recursively removes fields named like credentials, tokens, Authorization headers, or signed URLs. The browser download is a portable file chosen by the user, not client-managed after download.

## Delete all client data

Settings previews category counts and sizes. Deletion requires the exact phrase `DELETE ALL LOCAL TOME DATA`. The service revalidates the canonical root, rejects root/home/broad/symlinked targets, and resolves only a fixed allowlist: chats, attachments, partial responses, export cache, quarantine, profiles, and settings. It reports successes and failures separately, recreates only empty managed directories, and verifies first-run state when all categories succeed. External exports cannot be touched.

## Job replay

The jobs screen fetches `/api/v1/jobs`, opens `/api/v1/events/ws?after_event_id=<cursor>`, advances the native per-profile cursor monotonically, refreshes after events, reconnects after interruption, and polls as a fallback. It displays type, state, progress, timestamps, parent/retry linkage, and server errors. Only existing cancel and retry operations are exposed.

## Backups and troubleshooting

- For a complete backup, quit Tome and copy the application-data directory. Individual chat JSON files omit managed attachments.
- If a chat is quarantined, other chats remain usable. Preserve `quarantine/` before manual repair and import only a validated portable chat.
- Corrupt settings/profiles load safe defaults and identify the record type without logging content.
- Offline/incompatible servers do not block local browsing, search, settings, import/export, or delete-all.
- The Vite browser preview is intentionally read-only for product storage. Use the Tauri app for native workflows.

## Manual Phase 3 validation

1. Create two LAN/Tailscale profiles, quit/reopen, test one, and verify selection, compatibility, and cursor persistence.
2. Create, rename, search, export, delete, and re-import a chat while offline.
3. Put a truncated chat beside a valid chat in a temporary test root and confirm only the bad record is quarantined.
4. Interrupt a test write after `.tmp` or backup promotion and confirm restart recovery.
5. Connect to a Phase 2 server, confirm models still work, and confirm jobs refresh after WebSocket replay.
6. Exercise all attachment modes through native tests; confirm no attachment-send control appears.
7. Review delete-all sizes, enter the exact confirmation, and confirm an external export survives.

## Phase boundary

Phase 3 does not implement prompt inference, a composer/send button, token streaming, a server context manager, real attachment selection/upload/processing, OCR, transcription, advanced context management, scheduling, permanent server chat history, or later export/admin features.
