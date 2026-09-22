import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type FormEvent,
} from 'react';
import {
  connectToServer,
  loadProfile,
  saveProfile,
  type ConnectionProfile,
  type ConnectionState,
} from './connection';
import {
  deleteModel,
  exportInventory,
  formatBytes,
  jobAction,
  loadModelSnapshot,
  modelAction,
  retryJob,
  setDefaultModel,
  startDownload,
  type ModelSnapshot,
} from './model-management';

const defaultProfile: ConnectionProfile = {
  name: 'My Tome server',
  mode: 'lan',
  host: '127.0.0.1',
  port: 7331,
};

export function App() {
  const [profile, setProfile] = useState<ConnectionProfile>(
    () => loadProfile() ?? defaultProfile,
  );
  const [connection, setConnection] = useState<ConnectionState>({
    status: 'disconnected',
  });
  const [saved, setSaved] = useState(false);

  function update<K extends keyof ConnectionProfile>(
    key: K,
    value: ConnectionProfile[K],
  ) {
    setSaved(false);
    setProfile((current) => ({ ...current, [key]: value }));
  }

  function handleSave() {
    try {
      saveProfile(profile);
      setSaved(true);
      if (connection.status === 'error')
        setConnection({ status: 'disconnected' });
    } catch (error) {
      setConnection({ status: 'error', message: (error as Error).message });
    }
  }

  async function handleTest(event: FormEvent) {
    event.preventDefault();
    const result = await connectToServer(profile, { onState: setConnection });
    if (result.status === 'connected') {
      saveProfile(profile);
      setSaved(true);
    }
  }

  return (
    <main
      className={`shell ${connection.status === 'connected' ? 'connected-shell' : ''}`}
    >
      <section className="connection-card" aria-labelledby="connection-heading">
        <header>
          <div>
            <p className="eyebrow">Private server connection</p>
            <h1 id="connection-heading">Connect to Tome</h1>
          </div>
          <Status state={connection} />
        </header>

        <p className="intro">
          Connect over your private LAN or Tailscale network. This phase has no
          app-level login, so never expose the server directly to the public
          internet.
        </p>

        <form onSubmit={(event) => void handleTest(event)}>
          <label>
            Connection name
            <input
              value={profile.name}
              onChange={(event) => update('name', event.target.value)}
              autoComplete="off"
            />
          </label>

          <fieldset>
            <legend>Network</legend>
            <label className="radio">
              <input
                type="radio"
                name="mode"
                checked={profile.mode === 'lan'}
                onChange={() => update('mode', 'lan')}
              />
              LAN
            </label>
            <label className="radio">
              <input
                type="radio"
                name="mode"
                checked={profile.mode === 'tailscale'}
                onChange={() => update('mode', 'tailscale')}
              />
              Tailscale
            </label>
          </fieldset>

          <div className="address-row">
            <label>
              Host or IP
              <input
                value={profile.host}
                onChange={(event) => update('host', event.target.value)}
                placeholder={
                  profile.mode === 'tailscale'
                    ? '100.x.y.z or MagicDNS name'
                    : '192.168.x.y'
                }
                spellCheck={false}
              />
            </label>
            <label className="port-field">
              Port
              <input
                type="number"
                min="1"
                max="65535"
                value={profile.port}
                onChange={(event) => update('port', Number(event.target.value))}
              />
            </label>
          </div>

          {connection.status === 'error' && (
            <p className="error-message">{connection.message}</p>
          )}
          {connection.status === 'connected' && (
            <div className="capabilities">
              <span>Protocol {connection.capabilities.protocol.current}</span>
              <span>Persistent jobs</span>
              <span>Event replay</span>
            </div>
          )}

          <div className="actions">
            <button type="button" className="secondary" onClick={handleSave}>
              {saved ? 'Saved' : 'Save connection'}
            </button>
            <button type="submit" disabled={connection.status === 'connecting'}>
              {connection.status === 'connecting'
                ? 'Testing…'
                : 'Test connection'}
            </button>
          </div>
        </form>

        <footer>
          Only this connection bootstrap profile and the model event cursor are
          stored. Chat and product data remain out of scope in Phase 2.
        </footer>
      </section>
      {connection.status === 'connected' && (
        <ModelManagement profile={profile} />
      )}
    </main>
  );
}

