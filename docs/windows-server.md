# Windows server installation

## Install

1. Download the 64-bit `Tome Server_*-setup.exe` artifact and run it.
2. Approve the Windows administrator prompt. The installer is per-machine and adds Tome Server to the Start menu.
3. The installer adds two inbound rules for TCP port 7331, scoped to the Tome Server executable. The LAN rule permits only `LocalSubnet` on Private networks; the Tailscale rule permits only `100.64.0.0/10`.
4. Launch **Tome Server**. On first run, review the Ollama runtime state, port, LAN access, Tailscale access, firewall permission, data directory, and launch-at-login choice, then select **Save and start Tome Server**.
5. If Ollama is not installed, select **Get Ollama from official site**. Tome opens the [official Ollama for Windows download page](https://ollama.com/download/windows); you decide whether to download and run Ollama's normal Windows installer. Tome does not run a remote PowerShell script, silently install a package, or bypass normal Windows permissions and UAC.
6. If Ollama is installed but stopped, select **Start Ollama**. Tome starts `ollama serve` with `OLLAMA_HOST=127.0.0.1:11434` and checks only the official [`GET /api/version`](https://docs.ollama.com/api/version) capability.
7. In **Model setup**, review detected hardware/runtime and install a compatible profile or manually choose an approved catalog model. A download remains resumable if the app or PC restarts.

The development installer is unsigned, so Windows may show a publisher warning. Release signing is intentionally deferred.

## Ollama runtime boundary

Ollama is Tome's primary local AI runtime. The dashboard detects the official Windows executable, checks whether the local service is reachable, and reports the version capability as one of these actionable states:

- **Not installed** — open the official Ollama download page and complete its installer yourself.
- **Installed but not running** — start Ollama from the dashboard or the Ollama app.
- **Ready** — the version API responded successfully on loopback.
- **Incompatible** — another or incompatible service answered at the expected endpoint; update or repair Ollama.
- **Unreachable** — the loopback request timed out or otherwise could not be completed; inspect the local Ollama process and retry.
- **Install failed** or **start failed** — follow the displayed error and retry the explicit action.

Tome hard-codes its Ollama connection to `http://127.0.0.1:11434`. It never asks Ollama to bind to a LAN, wildcard, public, or Tailscale address and never creates an Ollama firewall rule. Remote clients connect only to Tome Server on its separately configured trusted interfaces; Tome Server remains the network boundary. This setup check does not call Ollama model inventory, model pull, or inference APIs.

The standard Ollama Windows app installs in the current user's account and runs in the background; see Ollama's [official Windows documentation](https://github.com/ollama/ollama/blob/main/docs/windows.mdx) for current OS and installation details. If an organizational policy requires administrator approval, let Windows present that prompt normally rather than working around it.

## Connect

The dashboard lists all usable addresses and marks each currently active listener. Use:

- **This PC** only for software on the server itself.
- **Private LAN** for a trusted device on the same private network.
- **Tailscale** for a signed-in device in the same tailnet. Install Tailscale and sign in on both devices first.

LAN and Tailscale listeners can run at the same time. Tome binds each displayed address directly and never uses a wildcard or public listener. If a VPN, Wi-Fi, Ethernet, sleep/wake, or DHCP change alters the address list, the dashboard shows that a restart is required.

The service has no application-level login in Phase 1. Do not enable router port forwarding, a public tunnel, or a Public-profile firewall exception.

## Operate and diagnose

Closing the dashboard window leaves the service running in the system tray. Open the tray menu to restore the dashboard or choose **Quit and stop server** for a graceful shutdown. The dashboard can start, stop, or restart the server, copy an address, open the data directory, show recent events, and copy a diagnostics summary.

If Tailscale is unavailable, the dashboard distinguishes not installed, not signed in, and running without an assigned address. If a listener cannot bind, check for another process using the configured port and use **Restart** after correcting it. Firewall status and the active listener marker should both be healthy before troubleshooting a client.

If Ollama remains **Installed but not running**, open the Ollama app once or use **Start Ollama**. If the dashboard reports **Incompatible**, verify that no unrelated process is using TCP port 11434 and update Ollama from the official download page. If it reports **Unreachable**, inspect the Ollama logs in `%LOCALAPPDATA%\Ollama` and confirm local security software permits this PC to contact `127.0.0.1`. Do not fix either condition by exposing port 11434 to another device.

Changing the port or firewall option may trigger an administrator prompt so Tome can replace the restricted inbound rules.

The model panel shows system memory, GPU data when Windows reports it, filesystem free space, llama.cpp availability, installed/default/loaded models, and durable download progress. GPU memory is shown as unknown when the Windows probe does not provide a reliable value. Install a reviewed `llama-server.exe` on `PATH` or configure `TOME_LLAMA_SERVER_PATH`; without it, downloads may be verified and registered but load/readiness remains explicitly unavailable.

## Uninstall or upgrade

Run the Tome Server uninstaller from Windows Settings or the Start menu. Uninstall removes the Tome firewall rules and launch-at-login registration. It then asks whether to remove the local settings, SQLite job history, and other local data; choose **No** to retain them for a later reinstall.

Installing a newer package replaces the application and refreshes the default-port firewall rules. Open the dashboard after upgrading and reapply settings if the configured port is not 7331.
