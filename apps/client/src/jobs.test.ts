import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  CLEAR_JOB_LOGS_CONFIRMATION,
  clearJobLogs,
  clearJobLogsErrorMessage,
  clearJobLogsSummary,
  mergeJobSnapshot,
  sortJobs,
  type ServerJob,
} from './jobs';
import type { ConnectionProfile } from './connection';

const profile: ConnectionProfile = {
  id: 'profile-test',
  name: 'Test server',
  mode: 'lan',
  host: '127.0.0.1',
  port: 7331,
  createdAt: '2026-01-01T00:00:00Z',
  updatedAt: '2026-01-01T00:00:00Z',
  lastConnectedAt: null,
  lastEventId: 0,
  compatibility: null,
};

afterEach(() => vi.unstubAllGlobals());

function job(
  id: string,
  state: ServerJob['state'],
  created_at: string,
): ServerJob {
  return {
    id,
    idempotency_key: id,
    job_type: 'model_download',
    state,
    progress: 0,
    input: {},
    created_at,
    updated_at: created_at,
    started_at: null,
    finished_at: null,
    parent_job_id: null,
    retry_of_job_id: null,
    error: null,
  };
}

describe('job history state', () => {
  it('sorts deterministically newest first', () => {
    expect(
      sortJobs([
        job('a', 'queued', '2026-01-01T00:00:00Z'),
        job('b', 'queued', '2026-02-01T00:00:00Z'),
      ]).map(({ id }) => id),
    ).toEqual(['b', 'a']);
  });

  it('lets an authoritative reconnect snapshot replace stale state', () => {
    const current = [job('a', 'running', '2026-01-01T00:00:00Z')];
    const refreshed = [job('a', 'completed', '2026-01-01T00:00:00Z')];
    expect(mergeJobSnapshot(current, refreshed)[0].state).toBe('completed');
  });

  it('clears terminal logs through the server and summarizes retained work', async () => {
    const fetcher = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          removed_terminal_jobs: 2,
          removed_job_events: 6,
          retained_active_jobs: 1,
          retained_active_job_events: 2,
          retained_linked_terminal_jobs: 1,
          retained_linked_terminal_events: 3,
        }),
        { status: 200, headers: { 'Content-Type': 'application/json' } },
      ),
    );
    vi.stubGlobal('fetch', fetcher);

    const result = await clearJobLogs(profile);

    expect(fetcher).toHaveBeenCalledWith(
      'http://127.0.0.1:7331/api/v1/jobs/clear',
      expect.objectContaining({ method: 'POST' }),
    );
    expect(clearJobLogsSummary(result)).toContain('Removed 2 terminal jobs');
    expect(clearJobLogsSummary(result)).toContain('Retained 1 active job');
    expect(clearJobLogsSummary(result)).toContain('1 linked terminal job');
  });

  it('surfaces an actionable server failure and explains clear scope', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValue(
          new Response(
            JSON.stringify({ error: { message: 'database is busy' } }),
            { status: 503, headers: { 'Content-Type': 'application/json' } },
          ),
        ),
    );

    let failure: unknown;
    try {
      await clearJobLogs(profile);
    } catch (reason) {
      failure = reason;
    }
    expect(clearJobLogsErrorMessage(failure)).toContain('database is busy');
    expect(clearJobLogsErrorMessage(failure)).toContain(
      'Check the server connection and try again.',
    );
    expect(CLEAR_JOB_LOGS_CONFIRMATION).toContain('Queued and running jobs');
    expect(CLEAR_JOB_LOGS_CONFIRMATION).toContain('partial downloads');
    expect(CLEAR_JOB_LOGS_CONFIRMATION).toContain('chats');
    expect(CLEAR_JOB_LOGS_CONFIRMATION).toContain('settings');
  });
});
