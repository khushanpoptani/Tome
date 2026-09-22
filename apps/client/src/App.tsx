import {
  Component,
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ChangeEvent,
  type ErrorInfo,
  type FormEvent,
  type ReactNode,
} from 'react';
import {
  connectToServer,
  type ConnectionProfile,
  type ConnectionState,
} from './connection';
import {
  chatDownloadName,
  newProfile,
  storage,
  type Bootstrap,
  type Chat,
  type ClientSettings,
  type ServerProfile,
} from './client-storage';
import {
  cancelServerJob,
  loadJobs,
  retryServerJob,
  type ServerJob,
} from './jobs';
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

type Screen = 'chats' | 'connections' | 'models' | 'jobs' | 'settings';

export function App() {
  const [data, setData] = useState<Bootstrap | null>(null);
  const [screen, setScreen] = useState<Screen>('chats');
  const [selectedChat, setSelectedChat] = useState<Chat | null>(null);
  const [profile, setProfile] = useState<ServerProfile | null>(null);
  const [connection, setConnection] = useState<ConnectionState>({
    status: 'disconnected',
  });
  const [error, setError] = useState('');
  const refresh = useCallback(async () => {
    try {
      const next = await storage.bootstrap();
      setData(next);
      setSelectedChat((current) =>
        current && next.chats.some(({ id }) => id === current.id)
          ? current
          : null,
      );
      setProfile((current) =>
        current
          ? (next.profiles.find(({ id }) => id === current.id) ?? null)
          : (next.profiles.find(
              ({ id }) => id === next.settings.lastUsedProfileId,
            ) ??
            next.profiles.find(
              ({ id }) => id === next.settings.defaultProfileId,
            ) ??
            next.profiles[0] ??
            null),
      );
      setError('');
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function openChat(id: string) {
    try {
      setSelectedChat(await storage.getChat(id));
      setScreen('chats');
    } catch (reason) {
      setError(String(reason));
    }
  }

  if (!data) {
    return (
      <main className="launch-state" role={error ? 'alert' : 'status'}>
        <p className="eyebrow">Tome · Client-local workspace</p>
        <h1>
          {error ? 'Local data is unavailable' : 'Opening your workspace…'}
        </h1>
        <p>
          {error ||
            'Checking versioned local records and recovering interrupted writes.'}
        </p>
        {error && <button onClick={() => void refresh()}>Try again</button>}
      </main>
    );
  }

  return (
    <div className={`app-shell appearance-${data.settings.appearance}`}>
      <aside className="app-sidebar">
        <div className="brand">
          <span>T</span>
          <strong>Tome</strong>
        </div>
        <nav aria-label="Main navigation">
          {(
            ['chats', 'connections', 'models', 'jobs', 'settings'] as Screen[]
          ).map((item) => (
            <button
              key={item}
              className={screen === item ? 'active' : ''}
              onClick={() => setScreen(item)}
            >
              {item[0].toUpperCase() + item.slice(1)}
            </button>
          ))}
        </nav>
        <div className="connection-summary">
          <Status state={connection} />
          <small>{profile?.name ?? 'No server selected'}</small>
        </div>
      </aside>
      <main className="app-content">
        {data.firstRun && (
          <div className="welcome-banner" role="status">
            <strong>Welcome to Tome.</strong> Your local workspace is ready.
            Create a chat for offline organization or save a private server
            connection.
          </div>
        )}
        {data.diagnostics.length > 0 && (
          <details className="diagnostic-banner">
            <summary>
              <strong>Some local data needs attention.</strong>{' '}
              {data.diagnostics.length} record
              {data.diagnostics.length === 1 ? '' : 's'} isolated; the rest of
              your workspace is available.
            </summary>
            <ul>
              {data.diagnostics.map((diagnostic, index) => (
                <li key={`${diagnostic.code}-${diagnostic.recordId}-${index}`}>
                  <strong>{diagnostic.recordType}</strong>
                  {diagnostic.recordId
                    ? ` ${diagnostic.recordId.slice(0, 8)}`
                    : ''}
                  : {diagnostic.message}
                </li>
              ))}
            </ul>
          </details>
        )}
        {error && (
          <p className="error-message" role="alert">
            {error}
          </p>
        )}
        {screen === 'chats' && (
          <ChatsScreen
            data={data}
            selected={selectedChat}
            profile={profile}
            onOpen={(id) => void openChat(id)}
            onRefresh={refresh}
            onError={setError}
          />
        )}
        {screen === 'connections' && (
          <ConnectionsScreen
            data={data}
            selected={profile}
            connection={connection}
            onConnection={setConnection}
            onSelect={setProfile}
            onRefresh={refresh}
            onError={setError}
          />
        )}
        {screen === 'models' &&
          (profile && connection.status === 'connected' ? (
            <ModelErrorBoundary key={profile.id}>
              <ModelManagement
                profile={profile}
                reconnect={data.settings.connection.reconnect}
                retryDelayMs={data.settings.connection.retryDelayMs}
              />
            </ModelErrorBoundary>
          ) : (
            <OfflinePanel
              title="Server models"
              onConnections={() => setScreen('connections')}
            />
          ))}
        {screen === 'jobs' &&
          (profile && connection.status === 'connected' ? (
            <JobsScreen profile={profile} settings={data.settings} />
          ) : (
            <OfflinePanel
              title="Server jobs"
              onConnections={() => setScreen('connections')}
            />
          ))}
        {screen === 'settings' && (
          <SettingsScreen data={data} onRefresh={refresh} onError={setError} />
        )}
      </main>
    </div>
  );
}

function PageHeader({
  eyebrow,
  title,
  children,
}: {
  eyebrow: string;
  title: string;
  children?: ReactNode;
}) {
  return (
    <header className="page-header">
      <div>
        <p className="eyebrow">{eyebrow}</p>
        <h1>{title}</h1>
      </div>
      {children}
    </header>
  );
}

function ChatsScreen({
  data,
  selected,
  profile,
  onOpen,
  onRefresh,
  onError,
}: {
  data: Bootstrap;
  selected: Chat | null;
  profile: ServerProfile | null;
  onOpen: (id: string) => void;
  onRefresh: () => Promise<void>;
  onError: (message: string) => void;
}) {
  const [query, setQuery] = useState('');
  const [chats, setChats] = useState(data.chats);
  useEffect(() => setChats(data.chats), [data.chats]);
  useEffect(() => {
    const timer = window.setTimeout(() => {
      storage
        .searchChats(query)
        .then(setChats)
        .catch((reason) => onError(String(reason)));
    }, 150);
    return () => window.clearTimeout(timer);
  }, [query, onError]);

  async function create() {
    try {
      const chat = await storage.createChat(
        'Untitled chat',
        profile?.id ?? null,
      );
      await onRefresh();
      onOpen(chat.id);
    } catch (reason) {
      onError(String(reason));
    }
  }

  async function importFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = '';
    if (!file) return;
    try {
      const chat = await storage.importChat(await file.text());
      await onRefresh();
      onOpen(chat.id);
    } catch (reason) {
      onError(`Import failed: ${String(reason)}`);
    }
  }

  async function rename(chat: Chat) {
    const title = window.prompt('Rename this local chat', chat.title)?.trim();
    if (!title) return;
    try {
      await storage.renameChat(chat.id, title);
      await onRefresh();
      onOpen(chat.id);
    } catch (reason) {
      onError(String(reason));
    }
  }

  async function remove(chat: Chat) {
    if (
      !window.confirm(
        `Delete “${chat.title}” and its client-managed attachment references? This cannot be undone.`,
      )
    )
      return;
    try {
      await storage.deleteChat(chat.id);
      await onRefresh();
    } catch (reason) {
      onError(String(reason));
    }
  }

  async function exportOne(chat: Chat) {
    try {
      const text = await storage.exportChat(chat.id);
      const url = URL.createObjectURL(
        new Blob([text], { type: 'application/json' }),
      );
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = chatDownloadName(chat);
      anchor.click();
      URL.revokeObjectURL(url);
    } catch (reason) {
      onError(String(reason));
    }
  }

  return (
    <div className="chat-layout">
      <section className="chat-list-panel" aria-label="Local chats">
        <div className="chat-list-actions">
          <button onClick={() => void create()}>New chat</button>
          <label className="button secondary">
            Import
            <input
              type="file"
              accept=".json,.tome.json,application/json"
              hidden
              onChange={(event) => void importFile(event)}
            />
          </label>
        </div>
        <input
          aria-label="Search local chats"
          placeholder="Search local chats"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
        <p className="storage-label">
          Stored on this Mac · {formatBytes(data.usage.chatBytes)}
        </p>
        <div className="chat-items">
          {chats.map((chat) => (
            <button
              key={chat.id}
              className={selected?.id === chat.id ? 'selected' : ''}
              onClick={() => onOpen(chat.id)}
            >
              <strong>{chat.title}</strong>
              <small>
                {new Date(chat.updatedAt).toLocaleString()} ·{' '}
                {chat.messageCount} messages
              </small>
            </button>
          ))}
          {chats.length === 0 && (
            <p className="empty-message">
              {query
                ? 'No local chats match your search.'
                : 'Create or import your first local chat.'}
            </p>
          )}
        </div>
      </section>
      <section className="conversation-placeholder">
        {selected ? (
          <>
            <PageHeader eyebrow="Client-local chat" title={selected.title}>
              <div className="row-actions">
                <button
                  className="secondary"
                  onClick={() => void exportOne(selected)}
                >
                  Export
                </button>
                <button
                  className="secondary"
                  onClick={() => void rename(selected)}
                >
                  Rename
                </button>
                <button
                  className="danger"
                  onClick={() => void remove(selected)}
                >
                  Delete
                </button>
              </div>
            </PageHeader>
            <div className="phase-placeholder">
              <span>Phase 4</span>
              <h2>Conversation and inference arrive next.</h2>
              <p>
                This phase establishes durable local chat records. There is
                intentionally no prompt composer, send button, or inference
                pipeline yet.
              </p>
              <dl>
                <div>
                  <dt>Server profile</dt>
                  <dd>
                    {data.profiles.find(
                      ({ id }) => id === selected.serverProfileId,
                    )?.name ?? 'Missing or not selected'}
                  </dd>
                </div>
                <div>
                  <dt>Model reference</dt>
                  <dd>{selected.modelId ?? 'Not selected'}</dd>
                </div>
              </dl>
            </div>
          </>
        ) : (
          <div className="phase-placeholder">
            <span>Local workspace</span>
            <h2>Select a chat or begin a new one.</h2>
            <p>
              Chat browsing, search, import, export, and settings stay available
              while every server is offline.
            </p>
          </div>
        )}
      </section>
    </div>
  );
}

function ConnectionsScreen({
  data,
  selected,
  connection,
  onConnection,
  onSelect,
  onRefresh,
  onError,
}: {
  data: Bootstrap;
  selected: ServerProfile | null;
  connection: ConnectionState;
  onConnection: (state: ConnectionState) => void;
  onSelect: (profile: ServerProfile | null) => void;
  onRefresh: () => Promise<void>;
  onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState<ServerProfile>(
    () => selected ?? newProfile(),
  );
  useEffect(() => {
    if (selected) setDraft(selected);
  }, [selected]);
  function update<K extends keyof ServerProfile>(
    key: K,
    value: ServerProfile[K],
  ) {
    setDraft((current) => ({ ...current, [key]: value }));
  }
  async function save() {
    try {
      const saved = await storage.upsertProfile(draft);
      onSelect(saved);
      setDraft(saved);
      await onRefresh();
    } catch (reason) {
      onError(String(reason));
    }
  }
  async function test(event: FormEvent) {
    event.preventDefault();
    try {
      const result = await connectToServer(draft, {
        onState: onConnection,
        maxAttempts: data.settings.connection.reconnect ? 2 : 1,
        retryDelayMs: data.settings.connection.retryDelayMs,
      });
      const checkedAt = new Date().toISOString();
      const saved = await storage.upsertProfile({
        ...draft,
        lastConnectedAt:
          result.status === 'connected' ? checkedAt : draft.lastConnectedAt,
        compatibility: {
          compatible: result.status === 'connected',
          protocol:
            result.status === 'connected'
              ? result.capabilities.protocol.current
              : 0,
          checkedAt,
          diagnostic:
            result.status === 'connected'
              ? null
              : 'message' in result
                ? result.message
                : result.status,
        },
      });
      onSelect(saved);
      await onRefresh();
      if (result.status === 'connected') {
        await storage.saveSettings({
          ...data.settings,
          lastUsedProfileId: saved.id,
        });
        await onRefresh();
      }
    } catch (reason) {
      onError(String(reason));
    }
  }
  async function duplicate() {
    const copy = newProfile({
      ...draft,
      id: crypto.randomUUID(),
      name: `${draft.name} copy`,
      createdAt: '',
      updatedAt: '',
      lastConnectedAt: null,
      lastEventId: 0,
      compatibility: null,
    });
    try {
      const saved = await storage.upsertProfile(copy);
      onSelect(saved);
      setDraft(saved);
      await onRefresh();
    } catch (reason) {
      onError(String(reason));
    }
  }
  async function remove() {
    if (
      !selected ||
      !window.confirm(
        `Delete the saved connection “${selected.name}”? Local chats keep their stable reference and will show the profile as missing.`,
      )
    )
      return;
    try {
      await storage.deleteProfile(selected.id);
      onSelect(null);
      setDraft(newProfile());
      onConnection({ status: 'disconnected' });
      await onRefresh();
    } catch (reason) {
      onError(String(reason));
    }
  }
  return (
    <section className="page-card">
      <PageHeader eyebrow="Private LAN or tailnet" title="Server connections">
        <Status state={connection} />
      </PageHeader>
      <p className="intro">
        Tome has no application-level login. Use only a server on a trusted
        private LAN or Tailscale network; public numeric endpoints are rejected.
      </p>
      <div className="profile-tabs">
        {data.profiles.map((item) => (
          <button
            className={selected?.id === item.id ? 'active' : 'secondary'}
            key={item.id}
            onClick={() => {
              onSelect(item);
              setDraft(item);
              onConnection({ status: 'disconnected' });
            }}
          >
            {item.name}
          </button>
        ))}
        <button
          className="secondary"
          onClick={() => {
            onSelect(null);
            setDraft(newProfile());
          }}
        >
          + New
        </button>
      </div>
      <form onSubmit={(event) => void test(event)}>
        <label>
          Profile name
          <input
            autoFocus
            value={draft.name}
            onChange={(event) => update('name', event.target.value)}
          />
        </label>
        <fieldset>
          <legend>Private network</legend>
          {(['lan', 'tailscale'] as const).map((mode) => (
            <label className="radio" key={mode}>
              <input
                type="radio"
                checked={draft.mode === mode}
                onChange={() => update('mode', mode)}
              />
              {mode === 'lan' ? 'LAN' : 'Tailscale'}
            </label>
          ))}
        </fieldset>
        <div className="address-row">
          <label>
            Host or IP
            <input
              value={draft.host}
              spellCheck={false}
              onChange={(event) => update('host', event.target.value)}
            />
          </label>
          <label>
            Port
            <input
              type="number"
              min="1"
              max="65535"
              value={draft.port}
              onChange={(event) => update('port', Number(event.target.value))}
            />
          </label>
        </div>
        {'message' in connection && (
          <p className="error-message">{connection.message}</p>
        )}
        <div className="actions">
          <button
            type="button"
            className="danger"
            disabled={!selected}
            onClick={() => void remove()}
          >
            Delete
          </button>
          <button
            type="button"
            className="secondary"
            onClick={() => void duplicate()}
          >
            Duplicate
          </button>
          <button
            type="button"
            className="secondary"
            onClick={() => void save()}
          >
            Save
          </button>
          <button type="submit" disabled={connection.status === 'connecting'}>
            Test & connect
          </button>
        </div>
      </form>
    </section>
  );
}

function JobsScreen({
  profile,
  settings,
}: {
  profile: ServerProfile;
  settings: ClientSettings;
}) {
  const [jobs, setJobs] = useState<ServerJob[]>([]);
  const [error, setError] = useState('');
  const [live, setLive] = useState<'connecting' | 'live' | 'offline'>(
    'connecting',
  );
  const refresh = useCallback(async () => {
    try {
      setJobs(await loadJobs(profile));
      setError('');
    } catch (reason) {
      setError(String(reason));
    }
  }, [profile]);
  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5000);
    let stopped = false;
    let socket: WebSocket | null = null;
    let reconnectTimer: number | undefined;
    let cursor = profile.lastEventId;
    const connect = () => {
      if (stopped) return;
      setLive('connecting');
      const host =
        profile.host.includes(':') && !profile.host.startsWith('[')
          ? `[${profile.host}]`
          : profile.host;
      socket = new WebSocket(
        `ws://${host}:${profile.port}/api/v1/events/ws?after_event_id=${cursor}`,
      );
      socket.onopen = () => setLive('live');
      socket.onmessage = (message) => {
        try {
          const event = JSON.parse(String(message.data)) as {
            event_id?: number;
          };
          if (typeof event.event_id === 'number' && event.event_id > cursor) {
            cursor = event.event_id;
            void storage.setEventCursor(profile.id, cursor);
          }
        } finally {
          void refresh();
        }
      };
      socket.onerror = () => setLive('offline');
      socket.onclose = () => {
        setLive('offline');
        if (!stopped && settings.connection.reconnect)
          reconnectTimer = window.setTimeout(
            connect,
            settings.connection.retryDelayMs,
          );
      };
    };
    connect();
    return () => {
      stopped = true;
      window.clearInterval(timer);
      if (reconnectTimer) window.clearTimeout(reconnectTimer);
      socket?.close();
    };
  }, [profile, refresh, settings.connection]);
  async function run(operation: () => Promise<unknown>) {
    try {
      await operation();
      await refresh();
    } catch (reason) {
      setError(String(reason));
    }
  }
  return (
    <section className="page-card">
      <PageHeader eyebrow="Stored on connected server" title="Job history">
        <span className={`live-state ${live}`}>{live}</span>
      </PageHeader>
      <p className="intro">
        The server owns durable job execution and replay. Local chats store only
        optional references; server retention is independent.
      </p>
      {error && <p className="error-message">{error}</p>}
      <div className="job-table">
        {jobs.map((job) => (
          <article key={job.id}>
            <div>
              <strong>{job.job_type.replaceAll('_', ' ')}</strong>
              <small>{job.id}</small>
            </div>
            <div>
              <span className={`job-state ${job.state}`}>{job.state}</span>
              <progress value={job.progress} max={1} />
              <small>{Math.round(job.progress * 100)}%</small>
            </div>
            <div>
              <small>Created {new Date(job.created_at).toLocaleString()}</small>
              {job.parent_job_id && <small>Parent {job.parent_job_id}</small>}
              {job.retry_of_job_id && (
                <small>Retry of {job.retry_of_job_id}</small>
              )}
              {job.error?.message && (
                <p className="error-message">{job.error.message}</p>
              )}
            </div>
            <div className="row-actions">
              {['queued', 'running'].includes(job.state) && (
                <button
                  className="danger"
                  onClick={() =>
                    void run(() => cancelServerJob(profile, job.id))
                  }
                >
                  Cancel
                </button>
              )}
              {['failed', 'cancelled', 'interrupted', 'completed'].includes(
                job.state,
              ) && (
                <button
                  className="secondary"
                  onClick={() =>
                    void run(() => retryServerJob(profile, job.id))
                  }
                >
                  Retry
                </button>
              )}
            </div>
          </article>
        ))}
        {jobs.length === 0 && (
          <p className="empty-message">No jobs are retained on this server.</p>
        )}
      </div>
    </section>
  );
}

