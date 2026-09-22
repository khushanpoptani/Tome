import { isTauri } from '@tauri-apps/api/core';
import { fetch as tauriFetch } from '@tauri-apps/plugin-http';
import type { ServerProfile } from './client-storage';

export const CLIENT_PROTOCOL_VERSION = 1;
export type ConnectionProfile = ServerProfile;

export interface ProtocolRange {
  current: number;
  minimum: number;
  maximum: number;
}

export interface ServerCapabilities {
  protocol: ProtocolRange;
  authentication: 'none';
  network_mode: 'loopback' | 'multi' | ServerProfile['mode'];
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
  | { status: 'offline'; message: string }
  | { status: 'incompatible'; message: string }
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
  if (isPublicIpLiteral(host, profile.mode)) {
    return profile.mode === 'tailscale'
      ? 'Tailscale profiles require a 100.64.0.0/10 address, Tailscale IPv6 address, or MagicDNS name.'
      : 'LAN profiles cannot use a public IP address.';
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
            return emitIncompatible(
              options,
              `Protocol mismatch: this client supports ${CLIENT_PROTOCOL_VERSION}, but the server accepts ${details.minimum}–${details.maximum}.`,
            );
          }
        }
        return emitError(options, `Server returned HTTP ${response.status}.`);
      }
      const capabilities = (await response.json()) as ServerCapabilities;
      if (!isProtocolCompatible(capabilities.protocol)) {
        return emitIncompatible(
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
  const state: ConnectionState = {
    status: 'offline',
    message: `Could not reach the server. Check its address, port, and network.${lastFailure ? ` ${lastFailure}` : ''}`,
  };
  options.onState?.(state);
  return state;
}

function emitError(options: ConnectOptions, message: string): ConnectionState {
  const state: ConnectionState = { status: 'error', message };
  options.onState?.(state);
  return state;
}

function emitIncompatible(
  options: ConnectOptions,
  message: string,
): ConnectionState {
  const state: ConnectionState = { status: 'incompatible', message };
  options.onState?.(state);
  return state;
}

function isPublicIpLiteral(host: string, mode: ServerProfile['mode']): boolean {
  const ipv4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (ipv4) {
    const octets = ipv4.slice(1).map(Number);
    if (octets.some((octet) => octet > 255)) return true;
    const [a, b] = octets;
    const tailscale = a === 100 && b >= 64 && b <= 127;
    if (mode === 'tailscale') return !tailscale;
    return !(
      a === 10 ||
      a === 127 ||
      (a === 169 && b === 254) ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168)
    );
  }
  if (host.includes(':')) {
    const normalized = host.replace(/^\[|\]$/g, '').toLowerCase();
    if (mode === 'tailscale') return !normalized.startsWith('fd7a:115c:a1e0:');
    return !(
      normalized === '::1' ||
      normalized.startsWith('fc') ||
      normalized.startsWith('fd') ||
      normalized.startsWith('fe8') ||
      normalized.startsWith('fe9') ||
      normalized.startsWith('fea') ||
      normalized.startsWith('feb')
    );
  }
  return false;
}

function describeFailure(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return typeof error === 'string' ? error : '';
}
