import { useState, type FormEvent } from 'react';
import {
  connectToServer,
  loadProfile,
  saveProfile,
  type ConnectionProfile,
  type ConnectionState,
} from './connection';

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
    <main className="shell">
      <section className="connection-card" aria-labelledby="connection-heading">
        <header>
          <div>
            <p className="eyebrow">Phase 1 · Server connection</p>
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
          Only this connection bootstrap profile is stored in Phase 1. Chat and
          product data remain out of scope.
        </footer>
      </section>
    </main>
  );
}

function Status({ state }: { state: ConnectionState }) {
  const label =
    state.status === 'reconnecting'
      ? `Reconnecting · ${state.attempt}`
      : state.status;
  return <span className={`status status-${state.status}`}>{label}</span>;
}