function SettingsScreen({
  data,
  onRefresh,
  onError,
}: {
  data: Bootstrap;
  onRefresh: () => Promise<void>;
  onError: (message: string) => void;
}) {
  const [settings, setSettings] = useState<ClientSettings>(data.settings);
  const [preview, setPreview] = useState<Awaited<
    ReturnType<typeof storage.deleteAllPreview>
  > | null>(null);
  const [confirmation, setConfirmation] = useState('');
  const [deleteResult, setDeleteResult] = useState('');
  const [deleting, setDeleting] = useState(false);
  useEffect(() => setSettings(data.settings), [data.settings]);
  async function save() {
    try {
      await storage.saveSettings(settings);
      await onRefresh();
    } catch (reason) {
      onError(String(reason));
    }
  }
  async function previewDelete() {
    try {
      setPreview(await storage.deleteAllPreview());
    } catch (reason) {
      onError(String(reason));
    }
  }
  async function deleteAll() {
    if (!preview) return;
    setDeleting(true);
    try {
      const result = await storage.deleteAllData(confirmation);
      setDeleteResult(
        result.failed.length
          ? `Deleted ${result.succeeded.length} categories; ${result.failed.length} failed. Review diagnostics before retrying.`
          : 'All client-managed data was deleted. Tome is back to first-run state.',
      );
      setPreview(null);
      setConfirmation('');
      await onRefresh();
    } catch (reason) {
      onError(String(reason));
    } finally {
      setDeleting(false);
    }
  }
  return (
    <section className="page-card">
      <PageHeader eyebrow="Stored on this Mac" title="Client settings">
        <button onClick={() => void save()}>Save settings</button>
      </PageHeader>
      <div className="settings-grid">
        <label>
          Default server
          <select
            value={settings.defaultProfileId ?? ''}
            onChange={(event) =>
              setSettings({
                ...settings,
                defaultProfileId: event.target.value || null,
              })
            }
          >
            <option value="">None</option>
            {data.profiles.map((profile) => (
              <option key={profile.id} value={profile.id}>
                {profile.name}
              </option>
            ))}
          </select>
        </label>
        <label>
          Neutral temperature
          <input
            type="number"
            min="0"
            max="2"
            step="0.1"
            value={settings.defaultTemperature}
            onChange={(event) =>
              setSettings({
                ...settings,
                defaultTemperature: Number(event.target.value),
              })
            }
          />
        </label>
        <label>
          Attachment storage
          <select
            value={settings.attachmentStorageMode}
            onChange={(event) =>
              setSettings({
                ...settings,
                attachmentStorageMode: event.target
                  .value as ClientSettings['attachmentStorageMode'],
              })
            }
          >
            <option value="optimized">Optimized copy</option>
            <option value="preserve_originals">Preserve originals</option>
            <option value="metadata_only">Metadata only</option>
          </select>
        </label>
        <label>
          Appearance
          <select
            value={settings.appearance}
            onChange={(event) =>
              setSettings({
                ...settings,
                appearance: event.target.value as ClientSettings['appearance'],
              })
            }
          >
            <option value="system">System</option>
            <option value="light">Light</option>
            <option value="dark">Dark</option>
          </select>
        </label>
        <label>
          Reconnect delay (milliseconds)
          <input
            type="number"
            min="250"
            max="60000"
            step="250"
            value={settings.connection.retryDelayMs}
            onChange={(event) =>
              setSettings({
                ...settings,
                connection: {
                  ...settings.connection,
                  retryDelayMs: Number(event.target.value),
                },
              })
            }
          />
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.connection.reconnect}
            onChange={(event) =>
              setSettings({
                ...settings,
                connection: {
                  ...settings.connection,
                  reconnect: event.target.checked,
                },
              })
            }
          />
          Reconnect to the chosen private server after a dropped connection
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.contextDisplay.showMeter}
            onChange={(event) =>
              setSettings({
                ...settings,
                contextDisplay: {
                  ...settings.contextDisplay,
                  showMeter: event.target.checked,
                },
              })
            }
          />
          Show future context meter
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.contextDisplay.showManifest}
            onChange={(event) =>
              setSettings({
                ...settings,
                contextDisplay: {
                  ...settings.contextDisplay,
                  showManifest: event.target.checked,
                },
              })
            }
          />
          Show future context manifest
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.partialResponses.retain}
            onChange={(event) =>
              setSettings({
                ...settings,
                partialResponses: {
                  ...settings.partialResponses,
                  retain: event.target.checked,
                },
              })
            }
          />
          Retain recoverable partial responses in Phase 4
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.partialResponses.showRecoveryBanner}
            onChange={(event) =>
              setSettings({
                ...settings,
                partialResponses: {
                  ...settings.partialResponses,
                  showRecoveryBanner: event.target.checked,
                },
              })
            }
          />
          Show a recovery banner for future partial responses
        </label>
      </div>
      {data.profiles.length > 0 && (
        <section className="storage-card">
          <h2>Default model references</h2>
          <p>
            These client-local preferences are checked against the selected
            server when it reconnects. A missing model remains an unavailable
            reference rather than silently changing servers.
          </p>
          <div className="settings-grid compact">
            {data.profiles.map((profile) => (
              <label key={profile.id}>
                {profile.name}
                <input
                  value={settings.defaultModels[profile.id] ?? ''}
                  placeholder="Optional installed model ID"
                  onChange={(event) =>
                    setSettings({
                      ...settings,
                      defaultModels: {
                        ...settings.defaultModels,
                        [profile.id]: event.target.value,
                      },
                    })
                  }
                />
              </label>
            ))}
          </div>
        </section>
      )}
      <section className="storage-card">
        <h2>Local storage</h2>
        <p>
          <strong>{formatBytes(data.usage.totalBytes)}</strong> managed under{' '}
          <code>{data.usage.dataDirectory}</code>.
        </p>
        <p>
          Export cache: {formatBytes(data.usage.exportCacheBytes)} ·
          Attachments: {formatBytes(data.usage.attachmentBytes)} · Partial
          responses: {formatBytes(data.usage.partialResponseBytes)}
        </p>
        <p>
          Export cache location:{' '}
          <code>{data.usage.dataDirectory}/export-cache</code>
        </p>
      </section>
      <section className="danger-zone">
        <h2>Delete all client data</h2>
        <p>
          Deletes chats, managed attachments, partial responses, caches, saved
          connections, and settings. Files you exported outside this directory
          are never touched.
        </p>
        <button className="danger" onClick={() => void previewDelete()}>
          Review delete-all
        </button>
        {preview && (
          <div
            className="delete-preview"
            role="dialog"
            aria-modal="true"
            aria-labelledby="delete-title"
          >
            <h3 id="delete-title">
              Delete {formatBytes(preview.totalBytes)} of client-managed data?
            </h3>
            <ul>
              {preview.categories.map((category) => (
                <li key={category.name}>
                  {category.name}: {category.records} files,{' '}
                  {formatBytes(category.bytes)}
                </li>
              ))}
            </ul>
            <label>
              Type <strong>{preview.confirmation}</strong>
              <input
                value={confirmation}
                onChange={(event) => setConfirmation(event.target.value)}
              />
            </label>
            <div className="actions">
              <button className="secondary" onClick={() => setPreview(null)}>
                Cancel
              </button>
              <button
                className="danger"
                disabled={deleting || confirmation !== preview.confirmation}
                onClick={() => void deleteAll()}
              >
                {deleting
                  ? 'Deleting managed data…'
                  : 'Permanently delete local data'}
              </button>
            </div>
          </div>
        )}
        {deleteResult && <p role="status">{deleteResult}</p>}
      </section>
    </section>
  );
}

