import { describe, expect, it } from 'vitest';
import { mergeJobSnapshot, sortJobs, type ServerJob } from './jobs';

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
});
