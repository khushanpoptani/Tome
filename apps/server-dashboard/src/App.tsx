import { isTauri, invoke } from '@tauri-apps/api/core';
import { FormEvent, useCallback, useEffect, useMemo, useState } from 'react';
import {
  AddressView,
  OllamaState,
  TailscaleState,
  groupAddresses,
  ollamaPresentation,
  tailscaleMessage,
} from './dashboard';
import {
  formatBytes,
  loadLocalModels,
  localDefault,
  localDelete,
  localDownload,
  localJobAction,
  localModelAction,
  type LocalModelSnapshot,
} from './models';

interface Settings {
  port: number;
  lan_enabled: boolean;
  tailscale_enabled: boolean;
  firewall_enabled: boolean;
  launch_at_login: boolean;
  data_directory: string;
}

interface LogEntry {
  timestamp: string;
  level: 'info' | 'error';
  message: string;
}

interface DashboardSnapshot {
  status: 'stopped' | 'starting' | 'running' | 'error';
  status_message: string;
  settings: Settings;
  first_run: boolean;
  addresses: AddressView[];
  tailscale: TailscaleState;
  ollama: OllamaState;
  firewall_status: string;
  network_change_pending: boolean;
  data_directory: string;
  version: string;
  protocol_version: number;
  logs: LogEntry[];
  diagnostics: string;
}

const preview: DashboardSnapshot = {
  status: 'running',
  status_message: 'Listening on 3 trusted addresses.',
  settings: {
    port: 7331,
    lan_enabled: true,
    tailscale_enabled: true,
    firewall_enabled: true,
    launch_at_login: false,
    data_directory: 'C:\\Users\\you\\AppData\\Local\\Tome Server\\data',
  },
  first_run: false,
  addresses: [
    {
      interface_name: 'Loopback',
      address: '127.0.0.1',
      kind: 'loopback',
      url: 'http://127.0.0.1:7331',
      active: true,
    },
    {
      interface_name: 'Ethernet',
      address: '192.168.1.42',
      kind: 'lan',
      url: 'http://192.168.1.42:7331',
      active: true,
    },
    {
      interface_name: 'Tailscale',
      address: '100.104.20.16',
      kind: 'tailscale',
      url: 'http://100.104.20.16:7331',
      active: true,
    },
  ],
  tailscale: { state: 'connected', hostname: 'studio.tailnet.ts.net' },
  ollama: {
    state: 'ready',
    version: '0.12.3',
    capability: 'version_api',
  },
  firewall_status: 'Configured',
  network_change_pending: false,
  data_directory: 'C:\\Users\\you\\AppData\\Local\\Tome Server\\data',
  version: '0.1.0',
  protocol_version: 1,
  logs: [
    {
      timestamp: new Date().toISOString(),
      level: 'info',
      message: 'Server running on port 7331.',
    },
  ],
  diagnostics: 'Tome Server preview diagnostics',
};

async function command<T>(name: string, args?: Record<string, unknown>) {
  if (!isTauri()) return preview as T;
  return invoke<T>(name, args);
}

function AddressGroup({
  title,
  description,
  addresses,
}: {
  title: string;
  description: string;
  addresses: AddressView[];
}) {
  return (
    <section className="address-group">
      <div className="address-heading">
        <div>
          <h3>{title}</h3>
          <p>{description}</p>
        </div>
        <span className="count">{addresses.length}</span>
      </div>
      {addresses.length ? (
        addresses.map((address) => (
          <div
            className="address-row"
            key={`${address.kind}-${address.address}`}
          >
            <span className={`active-dot ${address.active ? 'on' : ''}`} />
            <div>
              <strong>{address.url}</strong>
              <small>{address.interface_name}</small>
            </div>
            <button
              className="quiet-button"
              type="button"
              onClick={() => void navigator.clipboard.writeText(address.url)}
            >
              Copy
            </button>
          </div>
        ))
      ) : (
        <div className="empty-row">No eligible address detected.</div>
      )}
    </section>
  );
}

function Toggle({
  checked,
  title,
  detail,
  onChange,
}: {
  checked: boolean;
  title: string;
  detail: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="toggle-row">
      <input
        checked={checked}
        type="checkbox"
        onChange={(event) => onChange(event.target.checked)}
      />
      <span>
        <strong>{title}</strong>
        <small>{detail}</small>
      </span>
    </label>
  );
}

