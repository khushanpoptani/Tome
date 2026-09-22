import { afterEach, describe, expect, it, vi } from 'vitest';
import { formatBytes, loadModelSnapshot } from './model-management';
import type { ConnectionProfile } from './connection';

const profile: ConnectionProfile = {
  id: '0199a0ce-491b-7cc4-bbb7-9279f9067241',
  name: 'Server',
  mode: 'lan',
  host: '192.168.1.2',
  port: 7331,
  createdAt: '2026-09-22T00:00:00.000Z',
  updatedAt: '2026-09-22T00:00:00.000Z',
  lastConnectedAt: null,
  lastEventId: 0,
  compatibility: null,
};

afterEach(() => vi.unstubAllGlobals());

describe('remote model management', () => {
  it('formats known and unknown capacity honestly', () => {
    expect(formatBytes(null)).toBe('Unknown');
    expect(formatBytes(1_500_000_000)).toBe('1.5 GB');
  });

  it('refreshes authoritative state and keeps only model download jobs', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn((input: string | URL | Request) => {
        const url = String(input);
        let body: unknown = {};
        if (url.endsWith('/hardware')) body = { cpu_architecture: 'x86_64' };
        if (url.endsWith('/model-catalog'))
          body = { entries: [], catalog_version: 'test' };
        if (url.endsWith('/models')) body = { models: [], ready: false };
        if (url.endsWith('/model-setup')) body = { profiles: [], ready: false };
        if (url.includes('/jobs?'))
          body = {
            jobs: [
              { id: 'download', job_type: 'model_download', state: 'running' },
              { id: 'later', job_type: 'inference', state: 'failed' },
            ],
          };
        return Promise.resolve(
          new Response(JSON.stringify(body), { status: 200 }),
        );
      }),
    );
    const snapshot = await loadModelSnapshot(profile);
    expect(snapshot.jobs.map((job) => job.id)).toEqual(['download']);
  });
});
