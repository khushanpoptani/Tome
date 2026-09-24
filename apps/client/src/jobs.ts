import { isTauri } from '@tauri-apps/api/core';
import { fetch as tauriFetch } from '@tauri-apps/plugin-http';
import {
  CLIENT_PROTOCOL_VERSION,
  serverBaseUrl,
  type ConnectionProfile,
} from './connection';

export interface ServerJob {
  id: string;
  idempotency_key: string;
  job_type: string;
  state:
    'queued' | 'running' | 'completed' | 'failed' | 'cancelled' | 'interrupted';
  progress: number;
  input: Record<string, unknown>;
  created_at: string;
  updated_at: string;
  started_at: string | null;
  finished_at: string | null;
  parent_job_id?: string | null;
  retry_of_job_id?: string | null;
  error?: { code?: string; message?: string } | null;
}

export interface ClearJobLogsResult {
  removed_terminal_jobs: number;
  removed_job_events: number;
  retained_active_jobs: number;
  retained_active_job_events: number;
  retained_linked_terminal_jobs: number;
  retained_linked_terminal_events: number;
}

export const CLEAR_JOB_LOGS_CONFIRMATION =
  'Clear completed, failed, cancelled, and interrupted job records and their event logs? Queued and running jobs, plus terminal jobs they still reference, will be kept. Models, model files, partial downloads, chats, attachments, exports, and settings will not be removed. This cannot be undone.';

async function request<T>(
  profile: ConnectionProfile,
  path: string,
  init?: RequestInit,
): Promise<T> {
  const fetcher = isTauri() ? tauriFetch : fetch;
  const response = await fetcher(`${serverBaseUrl(profile)}${path}`, {
    ...init,
    headers: {
      'Content-Type': 'application/json',
      'X-Tome-Protocol-Version': String(CLIENT_PROTOCOL_VERSION),
      ...init?.headers,
    },
  });
  if (!response.ok) {
    const value = (await response.json().catch(() => null)) as {
      error?: { message?: string };
    } | null;
    throw new Error(
      value?.error?.message ?? `Server returned HTTP ${response.status}.`,
    );
  }
  return (await response.json()) as T;
}

export async function loadJobs(
  profile: ConnectionProfile,
): Promise<ServerJob[]> {
  const result = await request<{ jobs: ServerJob[] }>(
    profile,
    '/api/v1/jobs?limit=200',
  );
  return sortJobs(result.jobs);
}

export function cancelServerJob(profile: ConnectionProfile, id: string) {
  return request(profile, `/api/v1/jobs/${encodeURIComponent(id)}/cancel`, {
    method: 'POST',
  });
}

export function retryServerJob(profile: ConnectionProfile, id: string) {
  return request(profile, `/api/v1/jobs/${encodeURIComponent(id)}/retry`, {
    method: 'POST',
    body: JSON.stringify({
      idempotency_key: `client-retry-${crypto.randomUUID()}`,
    }),
  });
}

export function clearJobLogs(profile: ConnectionProfile) {
  return request<ClearJobLogsResult>(profile, '/api/v1/jobs/clear', {
    method: 'POST',
  });
}

export function clearJobLogsSummary(result: ClearJobLogsResult): string {
  const retainedTerminal = result.retained_linked_terminal_jobs;
  return `Removed ${result.removed_terminal_jobs} terminal job${result.removed_terminal_jobs === 1 ? '' : 's'} and ${result.removed_job_events} event${result.removed_job_events === 1 ? '' : 's'}. Retained ${result.retained_active_jobs} active job${result.retained_active_jobs === 1 ? '' : 's'}${retainedTerminal > 0 ? ` and ${retainedTerminal} linked terminal job${retainedTerminal === 1 ? '' : 's'}` : ''}.`;
}

export function clearJobLogsErrorMessage(reason: unknown): string {
  return `Could not clear job logs: ${String(reason)} Check the server connection and try again.`;
}

export function sortJobs(jobs: ServerJob[]): ServerJob[] {
  return [...jobs].sort(
    (left, right) =>
      right.created_at.localeCompare(left.created_at) ||
      left.id.localeCompare(right.id),
  );
}

export function mergeJobSnapshot(
  current: ServerJob[],
  authoritative: ServerJob[],
): ServerJob[] {
  const merged = new Map(current.map((job) => [job.id, job]));
  for (const job of authoritative) merged.set(job.id, job);
  return sortJobs([...merged.values()]);
}
