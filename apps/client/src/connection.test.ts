import { describe, expect, it, vi } from 'vitest';
import {
  CLIENT_PROTOCOL_VERSION,
  connectToServer,
  isProtocolCompatible,
  serverBaseUrl,
  type ConnectionProfile,
  type ConnectionState,
} from './connection';

const profile: ConnectionProfile = {
  id: '0199a0ce-491b-7cc4-bbb7-9279f9067241',
  name: 'Studio PC',
  mode: 'lan',
  host: '192.168.1.20',
  port: 7331,
  createdAt: '2026-09-22T00:00:00.000Z',
  updatedAt: '2026-09-22T00:00:00.000Z',
  lastConnectedAt: null,
  lastEventId: 0,
  compatibility: null,
};

function capabilities(minimum = 1, maximum = 1) {
  return {
    protocol: { current: 1, minimum, maximum },
    authentication: 'none',
    network_mode: 'lan',
    features: {
      persistent_jobs: true,
      event_replay: true,
      websocket_events: true,
      inference_token_streaming: false,
    },
    job_types: [],
  };
}

describe('connection foundation', () => {
  it('builds IPv4 and IPv6 server URLs', () => {
    expect(serverBaseUrl(profile)).toBe('http://192.168.1.20:7331');
    expect(serverBaseUrl({ ...profile, host: 'fd7a:115c:a1e0::1' })).toBe(
      'http://[fd7a:115c:a1e0::1]:7331',
    );
  });

  it('checks the supported protocol range', () => {
    expect(CLIENT_PROTOCOL_VERSION).toBe(1);
    expect(isProtocolCompatible({ current: 1, minimum: 1, maximum: 1 })).toBe(
      true,
    );
    expect(isProtocolCompatible({ current: 2, minimum: 2, maximum: 3 })).toBe(
      false,
    );
  });

  it('retrieves capabilities and reaches connected state', async () => {
    const states: ConnectionState[] = [];
    const fetcher = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(capabilities()), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    );
    const result = await connectToServer(profile, {
      fetcher,
      onState: (state) => states.push(state),
    });
    expect(result.status).toBe('connected');
    expect(states.map((state) => state.status)).toEqual([
      'connecting',
      'connected',
    ]);
  });

  it('reports reconnecting before a successful retry', async () => {
    const states: ConnectionState[] = [];
    const fetcher = vi
      .fn()
      .mockRejectedValueOnce(new TypeError('offline'))
      .mockResolvedValueOnce(
        new Response(JSON.stringify(capabilities()), { status: 200 }),
      );
    const result = await connectToServer(profile, {
      fetcher,
      onState: (state) => states.push(state),
      sleep: () => Promise.resolve(),
    });
    expect(result.status).toBe('connected');
    expect(states.map((state) => state.status)).toEqual([
      'connecting',
      'reconnecting',
      'connected',
    ]);
  });

  it('reports an incompatible protocol without a false connection', async () => {
    const result = await connectToServer(profile, {
      fetcher: vi
        .fn()
        .mockResolvedValue(
          new Response(JSON.stringify(capabilities(2, 2)), { status: 200 }),
        ),
    });
    expect(result).toEqual({
      status: 'incompatible',
      message:
        'Protocol mismatch: this client supports 1, but the server accepts 2–2.',
    });
  });

  it('rejects public numeric endpoints before connecting', async () => {
    const result = await connectToServer({ ...profile, host: '8.8.8.8' });
    expect(result.status).toBe('error');
  });
});
