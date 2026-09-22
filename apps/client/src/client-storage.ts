import { invoke, isTauri } from '@tauri-apps/api/core';

export type NetworkMode = 'lan' | 'tailscale';
export type AttachmentStorageMode =
  'optimized' | 'preserve_originals' | 'metadata_only';
export type Appearance = 'system' | 'light' | 'dark';

export interface CompatibilitySnapshot {
  compatible: boolean;
  protocol: number;
  checkedAt: string;
  diagnostic: string | null;
}

export interface ServerProfile {
  id: string;
  name: string;
  mode: NetworkMode;
  host: string;
  port: number;
  createdAt: string;
  updatedAt: string;
  lastConnectedAt: string | null;
  lastEventId: number;
  compatibility: CompatibilitySnapshot | null;
}

export interface ClientSettings {
  schemaVersion: 1;
  defaultProfileId: string | null;
  lastUsedProfileId: string | null;
  defaultModels: Record<string, string>;
  defaultTemperature: number;
  attachmentStorageMode: AttachmentStorageMode;
  appearance: Appearance;
  contextDisplay: { showMeter: boolean; showManifest: boolean };
  partialResponses: { retain: boolean; showRecoveryBanner: boolean };
  connection: { reconnect: boolean; retryDelayMs: number };
  [key: string]: unknown;
}

export interface ChatSummary {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  serverProfileId: string | null;
  modelId: string | null;
  messageCount: number;
}

export interface Message {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  createdAt: string;
  parentMessageId: string | null;
  attachmentIds: string[];
}

export interface Chat extends Omit<ChatSummary, 'messageCount'> {
  schemaVersion: 1;
  messages: Message[];
  jobReferences: Array<{
    jobId: string;
    serverProfileId: string;
    messageId: string | null;
    parentJobId: string | null;
    retryOfJobId: string | null;
    lastKnownState: string | null;
    updatedAt: string;
  }>;
  attachmentIds: string[];
  [key: string]: unknown;
}

export interface Diagnostic {
  recordType: string;
  recordId: string | null;
  code: string;
  message: string;
  recoverable: boolean;
}

export interface AttachmentMetadata {
  id: string;
  originalName: string;
  declaredMediaType: string | null;
  detectedMediaType: string | null;
  originalSize: number;
  storedSize: number;
  sha256: string;
  storageMode: AttachmentStorageMode;
  localRelativePath: string | null;
  createdAt: string;
  updatedAt: string;
  chatId: string | null;
  messageId: string | null;
  processingStatus: string;
}

export interface StorageUsage {
  dataDirectory: string;
  totalBytes: number;
  chatBytes: number;
  attachmentBytes: number;
  partialResponseBytes: number;
  exportCacheBytes: number;
}

export interface Bootstrap {
  settings: ClientSettings;
  profiles: ServerProfile[];
  chats: ChatSummary[];
  diagnostics: Diagnostic[];
  usage: StorageUsage;
  firstRun: boolean;
}

export interface DeletePreview {
  categories: Array<{ name: string; records: number; bytes: number }>;
  totalBytes: number;
  confirmation: string;
}

function nativeOnly(): never {
  throw new Error(
    'Local Tome storage is available in the desktop app. The web preview is read-only and does not use browser storage.',
  );
}

async function command<T>(name: string, args?: Record<string, unknown>) {
  if (!isTauri()) nativeOnly();
  return invoke<T>(name, args);
}

export const storage = {
  bootstrap: () => command<Bootstrap>('bootstrap'),
  createChat: (title: string, serverProfileId: string | null) =>
    command<Chat>('create_chat', { title, serverProfileId }),
  getChat: (id: string) => command<Chat>('get_chat', { id }),
  saveChat: (chat: Chat) => command<Chat>('save_chat', { chat }),
  renameChat: (id: string, title: string) =>
    command<Chat>('rename_chat', { id, title }),
  deleteChat: (id: string) => command<void>('delete_chat', { id }),
  searchChats: (query: string) =>
    command<ChatSummary[]>('search_chats', { query }),
  importChat: (jsonText: string) => command<Chat>('import_chat', { jsonText }),
  exportChat: (id: string) => command<string>('export_chat', { id }),
  saveSettings: (settings: ClientSettings) =>
    command<ClientSettings>('save_settings', { settings }),
  upsertProfile: (profile: ServerProfile) =>
    command<ServerProfile>('upsert_profile', { profile }),
  deleteProfile: (id: string) => command<void>('delete_profile', { id }),
  setEventCursor: (profileId: string, eventId: number) =>
    command<void>('set_event_cursor', { profileId, eventId }),
  storeAttachment: (
    originalName: string,
    declaredMediaType: string | null,
    bytes: Uint8Array,
    storageMode: AttachmentStorageMode,
    chatId: string | null,
    messageId: string | null,
  ) =>
    command<AttachmentMetadata>('store_attachment', {
      originalName,
      declaredMediaType,
      bytes: Array.from(bytes),
      storageMode,
      chatId,
      messageId,
    }),
  deleteAllPreview: () => command<DeletePreview>('delete_all_preview'),
  deleteAllData: (confirmation: string) =>
    command<{
      succeeded: string[];
      failed: string[];
      resetToFirstRun: boolean;
    }>('delete_all_data', { confirmation }),
};

export function newProfile(source?: Partial<ServerProfile>): ServerProfile {
  return {
    id: source?.id ?? crypto.randomUUID(),
    name: source?.name ?? 'My Tome server',
    mode: source?.mode ?? 'lan',
    host: source?.host ?? '127.0.0.1',
    port: source?.port ?? 7331,
    createdAt: source?.createdAt ?? '',
    updatedAt: '',
    lastConnectedAt: null,
    lastEventId: 0,
    compatibility: null,
  };
}

export function chatDownloadName(chat: Pick<Chat, 'id' | 'title'>): string {
  const safeTitle = chat.title
    .normalize('NFKD')
    .replace(/[^a-zA-Z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 60)
    .toLowerCase();
  return `${safeTitle || 'chat'}-${chat.id.slice(0, 8)}.tome.json`;
}