function ModelManagement({ profile }: { profile: ConnectionProfile }) {
  const [snapshot, setSnapshot] = useState<ModelSnapshot | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState('');
  const [search, setSearch] = useState('');
  const [live, setLive] = useState<'connecting' | 'live' | 'offline'>(
    'connecting',
  );

  const refresh = useCallback(async () => {
    try {
      setSnapshot(await loadModelSnapshot(profile));
      setError('');
    } catch (reason) {
      setError(String(reason));
    }
  }, [profile]);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5000);
    const cursorKey = `tome.model-events.${profile.host}:${profile.port}`;
    const host = profile.host.trim();
    const formattedHost =
      host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
    const base = `ws://${formattedHost}:${profile.port}`;
    let stopped = false;
    let socket: WebSocket | null = null;
    let reconnectTimer: number | undefined;
    const connectEvents = () => {
      if (stopped) return;
      setLive('connecting');
      const after = localStorage.getItem(cursorKey) ?? '0';
      socket = new WebSocket(
        `${base}/api/v1/events/ws?after_event_id=${after}`,
      );
      socket.onopen = () => setLive('live');
      socket.onmessage = (message) => {
        try {
          const event = JSON.parse(String(message.data)) as {
            event_id?: number;
          };
          if (typeof event.event_id === 'number')
            localStorage.setItem(cursorKey, String(event.event_id));
        } finally {
          void refresh();
        }
      };
      socket.onerror = () => setLive('offline');
      socket.onclose = () => {
        setLive('offline');
        if (!stopped) reconnectTimer = window.setTimeout(connectEvents, 1500);
      };
    };
    connectEvents();
    return () => {
      stopped = true;
      window.clearInterval(timer);
      if (reconnectTimer !== undefined) window.clearTimeout(reconnectTimer);
      socket?.close();
    };
  }, [profile, refresh]);

  const run = async (key: string, operation: () => Promise<unknown>) => {
    setBusy(key);
    setError('');
    try {
      await operation();
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy('');
    }
  };

  const entries = useMemo(() => {
    const needle = search.trim().toLowerCase();
    if (!snapshot || !needle) return snapshot?.catalog.entries ?? [];
    return snapshot.catalog.entries.filter((entry) =>
      [
        entry.name,
        entry.repository,
        entry.roles.join(' '),
        entry.capabilities.join(' '),
      ]
        .join(' ')
        .toLowerCase()
        .includes(needle),
    );
  }, [search, snapshot]);

  if (!snapshot)
    return (
      <section className="model-workspace loading-panel">
        {error || 'Loading server hardware and models…'}
      </section>
    );

  const activeJobs = snapshot.jobs.filter((job) =>
    ['queued', 'running', 'interrupted', 'failed'].includes(job.state),
  );
  return (
    <section className="model-workspace" aria-labelledby="models-heading">
      <header className="workspace-header">
        <div>
          <p className="eyebrow">Phase 2 · Model management</p>
          <h2 id="models-heading">Server models</h2>
          <p>
            {snapshot.inventory.ready
              ? 'The server has a verified compatible text model.'
              : 'First-run setup needs a verified compatible text model.'}
          </p>
        </div>
        <div className="workspace-actions">
          <span className={`live-state ${live}`}>{live}</span>
          <button
            className="secondary"
            onClick={() => void exportInventory(profile)}
          >
            Export inventory
          </button>
        </div>
      </header>
      {error && <p className="error-message">{error}</p>}
      <div className="hardware-grid">
        <Metric
          label="CPU"
          value={
            snapshot.hardware.cpu_brand ?? snapshot.hardware.cpu_architecture
          }
        />
        <Metric
          label="System memory"
          value={`${formatBytes(snapshot.hardware.available_memory_bytes)} free / ${formatBytes(snapshot.hardware.total_memory_bytes)}`}
        />
        <Metric
          label="GPU"
          value={snapshot.hardware.gpu.name ?? 'Unknown / CPU fallback'}
          detail={snapshot.hardware.gpu.memory_note}
        />
        <Metric
          label="Model storage"
          value={`${formatBytes(snapshot.hardware.model_storage.free_bytes)} free`}
          detail={snapshot.hardware.model_storage.root}
        />
        <Metric
          label="llama.cpp"
          value={
            snapshot.hardware.runtime.llama_cpp.available
              ? 'Available'
              : 'Unavailable'
          }
          detail={snapshot.hardware.runtime.llama_cpp.reason}
        />
      </div>

      {!snapshot.inventory.ready && (
        <div className="setup-strip">
          <div>
            <strong>Finish first-run model setup</strong>
            <p>Only profiles compatible with this server can be started.</p>
          </div>
          <div className="profile-buttons">
            {snapshot.setup.profiles
              .filter((candidate) => candidate.id !== 'manual')
              .map((candidate) => (
                <button
                  key={candidate.id}
                  disabled={!candidate.compatible || busy !== ''}
                  title={candidate.exclusion_reasons.join(' ')}
                  onClick={() =>
                    void run(`profile-${candidate.id}`, async () => {
                      for (const id of candidate.entry_ids)
                        await startDownload(profile, id);
                    })
                  }
                >
                  {candidate.name} · {formatBytes(candidate.download_bytes)}
                </button>
              ))}
          </div>
        </div>
      )}

      {activeJobs.length > 0 && (
        <section className="model-section">
          <h3>Downloads</h3>
          {activeJobs.map((job) => (
            <div className="download-row" key={job.id}>
              <div>
                <strong>{job.input.catalog_id ?? 'Model artifact'}</strong>
                <small>
                  {job.state} · {Math.round(job.progress * 100)}%
                </small>
              </div>
              <progress value={job.progress} max={1} />
              <div className="row-actions">
                {job.state === 'running' || job.state === 'queued' ? (
                  <button
                    className="secondary"
                    onClick={() =>
                      void run(job.id, () =>
                        jobAction(profile, job.id, 'pause'),
                      )
                    }
                  >
                    Pause
                  </button>
                ) : job.state === 'interrupted' ? (
                  <button
                    className="secondary"
                    onClick={() =>
                      void run(job.id, () =>
                        jobAction(profile, job.id, 'resume'),
                      )
                    }
                  >
                    Resume
                  </button>
                ) : (
                  <button
                    className="secondary"
                    onClick={() =>
                      void run(job.id, () => retryJob(profile, job.id))
                    }
                  >
                    Retry
                  </button>
                )}
                {['queued', 'running'].includes(job.state) && (
                  <button
                    className="danger"
                    onClick={() =>
                      void run(job.id, () =>
                        jobAction(profile, job.id, 'cancel'),
                      )
                    }
                  >
                    Cancel
                  </button>
                )}
              </div>
            </div>
          ))}
        </section>
      )}

      <section className="model-section">
        <h3>Installed</h3>
        {snapshot.inventory.models.length === 0 ? (
          <p className="empty-message">No verified models are installed yet.</p>
        ) : (
          snapshot.inventory.models.map((model) => (
            <article className="model-row" key={model.id}>
              <div>
                <strong>{model.display_name}</strong>
                <small>
                  {model.quantization} · {formatBytes(model.byte_size)} ·{' '}
                  {model.verification_state}
                </small>
                <p>{model.compatibility_reason}</p>
              </div>
              <div className="row-actions">
                <button
                  className="secondary"
                  disabled={snapshot.inventory.default_model_id === model.id}
                  onClick={() =>
                    void run(model.id, () => setDefaultModel(profile, model.id))
                  }
                >
                  {snapshot.inventory.default_model_id === model.id
                    ? 'Default'
                    : 'Set default'}
                </button>
                <button
                  disabled={model.compatibility_state !== 'compatible'}
                  onClick={() =>
                    void run(model.id, () =>
                      modelAction(
                        profile,
                        model.id,
                        model.loaded ? 'unload' : 'load',
                      ),
                    )
                  }
                >
                  {model.loaded ? 'Unload' : 'Load'}
                </button>
                <button
                  className="danger"
                  disabled={model.loaded || model.in_use_count > 0}
                  onClick={() => {
                    if (
                      window.confirm(
                        `Move ${model.display_name} to Tome's recoverable model trash?`,
                      )
                    )
                      void run(model.id, () => deleteModel(profile, model.id));
                  }}
                >
                  Delete
                </button>
              </div>
            </article>
          ))
        )}
      </section>

      <section className="model-section">
        <div className="catalog-heading">
          <div>
            <h3>Approved catalog</h3>
            <small>Version {snapshot.catalog.catalog_version}</small>
          </div>
          <input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Search models or capabilities"
          />
        </div>
        <div className="catalog-grid">
          {entries.map((entry) => {
            const installed = snapshot.inventory.models.some(
              (model) => model.catalog_id === entry.id,
            );
            return (
              <article className="catalog-card" key={entry.id}>
                <div className="tag-row">
                  {entry.roles.map((role) => (
                    <span key={role}>{role.replaceAll('_', ' ')}</span>
                  ))}
                </div>
                <h4>{entry.name}</h4>
                <p>{entry.reason}</p>
                <dl>
                  <div>
                    <dt>Artifact</dt>
                    <dd>
                      {entry.format} · {entry.quantization}
                    </dd>
                  </div>
                  <div>
                    <dt>Download</dt>
                    <dd>{formatBytes(entry.bytes)}</dd>
                  </div>
                  <div>
                    <dt>License</dt>
                    <dd>{entry.license}</dd>
                  </div>
                  <div>
                    <dt>Runtime</dt>
                    <dd>{entry.runtime_status.replace('_', ' ')}</dd>
                  </div>
                </dl>
                <button
                  disabled={installed || busy !== ''}
                  onClick={() =>
                    void run(entry.id, () => startDownload(profile, entry.id))
                  }
                >
                  {installed ? 'Installed' : 'Accept license & download'}
                </button>
              </article>
            );
          })}
        </div>
      </section>
    </section>
  );
}

function Metric({
  label,
  value,
  detail,
}: {
  label: string;
  value: string;
  detail?: string;
}) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
      {detail && <small>{detail}</small>}
    </div>
  );
}

function Status({ state }: { state: ConnectionState }) {
  const label =
    state.status === 'reconnecting'
      ? `Reconnecting · ${state.attempt}`
      : state.status;
  return <span className={`status status-${state.status}`}>{label}</span>;
}
