#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{
    Manager, State, WindowEvent,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Mutex;
use tome_server::{
    model::PROTOCOL_VERSION,
    network::{
        AddressKind, ListenerSelection, NetworkAddress, enumerate_addresses,
        select_listener_addresses,
    },
    runtime::{RuntimeConfig, ServerHandle},
};

const DEFAULT_PORT: u16 = 7331;
const MAX_LOGS: usize = 100;
#[cfg(windows)]
const LAN_RULE: &str = "Tome Server (Private LAN)";
#[cfg(windows)]
const TAILSCALE_RULE: &str = "Tome Server (Tailscale)";
#[cfg(windows)]
const TAILSCALE_REMOTE_RANGE: &str = "100.64.0.0-100.127.255.255";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct DashboardSettings {
    port: u16,
    lan_enabled: bool,
    tailscale_enabled: bool,
    firewall_enabled: bool,
    launch_at_login: bool,
    data_directory: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ServerStatus {
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize)]
struct LogEntry {
    timestamp: String,
    level: &'static str,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
struct AddressView {
    interface_name: String,
    address: String,
    kind: AddressKind,
    url: String,
    active: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
enum TailscaleState {
    Connected { hostname: Option<String> },
    NotInstalled,
    NotSignedIn,
    NoAddress,
}

#[derive(Debug, Clone, Serialize)]
struct DashboardSnapshot {
    status: ServerStatus,
    status_message: String,
    settings: DashboardSettings,
    first_run: bool,
    addresses: Vec<AddressView>,
    tailscale: TailscaleState,
    firewall_status: String,
    network_change_pending: bool,
    data_directory: String,
    version: &'static str,
    protocol_version: u32,
    logs: Vec<LogEntry>,
    diagnostics: String,
}

struct InnerState {
    settings: DashboardSettings,
    first_run: bool,
    status: ServerStatus,
    status_message: String,
    server: Option<ServerHandle>,
    active_addresses: Vec<IpAddr>,
    logs: VecDeque<LogEntry>,
}

struct DashboardState {
    settings_path: PathBuf,
    inner: Mutex<InnerState>,
}

#[tauri::command]
async fn get_dashboard(state: State<'_, DashboardState>) -> Result<DashboardSnapshot, String> {
    snapshot(&state).await
}

#[tauri::command]
async fn save_settings(
    state: State<'_, DashboardState>,
    settings: DashboardSettings,
) -> Result<DashboardSnapshot, String> {
    validate_settings(&settings)?;
    tokio::fs::create_dir_all(&settings.data_directory)
        .await
        .map_err(|error| error.to_string())?;
    let serialized = serde_json::to_string_pretty(&settings).map_err(|error| error.to_string())?;
    tokio::fs::write(&state.settings_path, serialized)
        .await
        .map_err(|error| error.to_string())?;
    apply_launch_at_login(settings.launch_at_login)?;
    {
        let mut inner = state.inner.lock().await;
        inner.settings = settings;
        inner.first_run = false;
        push_log(
            &mut inner,
            "info",
            "Settings saved. Restart the server to apply network changes.",
        );
    }
    snapshot(&state).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn start_server(state: State<'_, DashboardState>) -> Result<DashboardSnapshot, String> {
    tauri::async_runtime::block_on(async {
        start_server_inner(&state).await?;
        snapshot(&state).await
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn stop_server(state: State<'_, DashboardState>) -> Result<DashboardSnapshot, String> {
    tauri::async_runtime::block_on(async {
        stop_server_inner(&state).await?;
        snapshot(&state).await
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn restart_server(state: State<'_, DashboardState>) -> Result<DashboardSnapshot, String> {
    tauri::async_runtime::block_on(async {
        stop_server_inner(&state).await?;
        start_server_inner(&state).await?;
        snapshot(&state).await
    })
}

#[tauri::command]
async fn configure_firewall(
    state: State<'_, DashboardState>,
    enabled: bool,
) -> Result<DashboardSnapshot, String> {
    let port = state.inner.lock().await.settings.port;
    set_firewall_rules(enabled, port)?;
    let settings = {
        let mut inner = state.inner.lock().await;
        inner.settings.firewall_enabled = enabled;
        push_log(
            &mut inner,
            "info",
            if enabled {
                "Windows Firewall rules were requested through UAC."
            } else {
                "Windows Firewall rule removal was requested through UAC."
            },
        );
        inner.settings.clone()
    };
    let serialized = serde_json::to_string_pretty(&settings).map_err(|error| error.to_string())?;
    tokio::fs::write(&state.settings_path, serialized)
        .await
        .map_err(|error| error.to_string())?;
    snapshot(&state).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn open_data_directory(state: State<'_, DashboardState>) -> Result<(), String> {
    let path = state
        .inner
        .try_lock()
        .map_err(|_| "dashboard is busy; try again".to_owned())?
        .settings
        .data_directory
        .clone();
    open_directory(&path)
}

async fn start_server_inner(state: &DashboardState) -> Result<(), String> {
    let (settings, first_run) = {
        let mut inner = state.inner.lock().await;
        if inner.server.is_some() || inner.status == ServerStatus::Starting {
            return Ok(());
        }
        inner.status = ServerStatus::Starting;
        "Binding trusted network addresses…".clone_into(&mut inner.status_message);
        push_log(&mut inner, "info", "Starting Tome Server.");
        (inner.settings.clone(), inner.first_run)
    };
    if first_run {
        set_error(state, "Complete first-run setup before starting.").await;
        return Err("complete first-run setup before starting".to_owned());
    }
    let interfaces = enumerate_addresses().map_err(|error| error.to_string())?;
    let addresses = select_listener_addresses(
        &interfaces,
        ListenerSelection {
            lan_enabled: settings.lan_enabled,
            tailscale_enabled: settings.tailscale_enabled,
        },
    );
    let config = RuntimeConfig {
        listener_addresses: addresses.clone(),
        port: settings.port,
        database_path: settings.data_directory.join("tome.sqlite3"),
        temp_directory: settings.data_directory.join("temp"),
        retention_days: 30,
    };
    match ServerHandle::start(config).await {
        Ok(server) => {
            let listener_count = server.listeners.len();
            let mut inner = state.inner.lock().await;
            inner.server = Some(server);
            inner.active_addresses = addresses;
            inner.status = ServerStatus::Running;
            inner.status_message = format!("Listening on {listener_count} trusted address(es).");
            push_log(
                &mut inner,
                "info",
                &format!("Server running on port {}.", settings.port),
            );
            Ok(())
        }
        Err(error) => {
            set_error(state, &format!("Could not start server: {error}")).await;
            Err(error.to_string())
        }
    }
}

async fn stop_server_inner(state: &DashboardState) -> Result<(), String> {
    let server = {
        let mut inner = state.inner.lock().await;
        let server = inner.server.take();
        if server.is_some() {
            "Stopping listeners and saving job state…".clone_into(&mut inner.status_message);
            push_log(&mut inner, "info", "Stopping Tome Server.");
        }
        server
    };
    if let Some(server) = server {
        server.stop().await.map_err(|error| error.to_string())?;
    }
    let mut inner = state.inner.lock().await;
    inner.active_addresses.clear();
    inner.status = ServerStatus::Stopped;
    "Server is stopped.".clone_into(&mut inner.status_message);
    push_log(&mut inner, "info", "Server stopped cleanly.");
    Ok(())
}

async fn set_error(state: &DashboardState, message: &str) {
    let mut inner = state.inner.lock().await;
    inner.status = ServerStatus::Error;
    message.clone_into(&mut inner.status_message);
    push_log(&mut inner, "error", message);
}

async fn snapshot(state: &DashboardState) -> Result<DashboardSnapshot, String> {
    let interfaces = enumerate_addresses().map_err(|error| error.to_string())?;
    let tailscale = detect_tailscale(&interfaces);
    let firewall_status = firewall_status();
    let inner = state.inner.lock().await;
    let selected = select_listener_addresses(
        &interfaces,
        ListenerSelection {
            lan_enabled: inner.settings.lan_enabled,
            tailscale_enabled: inner.settings.tailscale_enabled,
        },
    );
    let addresses = interfaces
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                AddressKind::Loopback | AddressKind::Lan | AddressKind::Tailscale
            )
        })
        .map(|item| AddressView {
            interface_name: item.interface_name.clone(),
            address: item.address.to_string(),
            kind: item.kind,
            url: format!(
                "http://{}",
                SocketAddr::new(item.address, inner.settings.port)
            ),
            active: inner.active_addresses.contains(&item.address),
        })
        .collect::<Vec<_>>();
    let network_change_pending = inner.status == ServerStatus::Running
        && (selected.len() != inner.active_addresses.len()
            || selected
                .iter()
                .any(|address| !inner.active_addresses.contains(address)));
    let diagnostics = format!(
        "Tome Server {}\nProtocol {}\nStatus: {:?}\nPort: {}\nListeners: {:?}\nFirewall: {}\nTailscale: {:?}\nData: {}",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION,
        inner.status,
        inner.settings.port,
        inner.active_addresses,
        firewall_status,
        tailscale,
        inner.settings.data_directory.display()
    );
    Ok(DashboardSnapshot {
        status: inner.status,
        status_message: inner.status_message.clone(),
        settings: inner.settings.clone(),
        first_run: inner.first_run,
        addresses,
        tailscale,
        firewall_status,
        network_change_pending,
        data_directory: inner.settings.data_directory.display().to_string(),
        version: env!("CARGO_PKG_VERSION"),
        protocol_version: PROTOCOL_VERSION,
        logs: inner.logs.iter().cloned().collect(),
        diagnostics,
    })
}

fn detect_tailscale(addresses: &[NetworkAddress]) -> TailscaleState {
    let has_address = addresses
        .iter()
        .any(|item| item.kind == AddressKind::Tailscale);
    let output = hidden_command("tailscale", &["status", "--json"]).output();
    match output {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if has_address {
                TailscaleState::Connected { hostname: None }
            } else {
                TailscaleState::NotInstalled
            }
        }
        Err(_) => {
            if has_address {
                TailscaleState::Connected { hostname: None }
            } else {
                TailscaleState::NoAddress
            }
        }
        Ok(output) => {
            tailscale_state_from_probe(has_address, output.status.success(), &output.stdout)
        }
    }
}

fn tailscale_state_from_probe(has_address: bool, success: bool, stdout: &[u8]) -> TailscaleState {
    let value: Value = serde_json::from_slice(stdout).unwrap_or(Value::Null);
    let backend = value
        .get("BackendState")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let hostname = value
        .pointer("/Self/DNSName")
        .and_then(Value::as_str)
        .map(|value| value.trim_end_matches('.').to_owned())
        .filter(|value| !value.is_empty());
    if has_address {
        TailscaleState::Connected { hostname }
    } else if !success || matches!(backend, "NeedsLogin" | "Stopped") {
        TailscaleState::NotSignedIn
    } else {
        TailscaleState::NoAddress
    }
}

fn validate_settings(settings: &DashboardSettings) -> Result<(), String> {
    if settings.port == 0 {
        return Err("Port must be between 1 and 65535.".to_owned());
    }
    if settings.data_directory.as_os_str().is_empty() {
        return Err("Choose a data directory.".to_owned());
    }
    Ok(())
}

fn push_log(inner: &mut InnerState, level: &'static str, message: &str) {
    if inner.logs.len() == MAX_LOGS {
        inner.logs.pop_front();
    }
    inner.logs.push_back(LogEntry {
        timestamp: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".to_owned()),
        level,
        message: message.to_owned(),
    });
}

fn load_settings(path: &Path, default_data_directory: PathBuf) -> (DashboardSettings, bool) {
    let default = DashboardSettings {
        port: DEFAULT_PORT,
        lan_enabled: true,
        tailscale_enabled: true,
        firewall_enabled: true,
        launch_at_login: false,
        data_directory: default_data_directory,
    };
    let Ok(content) = std::fs::read(path) else {
        return (default, true);
    };
    serde_json::from_slice(&content).map_or((default, true), |settings| (settings, false))
}

#[cfg(windows)]
fn hidden_command(program: &str, arguments: &[&str]) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(program);
    command.args(arguments).creation_flags(0x0800_0000);
    command
}

#[cfg(not(windows))]
fn hidden_command(program: &str, arguments: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.args(arguments);
    command
}

#[cfg(windows)]
fn firewall_status() -> String {
    let script = format!(
        "if ((Get-NetFirewallRule -DisplayName '{LAN_RULE}' -ErrorAction SilentlyContinue) -and (Get-NetFirewallRule -DisplayName '{TAILSCALE_RULE}' -ErrorAction SilentlyContinue)) {{ exit 0 }} else {{ exit 1 }}"
    );
    hidden_command("powershell.exe", &["-NoProfile", "-Command", &script])
        .status()
        .map_or_else(
            |_| "Unable to inspect".to_owned(),
            |status| {
                if status.success() {
                    "Configured".to_owned()
                } else {
                    "Needs attention".to_owned()
                }
            },
        )
}

#[cfg(not(windows))]
fn firewall_status() -> String {
    "Managed by the Windows installer".to_owned()
}

#[cfg(windows)]
fn set_firewall_rules(enabled: bool, port: u16) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let remove = format!(
        "netsh advfirewall firewall delete rule name=\"{LAN_RULE}\"; netsh advfirewall firewall delete rule name=\"{TAILSCALE_RULE}\";"
    );
    let script = if enabled {
        format!(
            "{remove} netsh advfirewall firewall add rule name=\"{LAN_RULE}\" dir=in action=allow protocol=TCP localport={port} program=\"{}\" profile=private remoteip=LocalSubnet enable=yes; netsh advfirewall firewall add rule name=\"{TAILSCALE_RULE}\" dir=in action=allow protocol=TCP localport={port} program=\"{}\" profile=any remoteip={TAILSCALE_REMOTE_RANGE} enable=yes",
            executable.display(),
            executable.display()
        )
    } else {
        remove
    };
    let escaped = script.replace('"', "`\"");
    let elevate = format!(
        "$p=Start-Process powershell.exe -Verb RunAs -Wait -PassThru -ArgumentList '-NoProfile','-Command','{escaped}'; exit $p.ExitCode"
    );
    let status = hidden_command("powershell.exe", &["-NoProfile", "-Command", &elevate])
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Windows did not apply the firewall change.".to_owned())
    }
}

#[cfg(not(windows))]
fn set_firewall_rules(_enabled: bool, _port: u16) -> Result<(), String> {
    Err("Firewall rules can only be changed by the Windows application.".to_owned())
}

#[cfg(windows)]
fn apply_launch_at_login(enabled: bool) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let status = if enabled {
        hidden_command(
            "reg.exe",
            &[
                "add",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "Tome Server",
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{}\"", executable.display()),
                "/f",
            ],
        )
        .status()
    } else {
        hidden_command(
            "reg.exe",
            &[
                "delete",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "Tome Server",
                "/f",
            ],
        )
        .status()
    }
    .map_err(|error| error.to_string())?;
    if status.success() || !enabled {
        Ok(())
    } else {
        Err("Could not update launch at login.".to_owned())
    }
}

#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)]
fn apply_launch_at_login(_enabled: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn open_directory(path: &Path) -> Result<(), String> {
    hidden_command("explorer.exe", &[&path.display().to_string()])
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn open_directory(path: &Path) -> Result<(), String> {
    Command::new("open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn remove_local_data_for_uninstall() -> Result<(), String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "LOCALAPPDATA is unavailable".to_owned())?;
    let app_data = local_app_data.join("com.khushanpoptani.tome-server");
    let settings_path = app_data.join("settings.json");
    if let Ok(content) = std::fs::read(&settings_path)
        && let Ok(settings) = serde_json::from_slice::<DashboardSettings>(&content)
    {
        for name in ["tome.sqlite3", "tome.sqlite3-shm", "tome.sqlite3-wal"] {
            let path = settings.data_directory.join(name);
            if path.is_file() {
                std::fs::remove_file(path).map_err(|error| error.to_string())?;
            }
        }
        let temp = settings.data_directory.join("temp");
        if temp.is_dir() {
            std::fs::remove_dir_all(temp).map_err(|error| error.to_string())?;
        }
    }
    if settings_path.is_file() {
        std::fs::remove_file(settings_path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn main() {
    if std::env::args().any(|argument| argument == "--remove-local-data") {
        if remove_local_data_for_uninstall().is_err() {
            std::process::exit(1);
        }
        return;
    }
    tauri::Builder::default()
        .setup(|app| {
            let app_data = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&app_data)?;
            let settings_path = app_data.join("settings.json");
            let (settings, first_run) = load_settings(&settings_path, app_data.join("data"));
            let mut logs = VecDeque::new();
            logs.push_back(LogEntry {
                timestamp: OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_default(),
                level: "info",
                message: "Tome Server dashboard opened.".to_owned(),
            });
            app.manage(DashboardState {
                settings_path,
                inner: Mutex::new(InnerState {
                    settings,
                    first_run,
                    status: ServerStatus::Stopped,
                    status_message: if first_run {
                        "Complete first-run setup."
                    } else {
                        "Server is stopped."
                    }
                    .to_owned(),
                    server: None,
                    active_addresses: Vec::new(),
                    logs,
                }),
            });

            let show = MenuItem::with_id(app, "show", "Open dashboard", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit and stop server", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let handle = app.handle().clone();
            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .expect("dashboard icon is configured"),
                )
                .menu(&menu)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        let state = app.state::<DashboardState>();
                        let _ = tauri::async_runtime::block_on(stop_server_inner(&state));
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            if !first_run {
                let state = handle.state::<DashboardState>();
                let _ = tauri::async_runtime::block_on(start_server_inner(&state));
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard,
            save_settings,
            start_server,
            stop_server,
            restart_server,
            configure_firewall,
            open_data_directory,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Tome Server dashboard");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_settings_round_trip() {
        let settings = DashboardSettings {
            port: 7444,
            lan_enabled: true,
            tailscale_enabled: false,
            firewall_enabled: true,
            launch_at_login: true,
            data_directory: "data".into(),
        };
        let encoded = serde_json::to_vec(&settings).unwrap();
        assert_eq!(
            serde_json::from_slice::<DashboardSettings>(&encoded).unwrap(),
            settings
        );
    }

    #[test]
    fn tailscale_probe_distinguishes_login_and_address_states() {
        assert_eq!(
            tailscale_state_from_probe(false, true, br#"{"BackendState":"NeedsLogin"}"#),
            TailscaleState::NotSignedIn
        );
        assert_eq!(
            tailscale_state_from_probe(false, true, br#"{"BackendState":"Running"}"#),
            TailscaleState::NoAddress
        );
        assert_eq!(
            tailscale_state_from_probe(
                true,
                true,
                br#"{"BackendState":"Running","Self":{"DNSName":"studio.tailnet.ts.net."}}"#
            ),
            TailscaleState::Connected {
                hostname: Some("studio.tailnet.ts.net".to_owned())
            }
        );
    }
}
