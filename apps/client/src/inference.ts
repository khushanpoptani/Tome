import { isTauri } from '@tauri-apps/api/core';
import { fetch as tauriFetch } from '@tauri-apps/plugin-http';
import {
  CLIENT_PROTOCOL_VERSION,
  serverBaseUrl,
  type ConnectionProfile,
} from './connection';
import type { Message, MessageGeneration } from './client-storage';
import type { ServerJob } from './jobs';

export interface InferenceRequest {
  schema_version: 1;
  client_request_id: string;
  model_id: string;
  messages: Array<{
    id: string;
    role: Message['role'];
    content: string;
  }>;
  system_instructions: Array<{
    id: string;
    content: string;
    pinned: boolean;
  }>;
  settings: {
    temperature: number;
    max_output_tokens: number;
    agent_tool_reserve: number;
  };
  correlation: {
    chat_id: string;
    user_message_id: string;
    assistant_message_id: string;
    client_revision_id: string | null;
  };
  parent_job_id: string | null;
  retry_of_job_id: string | null;
}

export interface ContextManifest {
  schema_version: number;
  model_id: string;
  model_artifact_sha256: string;
  input_token_total: number;
  output_token_reserve: number;
  agent_tool_reserve: number;
  included_message_ids: string[];
  excluded_messages: Array<{ id: string; reason: string }>;
  warnings: string[];
}

export interface InferenceDetails {
  schema_version: number;
  job_id: string;
  model_id: string;
  model_artifact_sha256: string;
  request: InferenceRequest;
  settings: InferenceRequest['settings'];
  context_manifest: ContextManifest | null;
  output_text: string;
  output_sequence: number;
  usage: Record<string, unknown> | null;
  completion_reason: string | null;
  warnings: string[];
}

export interface InferenceEvent {
  event_id: number;
  job_id: string | null;
  event_type: string;
  payload: Record<string, unknown>;
  occurred_at: string;
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

export async function submitInference(
  profile: ConnectionProfile,
  inference: InferenceRequest,
): Promise<{ job: ServerJob; duplicate: boolean }> {
  return request(profile, '/api/v1/inference', {
    method: 'POST',
    body: JSON.stringify(inference),
  });
}

export function loadInferenceDetails(
  profile: ConnectionProfile,
  jobId: string,
): Promise<InferenceDetails> {
  return request(
    profile,
    `/api/v1/jobs/${encodeURIComponent(jobId)}/inference`,
  );
}

export function loadServerJob(
  profile: ConnectionProfile,
  jobId: string,
): Promise<ServerJob> {
  return request(profile, `/api/v1/jobs/${encodeURIComponent(jobId)}`);
}

export async function loadInferenceEvents(
  profile: ConnectionProfile,
  jobId: string,
): Promise<InferenceEvent[]> {
  const result = await request<{ events: InferenceEvent[] }>(
    profile,
    '/api/v1/events?after_event_id=0&limit=1000',
  );
  return result.events.filter((event) => event.job_id === jobId);
}

export function eventSocketUrl(
  profile: ConnectionProfile,
  afterEventId: number,
): string {
  const host =
    profile.host.includes(':') && !profile.host.startsWith('[')
      ? `[${profile.host}]`
      : profile.host;
  return `ws://${host}:${profile.port}/api/v1/events/ws?after_event_id=${Math.max(0, afterEventId)}`;
}

export function reconcileOutput(
  currentText: string,
  currentSequence: number,
  event: InferenceEvent,
): { text: string; sequence: number } {
  const sequence = Number(event.payload.output_sequence);
  if (!Number.isSafeInteger(sequence) || sequence <= currentSequence) {
    return { text: currentText, sequence: currentSequence };
  }
  if (event.event_type === 'inference.output_checkpoint') {
    if (typeof event.payload.text !== 'string') {
      return { text: currentText, sequence: currentSequence };
    }
    return {
      text: event.payload.text,
      sequence,
    };
  }
  if (event.event_type === 'inference.text_delta') {
    if (sequence !== currentSequence + 1) {
      return { text: currentText, sequence: currentSequence };
    }
    return {
      text:
        currentText +
        (typeof event.payload.text === 'string' ? event.payload.text : ''),
      sequence,
    };
  }
  return { text: currentText, sequence: currentSequence };
}

export function generationFromDetails(
  details: InferenceDetails,
  state: string,
  retryOfJobId: string | null,
): MessageGeneration {
  return {
    modelId: details.model_id,
    modelArtifactSha256: details.model_artifact_sha256,
    temperature: details.settings.temperature,
    maxOutputTokens: details.settings.max_output_tokens,
    jobId: details.job_id,
    state,
    outputSequence: details.output_sequence,
    usage: details.usage,
    completionReason: details.completion_reason,
    contextWarnings: [
      ...details.warnings,
      ...(details.context_manifest?.warnings ?? []),
      ...(details.context_manifest?.excluded_messages.length
        ? [
            `${details.context_manifest.excluded_messages.length} older message(s) were outside this model's context window.`,
          ]
        : []),
    ],
    retryOfJobId,
  };
}

export function interactionMarkdown(
  user: Message,
  assistant: Message,
  localOnly: boolean,
): string {
  const metadata = assistant.generation;
  return [
    '# Tome interaction',
    '',
    localOnly
      ? '> Local-only export: authoritative server job details were unavailable.'
      : '> Includes reconciled local content and authoritative server metadata.',
    '',
    '## User',
    '',
    user.content,
    '',
    '## Assistant',
    '',
    assistant.content || '_No output was generated._',
    '',
    '## Generation',
    '',
    `- State: ${metadata?.state ?? 'unknown'}`,
    `- Model: ${metadata?.modelId ?? 'unknown'}`,
    `- Temperature: ${metadata?.temperature ?? 'unknown'}`,
    `- Job: ${metadata?.jobId ?? 'unavailable'}`,
    `- Completion reason: ${metadata?.completionReason ?? 'unavailable'}`,
    '',
  ].join('\n');
}

export function sanitizedJobExport(value: unknown): string {
  const redact = (item: unknown): unknown => {
    if (Array.isArray(item)) return item.map(redact);
    if (item && typeof item === 'object') {
      const result: Record<string, unknown> = {};
      for (const [key, child] of Object.entries(item)) {
        if (
          /authorization|credential|access[_-]?token|signed[_-]?url/i.test(key)
        )
          continue;
        result[key] = redact(child);
      }
      return result;
    }
    return item;
  };
  return `${JSON.stringify(redact(value), null, 2)}\n`;
}