function OllamaSetup({
  ollama,
  busy,
  compact = false,
  onRun,
}: {
  ollama: OllamaState;
  busy: boolean;
  compact?: boolean;
  onRun: (command: string) => void;
}) {
  const presentation = ollamaPresentation(ollama);
  const action = presentation.action;
  const commandName =
    action === 'start'
      ? 'start_ollama'
      : action === 'retry'
        ? 'get_dashboard'
        : 'open_ollama_download_page';
  const actionLabel =
    action === 'start'
      ? 'Start Ollama'
      : action === 'retry'
        ? 'Check again'
        : 'Get Ollama from official site';
  return (
    <section className={`ollama-card ${compact ? 'compact' : ''}`}>
      <div className="ollama-heading">
        <div>
          <span className="eyebrow">LOCAL AI RUNTIME</span>
          <h2>{presentation.title}</h2>
        </div>
        <span className={`runtime-state ${presentation.tone}`}>
          {presentation.tone === 'ready' ? 'Ready' : 'Action needed'}
        </span>
      </div>
      <p>{presentation.detail}</p>
      <div className="loopback-note">
        <strong>Private by design</strong>
        <span>
          Tome contacts Ollama only at <code>127.0.0.1:11434</code>. Ollama is
          never exposed to LAN or Tailscale devices.
        </span>
      </div>
      {action && (
        <button
          className="primary-button"
          disabled={busy}
          onClick={() => onRun(commandName)}
          type="button"
        >
          {actionLabel}
        </button>
      )}
      {action === 'install' && (
        <small className="consent-note">
          Opens Ollama’s official Windows download page. You choose whether to
          download and run its normal installer; Tome never runs a remote script
          or installs it silently.
        </small>
      )}
    </section>
  );
}