function OfflinePanel({
  title,
  onConnections,
}: {
  title: string;
  onConnections: () => void;
}) {
  return (
    <section className="page-card offline-panel">
      <p className="eyebrow">Server-owned data</p>
      <h1>{title}</h1>
      <p>
        Select and connect to a compatible private Tome server. Local chats and
        settings remain available offline.
      </p>
      <button onClick={onConnections}>Open connections</button>
    </section>
  );
}

class ModelErrorBoundary extends Component<
  { children: ReactNode },
  { error: string }
> {
  state = { error: '' };

  static getDerivedStateFromError(error: unknown) {
    return { error: String(error) };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('Tome model workspace failed to render', error, info);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <section className="model-workspace loading-panel" role="alert">
        <strong>Model screen could not be displayed.</strong>
        <p>{this.state.error}</p>
        <button onClick={() => this.setState({ error: '' })}>Try again</button>
      </section>
    );
  }
}

function ModelManagement({
  profile,
  reconnect,
  retryDelayMs,
}: {
  profile: ConnectionProfile;
  reconnect: boolean;
  retryDelayMs: number;
}) {
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
    const host = profile.host.trim();
    const formattedHost =
      host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
    const base = `ws://${formattedHost}:${profile.port}`;
    let stopped = false;
    let socket: WebSocket | null = null;
    let reconnectTimer: number | undefined;
    let cursor = profile.lastEventId;
    const connectEvents = () => {
      if (stopped) return;
      setLive('connecting');
      try {
        socket = new WebSocket(
          `${base}/api/v1/events/ws?after_event_id=${cursor}`,
        );
      } catch (reason) {
        console.warn(
          'Live model events are unavailable; using polling.',
          reason,
        );
        setLive('offline');
        return;
      }
      socket.onopen = () => setLive('live');
      socket.onmessage = (message) => {
        try {
          const event = JSON.parse(String(message.data)) as {
            event_id?: number;
          };
          if (typeof event.event_id === 'number' && event.event_id > cursor) {
            cursor = event.event_id;
            void storage.setEventCursor(profile.id, cursor);
          }
        } finally {
          void refresh();
        }
      };
      socket.onerror = () => setLive('offline');
      socket.onclose = () => {
        setLive('offline');
        if (!stopped && reconnect)
          reconnectTimer = window.setTimeout(connectEvents, retryDelayMs);
      };
    };
    connectEvents();
    return () => {
      stopped = true;
      window.clearInterval(timer);
      if (reconnectTimer !== undefined) window.clearTimeout(reconnectTimer);
      socket?.close();
    };
  }, [profile, reconnect, refresh, retryDelayMs]);

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
