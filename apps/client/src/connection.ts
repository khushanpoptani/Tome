import { isTauri } from '@tauri-apps/api/core';
import { fetch as tauriFetch } from '@tauri-apps/plugin-http';

export const CLIENT_PROTOCOL_VERSION = 1;
const STORAGE_KEY = 'tome.phase1.connection-profile';

export type ConnectionMode = 'lan' | 'tailscale';

export interface ConnectionProfile {
  name: string;
  mode: ConnectionMode;
  host: string;
  port: number;
}

export interface ProtocolRange {
  current: number;
  minimum: number;
  maximum: number;
}

export interface ServerCapabilities {
  protocol: ProtocolRange;
  authentication: 'none';
  network_mode: 'loopback' | 'multi' | ConnectionMode;
  features: {
    persistent_jobs: boolean;
    event_replay: boolean;
    websocket_events: boolean;
    inference_token_streaming: boolean;
    model_management?: boolean;
    hardware_discovery?: boolean;
    durable_model_downloads?: boolean;
  };
  job_types: Array<{ job_type: string; implemented: boolean }>;
}

export type ConnectionState =
  | { status: 'disconnected' }
  | { status: 'connecting' }
  | { status: 'reconnecting'; attempt: number }
  | { status: 'connected'; capabilities: ServerCapabilities }
  | { status: 'error'; message: string };

export interface ConnectOptions {
  fetcher?: typeof fetch;
  maxAttempts?: number;
  retryDelayMs?: number;
  onState?: (state: ConnectionState) => void;
  sleep?: (milliseconds: number) => Promise<void>;
}

export function validateProfile(profile: ConnectionProfile): string | null {
  if (!profile.name.trim()) return 'Connection name is required.';
  const host = profile.host.trim();
  if (!host) return 'Host or IP address is required.';
  if (/^https?:\/\//i.test(host) || /[/\\?#]/.test(host)) {
    return 'Enter only a host name or IP address, without a URL or path.';
  }
  if (host === '0.0.0.0' || host === '::' || host === '[::]') {
    return 'A wildcard address cannot be used as a connection destination.';
  }
  if (
    !Number.isInteger(profile.port) ||
    profile.port < 1 ||
    profile.port > 65_535
  ) {
    return 'Port must be between 1 and 65535.';
  }
  return null;
}

export function serverBaseUrl(profile: ConnectionProfile): string {
  const host = profile.host.trim();
  const formattedHost =
    host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
  return `http://${formattedHost}:${profile.port}`;
}

export function isProtocolCompatible(protocol: ProtocolRange): boolean {
  return (
    CLIENT_PROTOCOL_VERSION >= protocol.minimum &&
    CLIENT_PROTOCOL_VERSION <= protocol.maximum
  );
}

export async function connectToServer(
  profile: ConnectionProfile,
  options: ConnectOptions = {},
): Promise<ConnectionState> {
  const validationError = validateProfile(profile);
  if (validationError) {
    const state: ConnectionState = {
      status: 'error',
      message: validationError,
    };
    options.onState?.(state);
    return state;
  }

  const fetcher = options.fetcher ?? (isTauri() ? tauriFetch : fetch);
  const attempts = Math.max(1, options.maxAttempts ?? 2);
  const sleep =
    options.sleep ??
    ((milliseconds) =>
      new Promise((resolve) => setTimeout(resolve, milliseconds)));
  options.onState?.({ status: 'connecting' });
  let lastFailure = '';

  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      const response = await fetcher(
        `${serverBaseUrl(profile)}/api/v1/capabilities`,
        {
          headers: {
            'X-Tome-Protocol-Version': String(CLIENT_PROTOCOL_VERSION),
          },
        },
      );
      if (!response.ok) {
        if (response.status === 426) {
          const envelope = (await response.json()) as {
            error?: { details?: Partial<ProtocolRange> };
          };
          const details = envelope.error?.details;
          if (
            typeof details?.minimum === 'number' &&
            typeof details.maximum === 'number'
          ) {
            return emitError(
              options,
              `Protocol mismatch: this client supports ${CLIENT_PROTOCOL_VERSION}, but the server accepts ${details.minimum}–${details.maximum}.`,
            );
          }
        }
        return emitError(options, `Server returned HTTP ${response.status}.`);
      }
      const capabilities = (await response.json()) as ServerCapabilities;
      if (!isProtocolCompatible(capabilities.protocol)) {
        return emitError(
          options,
          `Protocol mismatch: this client supports ${CLIENT_PROTOCOL_VERSION}, but the server accepts ${capabilities.protocol.minimum}–${capabilities.protocol.maximum}.`,
        );
      }
      if (capabilities.authentication !== 'none') {
        return emitError(
          options,
          'This Phase 1 client only supports servers without app-level login.',
        );
      }
      const state: ConnectionState = { status: 'connected', capabilities };
      options.onState?.(state);
      return state;
    } catch (error) {
      lastFailure = describeFailure(error);
      if (attempt < attempts) {
        options.onState?.({ status: 'reconnecting', attempt: attempt + 1 });
        await sleep(options.retryDelayMs ?? 750);
      }
    }
  }
  return emitError(
    options,
    `Could not reach the server. Check its address, port, and network.${lastFailure ? ` ${lastFailure}` : ''}`,
  );
}

export function saveProfile(
  profile: ConnectionProfile,
  storage: Storage = localStorage,
): void {
  const error = validateProfile(profile);
  if (error) throw new Error(error);
  storage.setItem(STORAGE_KEY, JSON.stringify(profile));
}

export function loadProfile(
  storage: Storage = localStorage,
): ConnectionProfile | null {
  const stored = storage.getItem(STORAGE_KEY);
  if (!stored) return null;
  try {
    const profile = JSON.parse(stored) as ConnectionProfile;
    return validateProfile(profile) ? null : profile;
  } catch {
    return null;
  }
}

function emitError(options: ConnectOptions, message: string): ConnectionState {
  const state: ConnectionState = { status: 'error', message };
  options.onState?.(state);
  return state;
}

function describeFailure(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return typeof error === 'string' ? error : '';
}