export function App() {
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const refresh = useCallback(async () => {
    try {
      const next = await command<DashboardSnapshot>('get_dashboard');
      setSnapshot(next);
      setDraft((current) => current ?? next.settings);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const run = async (name: string, args?: Record<string, unknown>) => {
    setBusy(true);
    setError('');
    try {
      setSnapshot(await command<DashboardSnapshot>(name, args));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!draft) return;
    setBusy(true);
    setError('');
    try {
      let next = await command<DashboardSnapshot>('save_settings', {
        settings: draft,
      });
      if (
        snapshot?.first_run ||
        draft.firewall_enabled !== snapshot?.settings.firewall_enabled
      )
        next = await command<DashboardSnapshot>('configure_firewall', {
          enabled: draft.firewall_enabled,
        });
      next = await command<DashboardSnapshot>(
        snapshot?.first_run ? 'start_server' : 'restart_server',
      );
      setSnapshot(next);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const groups = useMemo(
    () => groupAddresses(snapshot?.addresses ?? []),
    [snapshot?.addresses],
  );
  if (!snapshot || !draft)
    return <main className="loading">Opening Tome Server…</main>;

  return (
    <main>
      <header className="topbar">
        <div className="brand">
          <div className="brand-mark">T</div>
          <div>
            <h1>Tome Server</h1>
            <p>Private job service for your trusted devices</p>
          </div>
        </div>
        <div className={`status-pill ${snapshot.status}`}>
          <span /> {snapshot.status}
        </div>
      </header>
      <div className="shell">
        {error && <div className="error-banner">{error}</div>}
        {snapshot.network_change_pending && (
          <div className="notice">
            Your network addresses changed. Restart to apply the trusted address
            list.
            <button onClick={() => void run('restart_server')} type="button">
              Restart now
            </button>
          </div>
        )}
        <section className="hero-card">
          <div>
            <span className="eyebrow">SERVER STATUS</span>
            <h2>{snapshot.status_message}</h2>
            <p>
              Port {snapshot.settings.port} · Firewall{' '}
              {snapshot.firewall_status.toLowerCase()}
            </p>
          </div>
          <div className="hero-actions">
            {snapshot.status === 'running' ? (
              <button
                className="danger-button"
                disabled={busy}
                onClick={() => void run('stop_server')}
                type="button"
              >
                Stop server
              </button>
            ) : (
              <button
                className="primary-button"
                disabled={busy || snapshot.first_run}
                onClick={() => void run('start_server')}
                type="button"
              >
                Start server
              </button>
            )}
            <button
              className="secondary-button"
              disabled={busy || snapshot.first_run}
              onClick={() => void run('restart_server')}
              type="button"
            >
              Restart
            </button>
          </div>
        </section>
        <div className="content-grid">
          <div className="main-column">
            <OllamaSetup
              busy={busy}
              ollama={snapshot.ollama}
              onRun={(name) => void run(name)}
            />
            <ModelSetup
              port={snapshot.settings.port}
              running={snapshot.status === 'running'}
            />
            <section className="panel">
              <div className="panel-heading">
                <div>
                  <span className="eyebrow">CONNECTIONS</span>
                  <h2>Available addresses</h2>
                </div>
                <span className="live-key">
                  <span className="active-dot on" /> actively listening
                </span>
              </div>
              <AddressGroup
                title="This PC"
                description="Only programs running on this computer"
                addresses={groups.loopback}
              />
              <AddressGroup
                title="Private LAN"
                description="Devices on your private home or office network"
                addresses={groups.lan}
              />
              <AddressGroup
                title="Tailscale"
                description={tailscaleMessage(snapshot.tailscale)}
                addresses={groups.tailscale}
              />
            </section>
            <section className="panel">
              <div className="panel-heading">
                <div>
                  <span className="eyebrow">ACTIVITY</span>
                  <h2>Recent server events</h2>
                </div>
                <button
                  className="quiet-button"
                  type="button"
                  onClick={() =>
                    void navigator.clipboard.writeText(snapshot.diagnostics)
                  }
                >
                  Copy diagnostics
                </button>
              </div>
              <div className="logs">
                {[...snapshot.logs].reverse().map((entry, index) => (
                  <div className="log-row" key={`${entry.timestamp}-${index}`}>
                    <time>
                      {new Date(entry.timestamp).toLocaleTimeString()}
                    </time>
                    <span className={entry.level}>{entry.level}</span>
                    <p>{entry.message}</p>
                  </div>
                ))}
              </div>
            </section>
          </div>
          <aside className="side-column">
            <form className="panel settings-panel" onSubmit={save}>
              <span className="eyebrow">SETTINGS</span>
              <h2>Network access</h2>
              <label className="field">
                <span>Port</span>
                <input
                  min="1"
                  max="65535"
                  type="number"
                  value={draft.port}
                  onChange={(event) =>
                    setDraft({ ...draft, port: Number(event.target.value) })
                  }
                />
              </label>
              <Toggle
                checked={draft.lan_enabled}
                title="Private LAN"
                detail="Listen on private adapter addresses"
                onChange={(checked) =>
                  setDraft({ ...draft, lan_enabled: checked })
                }
              />
              <Toggle
                checked={draft.tailscale_enabled}
                title="Tailscale"
                detail="Requires Tailscale installed and signed in"
                onChange={(checked) =>
                  setDraft({ ...draft, tailscale_enabled: checked })
                }
              />
              <Toggle
                checked={draft.firewall_enabled}
                title="Windows Firewall rules"
                detail="Scoped to LocalSubnet and Tailscale CGNAT"
                onChange={(checked) =>
                  setDraft({ ...draft, firewall_enabled: checked })
                }
              />
              <Toggle
                checked={draft.launch_at_login}
                title="Launch at login"
                detail="Keep the service available in the tray"
                onChange={(checked) =>
                  setDraft({ ...draft, launch_at_login: checked })
                }
              />
              <label className="field">
                <span>Data directory</span>
                <input
                  value={draft.data_directory}
                  onChange={(event) =>
                    setDraft({ ...draft, data_directory: event.target.value })
                  }
                />
              </label>
              <button
                className="primary-button wide"
                disabled={busy}
                type="submit"
              >
                {snapshot.first_run ? 'Save and start' : 'Save settings'}
              </button>
              <button
                className="quiet-button wide"
                type="button"
                onClick={() => void command('open_data_directory')}
              >
                Open data directory
              </button>
              {!snapshot.first_run && (
                <button
                  className="quiet-button wide"
                  type="button"
                  onClick={() =>
                    void run('configure_firewall', {
                      enabled: !snapshot.settings.firewall_enabled,
                    })
                  }
                >
                  {snapshot.settings.firewall_enabled
                    ? 'Remove firewall rules'
                    : 'Configure firewall rules'}
                </button>
              )}
            </form>
            <div className="version-card">
              <strong>Tome Server {snapshot.version}</strong>
              <span>Protocol {snapshot.protocol_version}</span>
            </div>
          </aside>
        </div>
      </div>
      {snapshot.first_run && (
        <div className="setup-backdrop">
          <form className="setup-card" onSubmit={save}>
            <span className="eyebrow">WELCOME TO TOME SERVER</span>
            <h2>Choose where your devices can connect</h2>
            <p>
              Tome binds only to the specific private addresses you enable. It
              never opens a wildcard or public listener.
            </p>
            <OllamaSetup
              busy={busy}
              compact
              ollama={snapshot.ollama}
              onRun={(name) => void run(name)}
            />
            <div className="setup-options">
              <Toggle
                checked={draft.lan_enabled}
                title="Private LAN"
                detail="For trusted devices on this network"
                onChange={(checked) =>
                  setDraft({ ...draft, lan_enabled: checked })
                }
              />
              <Toggle
                checked={draft.tailscale_enabled}
                title="Tailscale"
                detail="Install Tailscale and sign in before using this address"
                onChange={(checked) =>
                  setDraft({ ...draft, tailscale_enabled: checked })
                }
              />
              <Toggle
                checked={draft.firewall_enabled}
                title="Configure Windows Firewall"
                detail="Windows will ask for administrator approval"
                onChange={(checked) =>
                  setDraft({ ...draft, firewall_enabled: checked })
                }
              />
              <Toggle
                checked={draft.launch_at_login}
                title="Launch at login"
                detail="Start quietly and remain available from the system tray"
                onChange={(checked) =>
                  setDraft({ ...draft, launch_at_login: checked })
                }
              />
            </div>
            <label className="field inline-field">
              <span>Server port</span>
              <input
                min="1"
                max="65535"
                type="number"
                value={draft.port}
                onChange={(event) =>
                  setDraft({ ...draft, port: Number(event.target.value) })
                }
              />
            </label>
            <label className="field">
              <span>Data directory</span>
              <input
                value={draft.data_directory}
                onChange={(event) =>
                  setDraft({ ...draft, data_directory: event.target.value })
                }
              />
            </label>
            {error && <div className="error-banner">{error}</div>}
            <button
              className="primary-button wide"
              disabled={busy}
              type="submit"
            >
              {busy ? 'Setting up…' : 'Save and start Tome Server'}
            </button>
          </form>
        </div>
      )}
    </main>
  );
}

function ModelSetup({ port, running }: { port: number; running: boolean }) {
  const [models, setModels] = useState<LocalModelSnapshot | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState('');
  const refresh = useCallback(async () => {
    if (!running) return;
    try {
      setModels(await loadLocalModels(port));
      setError('');
    } catch (reason) {
      setError(String(reason));
    }
  }, [port, running]);
  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 4000);
    return () => window.clearInterval(timer);
  }, [refresh]);
  const action = async (key: string, work: () => Promise<unknown>) => {
    setBusy(key);
    setError('');
    try {
      await work();
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy('');
    }
  };
  if (!running)
    return (
      <section className="panel model-panel">
        <span className="eyebrow">MODELS</span>
        <h2>Start the server to manage models</h2>
      </section>
    );
  if (!models)
    return (
      <section className="panel model-panel">
        <span className="eyebrow">MODELS</span>
        <h2>Inspecting hardware and model storage…</h2>
        {error && <div className="error-banner">{error}</div>}
      </section>
    );
  const active = models.jobs.filter((job) =>
    ['queued', 'running', 'interrupted', 'failed'].includes(job.state),
  );
  return (
    <section className="panel model-panel">
      <div className="panel-heading">
        <div>
          <span className="eyebrow">MODEL SETUP</span>
          <h2>
            {models.inventory.ready
              ? 'Server model-ready'
              : 'Install your first text model'}
          </h2>
        </div>
        <span className={`readiness ${models.inventory.ready ? 'ready' : ''}`}>
          {models.inventory.ready ? 'Ready' : 'Setup needed'}
        </span>
      </div>
      {error && <div className="error-banner">{error}</div>}
      <div className="hardware-summary">
        <ModelMetric
          label="CPU / RAM"
          value={`${models.hardware.cpu_brand ?? models.hardware.cpu_architecture} · ${formatBytes(models.hardware.total_memory_bytes)}`}
        />
        <ModelMetric
          label="GPU"
          value={models.hardware.gpu.name ?? 'Unknown / CPU'}
          detail={models.hardware.gpu.memory_note}
        />
        <ModelMetric
          label="Storage"
          value={`${formatBytes(models.hardware.model_storage.free_bytes)} free`}
          detail={models.hardware.model_storage.root}
        />
        <ModelMetric
          label="Runtime"
          value={
            models.hardware.runtime.llama_cpp.available
              ? 'llama.cpp available'
              : 'llama.cpp unavailable'
          }
          detail={models.hardware.runtime.llama_cpp.reason}
        />
      </div>
      {!models.inventory.ready && (
        <div className="profile-list">
          {models.setup.profiles
            .filter((profile) => profile.id !== 'manual')
            .map((profile) => (
              <button
                key={profile.id}
                className="profile-card"
                disabled={!profile.compatible || busy !== ''}
                title={profile.exclusion_reasons.join(' ')}
                onClick={() =>
                  void action(profile.id, async () => {
                    for (const id of profile.entry_ids)
                      await localDownload(port, id);
                  })
                }
              >
                <strong>{profile.name}</strong>
                <span>{formatBytes(profile.download_bytes)}</span>
                <small>
                  {profile.compatible
                    ? 'Compatible with detected server'
                    : profile.exclusion_reasons.join(' ')}
                </small>
              </button>
            ))}
        </div>
      )}
      {active.map((job) => (
        <div className="model-download" key={job.id}>
          <div>
            <strong>{job.input.catalog_id}</strong>
            <small>
              {job.state} · {Math.round(job.progress * 100)}%
            </small>
          </div>
          <progress value={job.progress} max={1} />
          <div>
            {job.state === 'interrupted' ? (
              <button
                className="quiet-button"
                onClick={() =>
                  void action(job.id, () =>
                    localJobAction(port, job.id, 'resume'),
                  )
                }
              >
                Resume
              </button>
            ) : (
              <button
                className="quiet-button"
                onClick={() =>
                  void action(job.id, () =>
                    localJobAction(port, job.id, 'pause'),
                  )
                }
              >
                Pause
              </button>
            )}{' '}
            {['queued', 'running'].includes(job.state) && (
              <button
                className="danger-button"
                onClick={() =>
                  void action(job.id, () =>
                    localJobAction(port, job.id, 'cancel'),
                  )
                }
              >
                Cancel
              </button>
            )}
          </div>
        </div>
      ))}
      <div className="installed-list">
        {models.inventory.models.map((model) => (
          <div className="installed-model" key={model.id}>
            <div>
              <strong>{model.display_name}</strong>
              <small>
                {model.quantization} · {formatBytes(model.byte_size)} ·{' '}
                {model.compatibility_state}
              </small>
              <p>{model.compatibility_reason}</p>
            </div>
            <div className="model-buttons">
              <button
                className="quiet-button"
                disabled={models.inventory.default_model_id === model.id}
                onClick={() =>
                  void action(model.id, () => localDefault(port, model.id))
                }
              >
                {models.inventory.default_model_id === model.id
                  ? 'Default'
                  : 'Set default'}
              </button>
              <button
                className="primary-button"
                disabled={model.compatibility_state !== 'compatible'}
                onClick={() =>
                  void action(model.id, () =>
                    localModelAction(
                      port,
                      model.id,
                      model.loaded ? 'unload' : 'load',
                    ),
                  )
                }
              >
                {model.loaded ? 'Unload' : 'Load'}
              </button>
              <button
                className="danger-button"
                disabled={model.loaded || model.in_use_count > 0}
                onClick={() => {
                  if (
                    window.confirm(
                      `Move ${model.display_name} to recoverable trash?`,
                    )
                  )
                    void action(model.id, () => localDelete(port, model.id));
                }}
              >
                Delete
              </button>
            </div>
          </div>
        ))}
      </div>
      <details className="catalog-details">
        <summary>Approved catalog · {models.catalog.catalog_version}</summary>
        <div className="dashboard-catalog">
          {models.catalog.entries.map((entry) => {
            const installed = models.inventory.models.some(
              (model) => model.catalog_id === entry.id,
            );
            return (
              <article key={entry.id}>
                <span>{entry.roles.join(' · ')}</span>
                <strong>{entry.name}</strong>
                <p>{entry.reason}</p>
                <small>
                  {entry.runtime_status.replace('_', ' ')} · {entry.license} ·{' '}
                  {formatBytes(entry.bytes)}
                </small>
                <button
                  className="primary-button"
                  disabled={installed || busy !== ''}
                  onClick={() =>
                    void action(entry.id, () => localDownload(port, entry.id))
                  }
                >
                  {installed ? 'Installed' : 'Accept license & download'}
                </button>
              </article>
            );
          })}
        </div>
      </details>
    </section>
  );
}

function ModelMetric({
  label,
  value,
  detail,
}: {
  label: string;
  value: string;
  detail?: string;
}) {
  return (
    <div className="model-metric">
      <span>{label}</span>
      <strong>{value}</strong>
      {detail && <small>{detail}</small>}
    </div>
  );
}
