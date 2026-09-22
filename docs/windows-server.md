# Windows server installation

## Install

1. Download the 64-bit `Tome Server_*-setup.exe` artifact and run it.
2. Approve the Windows administrator prompt. The installer is per-machine and adds Tome Server to the Start menu.
3. The installer adds two inbound rules for TCP port 7331, scoped to the Tome Server executable. The LAN rule permits only `LocalSubnet` on Private networks; the Tailscale rule permits only `100.64.0.0/10`.
4. Launch **Tome Server**. On first run, review the port, LAN access, Tailscale access, firewall permission, data directory, and launch-at-login choice, then select **Save and start Tome Server**.

The development installer is unsigned, so Windows may show a publisher warning. Release signing is intentionally deferred.

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

Changing the port or firewall option may trigger an administrator prompt so Tome can replace the restricted inbound rules.

## Uninstall or upgrade

Run the Tome Server uninstaller from Windows Settings or the Start menu. Uninstall removes the Tome firewall rules and launch-at-login registration. It then asks whether to remove the local settings, SQLite job history, and other local data; choose **No** to retain them for a later reinstall.

Installing a newer package replaces the application and refreshes the default-port firewall rules. Open the dashboard after upgrading and reapply settings if the configured port is not 7331.
