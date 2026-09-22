# Tome

Tome is a macOS desktop client and a self-hosted Windows server for local AI conversations.

This repository contains the Phase 1 private-network server and durable job protocol, Phase 2 model management, Phase 3 macOS client foundation, and Phase 4 end-to-end text chat. The client owns durable local conversations while the Windows server provides verified llama.cpp execution, deterministic context selection, replayable token streaming, cancellation/recovery, and authoritative per-job inference records. Attachment processing and permanent server-side chat history remain later-phase work.

- [Development setup](docs/development.md)
- [Architecture decisions](docs/architecture.md)
- [Windows server installation](docs/windows-server.md)
- [Phase 2 model management](docs/phase-2-model-management.md)
- [Phase 3 client storage](docs/phase-3-client-storage.md)
- [Phase 4 text chat and Context Manager V1](docs/phase-4-text-chat.md)
- [Contributing](CONTRIBUTING.md)
