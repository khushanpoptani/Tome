export interface LocalModelSnapshot {
  hardware: {
    cpu_brand: string | null;
    cpu_architecture: string;
    total_memory_bytes: number;
    gpu: { name: string | null; memory_note: string };
    model_storage: { root: string; free_bytes: number | null };
    runtime: { llama_cpp: { available: boolean; reason: string } };
  };
  inventory: {
    ready: boolean;
    default_model_id: string | null;
    models: Array<{
      id: string;
      catalog_id: string;
      display_name: string;
      quantization: string;
      byte_size: number;
      loaded: boolean;
      in_use_count: number;
      compatibility_state: string;
      compatibility_reason: string;
    }>;
  };
  jobs: Array<{
    id: string;
    job_type: string;
    state: string;
    progress: number;
    input: { catalog_id?: string };
  }>;
}

export interface ModelCandidate {
  candidate_id: string;
  provider: string;
  repository: string;
  revision: string;
  artifact: string;
  display_name: string;
  parameter_size: string | null;
  quantization: string | null;
  format: string;
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
  normalized_query: string;
  candidates: ModelCandidate[];
}

async function api<T>(
  port: number,
  path: string,
  init?: RequestInit,
): Promise<T> {
  const response = await fetch(`http://127.0.0.1:${port}${path}`, {
    ...init,
    headers: {
      'Content-Type': 'application/json',
      'X-Tome-Protocol-Version': '1',
      ...init?.headers,
    },
  });
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as {
      error?: { code?: string; message?: string };
    } | null;
    throw new Error(
      `${body?.error?.code ? `${body.error.code}: ` : ''}${body?.error?.message ?? `HTTP ${response.status}`}`,
    );
  }
  return response.json() as Promise<T>;
}

export async function loadLocalModels(
  port: number,
): Promise<LocalModelSnapshot> {
  const [hardware, inventory, jobs] = await Promise.all([
    api<LocalModelSnapshot['hardware']>(port, '/api/v1/hardware'),
    api<LocalModelSnapshot['inventory']>(port, '/api/v1/models'),
    api<{ jobs: LocalModelSnapshot['jobs'] }>(port, '/api/v1/jobs?limit=100'),
  ]);
  return {
    hardware,
    inventory,
    jobs: jobs.jobs.filter((job) => job.job_type === 'model_download'),
  };
}

export const localSearch = (port: number, query: string) =>
  api<ModelSearchResponse>(
    port,
    `/api/v1/model-search?q=${encodeURIComponent(query)}`,
  );

export const localDownload = (port: number, candidate: ModelCandidate) =>
  api(port, '/api/v1/model-downloads', {
    method: 'POST',
    body: JSON.stringify({
      provider: candidate.provider,
      repository: candidate.repository,
      revision: candidate.revision,
      artifact: candidate.artifact,
      idempotency_key: `dashboard-${candidate.candidate_id}-${crypto.randomUUID()}`,
      license_accepted: true,
    }),
  });
export const localJobAction = (
  port: number,
  id: string,
  action: 'pause' | 'resume' | 'cancel',
) =>
  api(port, `/api/v1/jobs/${encodeURIComponent(id)}/${action}`, {
    method: 'POST',
  });
export const localModelAction = (
  port: number,
  id: string,
  action: 'load' | 'unload',
) =>
  api(port, `/api/v1/models/${encodeURIComponent(id)}/${action}`, {
    method: 'POST',
  });
export const localDefault = (port: number, id: string) =>
  api(port, '/api/v1/models/default', {
    method: 'PUT',
    body: JSON.stringify({ model_id: id }),
  });
export const localDelete = (port: number, id: string) =>
  api(port, `/api/v1/models/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    body: JSON.stringify({ confirmation: id }),
  });

export function formatBytes(bytes: number | null) {
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
