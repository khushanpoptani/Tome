import { isTauri } from '@tauri-apps/api/core';
import { fetch as tauriFetch } from '@tauri-apps/plugin-http';
import {
  CLIENT_PROTOCOL_VERSION,
  serverBaseUrl,
  type ConnectionProfile,
} from './connection';

export interface HardwareCapabilities {
  cpu_architecture: string;
  cpu_brand: string | null;
  logical_cpu_count: number;
  total_memory_bytes: number;
  available_memory_bytes: number | null;
  gpu: {
    vendor: string | null;
    name: string | null;
    kind: string;
    usable_memory_bytes: number | null;
    memory_note: string;
  };
  model_storage: {
    root: string;
    free_bytes: number | null;
    free_bytes_note: string;
  };
  runtime: {
    llama_cpp: {
      available: boolean;
      executable: string | null;
      version: string | null;
      reason: string;
    };
  };
}

export interface ModelCandidate {
  candidate_id: string;
  provider: string;
  repository: string;
  revision: string;
  artifact: string;
  display_name: string;
  parameter_size: string | null;
  format: string;
  quantization: string | null;
  bytes: number | null;
  sha256: string | null;
  license: string | null;
  access: string;
  context_limit: number | null;
  tokenizer: string | null;
  chat_template_available: boolean | null;
  capabilities: string[];
  runtime_compatible: boolean;
  runtime_reason: string;
  estimated_disk_bytes: number | null;
  estimated_ram_bytes: number | null;
  downloadable: boolean;
  unavailable_reason: string | null;
}

export interface ModelSearchResponse {
  provider: string;
  query: string;
  normalized_query: string;
  candidates: ModelCandidate[];
}

export interface InstalledModel {
  id: string;
  catalog_id: string;
  display_name: string;
  format: string;
  quantization: string;
  byte_size: number;
  verification_state: string;
  compatibility_state: string;
  compatibility_reason: string;
  capabilities: string[];
  loaded: boolean;
  in_use_count: number;
}

export interface Inventory {
  schema_version: number;
  catalog_version: string;
  ready: boolean;
  default_model_id: string | null;
  models: InstalledModel[];
}

export interface ModelJob {
  id: string;
  job_type: string;
  state: string;
  progress: number;
  input: { catalog_id?: string };
  error?: { code?: string; message?: string };
}

interface JobsResponse {
  jobs: ModelJob[];
}

export interface ModelSnapshot {
  hardware: HardwareCapabilities;
  inventory: Inventory;
  jobs: ModelJob[];
}

async function request<T>(
  profile: ConnectionProfile,
  path: string,
  init?: RequestInit,
): Promise<T> {
  const fetcher = isTauri() ? tauriFetch : fetch;
  let response: Response;
  try {
    response = await fetcher(`${serverBaseUrl(profile)}${path}`, {
      ...init,
      headers: {
        'Content-Type': 'application/json',
        'X-Tome-Protocol-Version': String(CLIENT_PROTOCOL_VERSION),
        ...init?.headers,
      },
    });
  } catch (error) {
    throw new Error(`Server is offline or unreachable. ${String(error)}`, {
      cause: error,
    });
  }
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as {
      error?: { code?: string; message?: string };
    } | null;
    const code = body?.error?.code ? `${body.error.code}: ` : '';
    throw new Error(
      `${code}${body?.error?.message ?? `HTTP ${response.status}`}`,
    );
  }
  return (await response.json()) as T;
}

export async function loadModelSnapshot(
  profile: ConnectionProfile,
): Promise<ModelSnapshot> {
  const [hardware, inventory, jobs] = await Promise.all([
    request<HardwareCapabilities>(profile, '/api/v1/hardware'),
    request<Inventory>(profile, '/api/v1/models'),
    request<JobsResponse>(profile, '/api/v1/jobs?limit=100'),
  ]);
  return {
    hardware,
    inventory,
    jobs: jobs.jobs.filter((job) => job.job_type === 'model_download'),
  };
}

export function searchModels(profile: ConnectionProfile, query: string) {
  return request<ModelSearchResponse>(
    profile,
    `/api/v1/model-search?q=${encodeURIComponent(query)}`,
  );
}

export function startDownload(
  profile: ConnectionProfile,
  candidate: ModelCandidate,
) {
  return request(profile, '/api/v1/model-downloads', {
    method: 'POST',
    body: JSON.stringify({
      provider: candidate.provider,
      repository: candidate.repository,
      revision: candidate.revision,
      artifact: candidate.artifact,
      idempotency_key: `client-${candidate.candidate_id}-${crypto.randomUUID()}`,
      license_accepted: true,
    }),
  });
}

export function jobAction(
  profile: ConnectionProfile,
  jobId: string,
  action: 'pause' | 'resume' | 'cancel',
) {
  return request(
    profile,
    `/api/v1/jobs/${encodeURIComponent(jobId)}/${action}`,
    {
      method: 'POST',
    },
  );
}

export function retryJob(profile: ConnectionProfile, jobId: string) {
  return request(profile, `/api/v1/jobs/${encodeURIComponent(jobId)}/retry`, {
    method: 'POST',
    body: JSON.stringify({
      idempotency_key: `retry-${jobId}-${crypto.randomUUID()}`,
    }),
  });
}

export function modelAction(
  profile: ConnectionProfile,
  modelId: string,
  action: 'load' | 'unload',
) {
  return request(
    profile,
    `/api/v1/models/${encodeURIComponent(modelId)}/${action}`,
    {
      method: 'POST',
    },
  );
}

export function deleteModel(profile: ConnectionProfile, modelId: string) {
  return request(profile, `/api/v1/models/${encodeURIComponent(modelId)}`, {
    method: 'DELETE',
    body: JSON.stringify({ confirmation: modelId }),
  });
}

export function setDefaultModel(profile: ConnectionProfile, modelId: string) {
  return request(profile, '/api/v1/models/default', {
    method: 'PUT',
    body: JSON.stringify({ model_id: modelId }),
  });
}

export async function exportInventory(profile: ConnectionProfile) {
  const inventory = await request<Inventory>(profile, '/api/v1/models/export');
  const blob = new Blob([`${JSON.stringify(inventory, null, 2)}\n`], {
    type: 'application/json',
  });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = 'tome-model-inventory.json';
  anchor.click();
  URL.revokeObjectURL(url);
}

export function formatBytes(bytes: number | null): string {
  if (bytes === null) return 'Unknown';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value.toFixed(unit > 1 ? 1 : 0)} ${units[unit]}`;
}
