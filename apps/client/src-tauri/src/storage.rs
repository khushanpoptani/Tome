#![allow(clippy::needless_pass_by_value)] // Tauri commands deserialize owned arguments.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::IpAddr,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

use chrono::{SecondsFormat, Utc};
use flate2::{Compression, write::GzEncoder};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::State;
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;
const MAX_IMPORT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;
const DELETE_CONFIRMATION: &str = "DELETE ALL LOCAL TOME DATA";

#[derive(Debug, thiserror::Error)]
enum StorageError {
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error(
        "unsupported schema version {found} for {record}; this Tome build supports version {supported}"
    )]
    Unsupported {
        record: &'static str,
        found: u64,
        supported: u32,
    },
    #[error("local record was not found")]
    NotFound,
    #[error("local storage operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("local record is malformed: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub schema_version: u32,
    pub default_profile_id: Option<String>,
    pub last_used_profile_id: Option<String>,
    #[serde(default)]
    pub default_models: HashMap<String, String>,
    pub default_temperature: f64,
    pub attachment_storage_mode: AttachmentStorageMode,
    pub appearance: Appearance,
    pub context_display: ContextDisplay,
    pub partial_responses: PartialResponsePreference,
    pub connection: ConnectionPreference,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            default_profile_id: None,
            last_used_profile_id: None,
            default_models: HashMap::new(),
            default_temperature: 0.7,
            attachment_storage_mode: AttachmentStorageMode::Optimized,
            appearance: Appearance::System,
            context_display: ContextDisplay {
                show_meter: true,
                show_manifest: false,
            },
            partial_responses: PartialResponsePreference {
                retain: true,
                show_recovery_banner: true,
            },
            connection: ConnectionPreference {
                reconnect: true,
                retry_delay_ms: 1_500,
            },
            extra: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentStorageMode {
    Optimized,
    PreserveOriginals,
    MetadataOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDisplay {
    pub show_meter: bool,
    pub show_manifest: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialResponsePreference {
    pub retain: bool,
    pub show_recovery_banner: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPreference {
    pub reconnect: bool,
    pub retry_delay_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileStore {
    pub schema_version: u32,
    pub profiles: Vec<ServerProfile>,
}

impl Default for ProfileStore {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            profiles: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProfile {
    pub id: String,
    pub name: String,
    pub mode: NetworkMode,
    pub host: String,
    pub port: u16,
    pub created_at: String,
    pub updated_at: String,
    pub last_connected_at: Option<String>,
    pub last_event_id: i64,
    pub compatibility: Option<CompatibilitySnapshot>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Lan,
    Tailscale,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatibilitySnapshot {
    pub compatible: bool,
    pub protocol: u32,
    pub checked_at: String,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub server_profile_id: Option<String>,
    pub model_id: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub job_references: Vec<JobReference>,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub created_at: String,
    pub parent_message_id: Option<String>,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    pub generation: Option<MessageGeneration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageGeneration {
    pub model_id: String,
    pub model_artifact_sha256: Option<String>,
    pub temperature: f64,
    pub max_output_tokens: u32,
    pub job_id: Option<String>,
    pub state: String,
    pub output_sequence: u64,
    pub usage: Option<Value>,
    pub completion_reason: Option<String>,
    #[serde(default)]
    pub context_warnings: Vec<String>,
    pub retry_of_job_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobReference {
    pub job_id: String,
    pub server_profile_id: String,
    pub message_id: Option<String>,
    pub parent_job_id: Option<String>,
    pub retry_of_job_id: Option<String>,
    pub last_known_state: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub server_profile_id: Option<String>,
    pub model_id: Option<String>,
    pub message_count: usize,
}

impl From<&Chat> for ChatSummary {
    fn from(chat: &Chat) -> Self {
        Self {
            id: chat.id.clone(),
            title: chat.title.clone(),
            created_at: chat.created_at.clone(),
            updated_at: chat.updated_at.clone(),
            server_profile_id: chat.server_profile_id.clone(),
            model_id: chat.model_id.clone(),
            message_count: chat.messages.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentStore {
    pub schema_version: u32,
    pub attachments: Vec<AttachmentMetadata>,
}

impl Default for AttachmentStore {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            attachments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMetadata {
    pub id: String,
    pub original_name: String,
    pub declared_media_type: Option<String>,
    pub detected_media_type: Option<String>,
    pub original_size: u64,
    pub stored_size: u64,
    pub sha256: String,
    pub storage_mode: AttachmentStorageMode,
    pub local_relative_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub chat_id: Option<String>,
    pub message_id: Option<String>,
    pub processing_status: String,
    pub derivative: Option<DerivativeMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivativeMetadata {
    pub media_type: String,
    pub relative_path: String,
    pub byte_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialResponseStore {
    pub schema_version: u32,
    pub responses: Vec<PartialResponseMetadata>,
}

impl Default for PartialResponseStore {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            responses: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialResponseMetadata {
    pub id: String,
    pub chat_id: String,
    pub message_id: String,
    pub job_id: Option<String>,
    pub byte_length: u64,
    pub relative_path: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialResponse {
    pub id: String,
    pub chat_id: String,
    pub message_id: String,
    pub job_id: Option<String>,
    pub content: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCacheStore {
    pub schema_version: u32,
    pub entries: Vec<ExportCacheMetadata>,
}

impl Default for ExportCacheStore {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCacheMetadata {
    pub id: String,
    pub chat_id: String,
    pub relative_path: String,
    pub byte_size: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub record_type: String,
    pub record_id: Option<String>,
    pub code: String,
    pub message: String,
    pub recoverable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsage {
    pub data_directory: String,
    pub total_bytes: u64,
    pub chat_bytes: u64,
    pub attachment_bytes: u64,
    pub partial_response_bytes: u64,
    pub export_cache_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub settings: Settings,
    pub profiles: Vec<ServerProfile>,
    pub chats: Vec<ChatSummary>,
    pub diagnostics: Vec<Diagnostic>,
    pub usage: StorageUsage,
    pub first_run: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePreview {
    pub categories: Vec<DeleteCategory>,
    pub total_bytes: u64,
    pub confirmation: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCategory {
    pub name: String,
    pub records: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSummary {
    pub succeeded: Vec<String>,
    pub failed: Vec<String>,
    pub reset_to_first_run: bool,
}

pub struct ClientStorage {
    root: PathBuf,
    gate: Mutex<()>,
}

impl ClientStorage {
    pub fn open(root: PathBuf) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        validate_root(&root)?;
        if root.exists() && fs::symlink_metadata(&root)?.file_type().is_symlink() {
            return Err("application data root cannot be a symbolic link".into());
        }
        fs::create_dir_all(&root)?;
        let root = fs::canonicalize(root)?;
        let storage = Self {
            root,
            gate: Mutex::new(()),
        };
        storage.ensure_layout()?;
        Ok(storage)
    }

    fn ensure_layout(&self) -> Result<()> {
        for relative in [
            "chats",
            "attachments/content",
            "attachments/partial",
            "partials/content",
            "export-cache/content",
            "quarantine",
        ] {
            fs::create_dir_all(self.safe_path(relative)?)?;
        }
        Ok(())
    }

    fn safe_path(&self, relative: &str) -> Result<PathBuf> {
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path.components().any(|component| {
                !matches!(component, Component::Normal(_))
                    || component.as_os_str().to_string_lossy().contains('\0')
            })
        {
            return Err(StorageError::Invalid("managed path"));
        }
        let candidate = self.root.join(relative_path);
        if !candidate.starts_with(&self.root) {
            return Err(StorageError::Invalid("managed path"));
        }
        Ok(candidate)
    }

    fn settings_path(&self) -> Result<PathBuf> {
        self.safe_path("settings.json")
    }

    fn profiles_path(&self) -> Result<PathBuf> {
        self.safe_path("profiles.json")
    }

    fn chat_path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        self.safe_path(&format!("chats/{id}.json"))
    }

    fn bootstrap(&self) -> Result<Bootstrap> {
        let _lock = self.gate.lock().expect("storage lock poisoned");
        self.bootstrap_locked()
    }

    fn bootstrap_locked(&self) -> Result<Bootstrap> {
        self.ensure_layout()?;
        let mut diagnostics = Vec::new();
        let settings = self.load_or_default_validated::<Settings>(
            &self.settings_path()?,
            "settings",
            Settings::default,
            &mut diagnostics,
            validate_settings,
        );
        let profile_store = self.load_or_default::<ProfileStore>(
            &self.profiles_path()?,
            "profiles",
            ProfileStore::default,
            &mut diagnostics,
        );
        let _attachments = self.load_or_default_validated::<AttachmentStore>(
            &self.safe_path("attachments/index.json")?,
            "attachments",
            AttachmentStore::default,
            &mut diagnostics,
            validate_attachment_store,
        );
        let _partials = self.load_or_default_validated::<PartialResponseStore>(
            &self.safe_path("partials/index.json")?,
            "partial responses",
            PartialResponseStore::default,
            &mut diagnostics,
            validate_partial_store,
        );
        let _export_cache = self.load_or_default_validated::<ExportCacheStore>(
            &self.safe_path("export-cache/index.json")?,
            "export cache",
            ExportCacheStore::default,
            &mut diagnostics,
            validate_export_cache,
        );
        let deletion_diagnostic = self.safe_path("delete-diagnostic.json")?;
        if deletion_diagnostic.exists() {
            diagnostics.push(Diagnostic {
                record_type: "delete all".into(),
                record_id: None,
                code: "partial_deletion".into(),
                message: "A previous delete-all operation could not remove every managed category. Review the operation summary and retry.".into(),
                recoverable: true,
            });
        }
        let mut seen = HashSet::new();
        let profiles: Vec<ServerProfile> = profile_store
            .profiles
            .into_iter()
            .filter(|profile| {
                let valid = validate_profile(profile).is_ok() && seen.insert(profile.id.clone());
                if !valid {
                    diagnostics.push(Diagnostic {
                        record_type: "profile".into(),
                        record_id: Some(profile.id.clone()),
                        code: "invalid_or_duplicate_id".into(),
                        message: "A saved server profile was ignored because its identifier or fields are invalid.".into(),
                        recoverable: true,
                    });
                }
                valid
            })
            .collect();
        let chats = self.load_chat_summaries(&mut diagnostics)?;
        let usage = self.usage()?;
        let first_run = chats.is_empty() && profiles.is_empty() && !self.settings_path()?.exists();
        Ok(Bootstrap {
            settings,
            profiles,
            chats,
            diagnostics,
            usage,
            first_run,
        })
    }

    fn load_or_default<T: DeserializeOwned>(
        &self,
        path: &Path,
        record_type: &'static str,
        default: impl FnOnce() -> T,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> T {
        if !path.exists() && !temp_path(path).exists() && !backup_path(path).exists() {
            return default();
        }
        match recover_json::<T>(path, record_type) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.quarantine(path, record_type);
                diagnostics.push(Diagnostic {
                    record_type: record_type.into(),
                    record_id: None,
                    code: "corrupt_local_record".into(),
                    message: error.to_string(),
                    recoverable: true,
                });
                default()
            }
        }
    }

    fn load_or_default_validated<T: DeserializeOwned>(
        &self,
        path: &Path,
        record_type: &'static str,
        default: impl FnOnce() -> T,
        diagnostics: &mut Vec<Diagnostic>,
        validator: fn(&T) -> Result<()>,
    ) -> T {
        if !path.exists() && !temp_path(path).exists() && !backup_path(path).exists() {
            return default();
        }
        match recover_json_validated::<T>(path, record_type, validator) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.quarantine(path, record_type);
                diagnostics.push(Diagnostic {
                    record_type: record_type.into(),
                    record_id: None,
                    code: "invalid_local_record".into(),
                    message: error.to_string(),
                    recoverable: true,
                });
                default()
            }
        }
    }

    fn load_chat_summaries(&self, diagnostics: &mut Vec<Diagnostic>) -> Result<Vec<ChatSummary>> {
        let chats_dir = self.safe_path("chats")?;
        let mut chats = Vec::new();
        let mut seen = HashSet::new();
        let mut candidates = HashSet::new();
        for entry in fs::read_dir(&chats_dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
                if Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
                {
                    candidates.insert(path);
                } else if let Some(base) = name
                    .strip_suffix(".json.tmp")
                    .or_else(|| name.strip_suffix(".json.bak"))
                {
                    candidates.insert(chats_dir.join(format!("{base}.json")));
                }
            }
        }
        for path in candidates {
            let file_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            let loaded =
                recover_json_validated::<Chat>(&path, "chat", validate_chat).and_then(|chat| {
                    if chat.id != file_id || !seen.insert(chat.id.clone()) {
                        return Err(StorageError::Invalid("duplicate or mismatched chat ID"));
                    }
                    Ok(chat)
                });
            match loaded {
                Ok(chat) => chats.push(ChatSummary::from(&chat)),
                Err(error) => {
                    let _ = self.quarantine(&path, "chat");
                    diagnostics.push(Diagnostic {
                        record_type: "chat".into(),
                        record_id: validate_id(file_id).ok().map(|()| file_id.to_owned()),
                        code: "corrupt_chat".into(),
                        message: error.to_string(),
                        recoverable: true,
                    });
                }
            }
        }
        chats.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(chats)
    }

    fn quarantine(&self, path: &Path, kind: &str) -> Result<()> {
        for candidate in [path.to_path_buf(), temp_path(path), backup_path(path)] {
            if !candidate.exists() {
                continue;
            }
            let file_name = candidate
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("record");
            let target = self.safe_path(&format!(
                "quarantine/{kind}-{file_name}-{}.bad",
                Uuid::now_v7()
            ))?;
            fs::rename(candidate, target)?;
        }
        Ok(())
    }

    fn usage(&self) -> Result<StorageUsage> {
        let chat_bytes = directory_size(&self.safe_path("chats")?)?;
        let attachment_bytes = directory_size(&self.safe_path("attachments")?)?;
        let partial_response_bytes = directory_size(&self.safe_path("partials")?)?;
        let export_cache_bytes = directory_size(&self.safe_path("export-cache")?)?;
        Ok(StorageUsage {
            data_directory: self.root.display().to_string(),
            total_bytes: directory_size(&self.root)?,
            chat_bytes,
            attachment_bytes,
            partial_response_bytes,
            export_cache_bytes,
        })
    }
}

#[tauri::command]
pub fn bootstrap(storage: State<'_, ClientStorage>) -> std::result::Result<Bootstrap, String> {
    storage.bootstrap().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn create_chat(
    storage: State<'_, ClientStorage>,
    title: String,
    server_profile_id: Option<String>,
) -> std::result::Result<Chat, String> {
    create_chat_inner(&storage, title, server_profile_id).map_err(|error| error.to_string())
}

fn create_chat_inner(
    storage: &ClientStorage,
    title: String,
    server_profile_id: Option<String>,
) -> Result<Chat> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    validate_title(&title)?;
    if let Some(id) = &server_profile_id {
        validate_id(id)?;
    }
    let now = now();
    let chat = Chat {
        schema_version: SCHEMA_VERSION,
        id: Uuid::now_v7().to_string(),
        title: title.trim().to_owned(),
        created_at: now.clone(),
        updated_at: now,
        server_profile_id,
        model_id: None,
        messages: Vec::new(),
        job_references: Vec::new(),
        attachment_ids: Vec::new(),
        extra: serde_json::Map::new(),
    };
    atomic_write_json(&storage.chat_path(&chat.id)?, &chat)?;
    Ok(chat)
}

#[tauri::command]
pub fn get_chat(
    storage: State<'_, ClientStorage>,
    id: String,
) -> std::result::Result<Chat, String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let path = storage.chat_path(&id).map_err(|error| error.to_string())?;
    recover_json_validated::<Chat>(&path, "chat", validate_chat).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_chat(
    storage: State<'_, ClientStorage>,
    mut chat: Chat,
) -> std::result::Result<Chat, String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    validate_chat(&chat).map_err(|error| error.to_string())?;
    let path = storage
        .chat_path(&chat.id)
        .map_err(|error| error.to_string())?;
    if !path.exists() {
        return Err("local chat was not found".into());
    }
    chat.updated_at = now();
    atomic_write_json(&path, &chat).map_err(|error| error.to_string())?;
    Ok(chat)
}

#[tauri::command]
pub fn rename_chat(
    storage: State<'_, ClientStorage>,
    id: String,
    title: String,
) -> std::result::Result<Chat, String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    validate_title(&title).map_err(|error| error.to_string())?;
    let path = storage.chat_path(&id).map_err(|error| error.to_string())?;
    let mut chat = recover_json_validated::<Chat>(&path, "chat", validate_chat)
        .map_err(|error| error.to_string())?;
    title.trim().clone_into(&mut chat.title);
    chat.updated_at = now();
    atomic_write_json(&path, &chat).map_err(|error| error.to_string())?;
    Ok(chat)
}

#[tauri::command]
pub fn delete_chat(
    storage: State<'_, ClientStorage>,
    id: String,
) -> std::result::Result<(), String> {
    delete_chat_inner(&storage, &id).map_err(|error| error.to_string())
}

fn delete_chat_inner(storage: &ClientStorage, id: &str) -> Result<()> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let path = storage.chat_path(id)?;
    let chat = recover_json_validated::<Chat>(&path, "chat", validate_chat)?;
    let attachment_path = storage.safe_path("attachments/index.json")?;
    let mut diagnostics = Vec::new();
    let mut attachments = storage.load_or_default_validated::<AttachmentStore>(
        &attachment_path,
        "attachments",
        AttachmentStore::default,
        &mut diagnostics,
        validate_attachment_store,
    );
    let removed: Vec<_> = attachments
        .attachments
        .iter()
        .filter(|item| item.chat_id.as_deref() == Some(id))
        .cloned()
        .collect();
    attachments
        .attachments
        .retain(|item| item.chat_id.as_deref() != Some(id));
    for attachment in removed {
        if let Some(relative) = attachment.local_relative_path {
            remove_managed_file(storage, &relative)?;
        }
        if let Some(derivative) = attachment.derivative {
            remove_managed_file(storage, &derivative.relative_path)?;
        }
    }
    for attachment_id in chat.attachment_ids {
        validate_id(&attachment_id)?;
    }
    atomic_write_json(&attachment_path, &attachments)?;
    let partial_path = storage.safe_path("partials/index.json")?;
    let mut partials = storage.load_or_default_validated::<PartialResponseStore>(
        &partial_path,
        "partial responses",
        PartialResponseStore::default,
        &mut diagnostics,
        validate_partial_store,
    );
    let removed_partials: Vec<_> = partials
        .responses
        .iter()
        .filter(|item| item.chat_id == id)
        .cloned()
        .collect();
    partials.responses.retain(|item| item.chat_id != id);
    for partial in removed_partials {
        remove_managed_file(storage, &partial.relative_path)?;
    }
    atomic_write_json(&partial_path, &partials)?;
    let export_path = storage.safe_path("export-cache/index.json")?;
    let mut exports = storage.load_or_default_validated::<ExportCacheStore>(
        &export_path,
        "export cache",
        ExportCacheStore::default,
        &mut diagnostics,
        validate_export_cache,
    );
    let removed_exports: Vec<_> = exports
        .entries
        .iter()
        .filter(|item| item.chat_id == id)
        .cloned()
        .collect();
    exports.entries.retain(|item| item.chat_id != id);
    for export in removed_exports {
        remove_managed_file(storage, &export.relative_path)?;
    }
    atomic_write_json(&export_path, &exports)?;
    fs::remove_file(path)?;
    for sibling in [
        backup_path(&storage.chat_path(id)?),
        temp_path(&storage.chat_path(id)?),
    ] {
        if sibling.exists() {
            fs::remove_file(sibling)?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn search_chats(
    storage: State<'_, ClientStorage>,
    query: String,
) -> std::result::Result<Vec<ChatSummary>, String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let needle = query.trim().to_lowercase();
    if needle.len() > 256 {
        return Err("search query is too long".into());
    }
    let mut diagnostics = Vec::new();
    let summaries = storage
        .load_chat_summaries(&mut diagnostics)
        .map_err(|error| error.to_string())?;
    if needle.is_empty() {
        return Ok(summaries);
    }
    let mut result = Vec::new();
    for summary in summaries {
        let path = storage
            .chat_path(&summary.id)
            .map_err(|error| error.to_string())?;
        if let Ok(chat) = recover_json_validated::<Chat>(&path, "chat", validate_chat) {
            let matches = chat.title.to_lowercase().contains(&needle)
                || chat
                    .messages
                    .iter()
                    .any(|message| message.content.to_lowercase().contains(&needle));
            if matches {
                result.push(summary);
            }
        }
    }
    Ok(result)
}

#[tauri::command]
pub fn import_chat(
    storage: State<'_, ClientStorage>,
    json_text: String,
) -> std::result::Result<Chat, String> {
    import_chat_inner(&storage, &json_text).map_err(|error| error.to_string())
}

fn import_chat_inner(storage: &ClientStorage, json_text: &str) -> Result<Chat> {
    if json_text.len() > MAX_IMPORT_BYTES {
        return Err(StorageError::Invalid("chat import size"));
    }
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let value: Value = serde_json::from_str(json_text)?;
    let value = migrate_value(value, "chat")?;
    let mut chat: Chat = serde_json::from_value(value)?;
    validate_chat(&chat)?;
    let original_id = chat.id.clone();
    if storage.chat_path(&chat.id)?.exists() {
        chat.id = Uuid::now_v7().to_string();
        for reference in &mut chat.job_references {
            reference.message_id = reference.message_id.take();
        }
        chat.extra
            .insert("importedFromChatId".into(), Value::String(original_id));
    }
    chat.updated_at = now();
    atomic_write_json(&storage.chat_path(&chat.id)?, &chat)?;
    Ok(chat)
}

#[tauri::command]
pub fn export_chat(
    storage: State<'_, ClientStorage>,
    id: String,
) -> std::result::Result<String, String> {
    export_chat_inner(&storage, &id).map_err(|error| error.to_string())
}

fn export_chat_inner(storage: &ClientStorage, id: &str) -> Result<String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let path = storage.chat_path(id)?;
    let chat = recover_json_validated::<Chat>(&path, "chat", validate_chat)?;
    let mut value = serde_json::to_value(chat)?;
    redact_sensitive(&mut value);
    serde_json::to_string_pretty(&value)
        .map(|text| format!("{text}\n"))
        .map_err(StorageError::from)
}

#[tauri::command]
pub fn export_chat_file(
    storage: State<'_, ClientStorage>,
    id: String,
    destination: String,
) -> std::result::Result<String, String> {
    export_chat_file_inner(&storage, &id, Path::new(&destination))
        .map(|path| path.display().to_string())
        .map_err(|error| error.to_string())
}

fn export_chat_file_inner(
    storage: &ClientStorage,
    id: &str,
    destination: &Path,
) -> Result<PathBuf> {
    if !destination.is_absolute() || destination.file_name().is_none() {
        return Err(StorageError::Invalid("export destination"));
    }
    let parent = destination
        .parent()
        .ok_or(StorageError::Invalid("export destination"))?;
    let canonical_parent = fs::canonicalize(parent)?;
    if canonical_parent.starts_with(&storage.root) {
        return Err(StorageError::Invalid(
            "export destination inside managed data root",
        ));
    }
    let file_name = destination
        .file_name()
        .ok_or(StorageError::Invalid("export file name"))?;
    let destination = canonical_parent.join(file_name);
    let text = export_chat_inner(storage, id)?;
    let temporary = canonical_parent.join(format!(".tome-export-{}.tmp", Uuid::now_v7()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    if let Err(error) = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    drop(file);
    if destination.exists() {
        fs::remove_file(&destination)?;
    }
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    sync_directory(&canonical_parent);
    Ok(destination)
}

#[tauri::command]
pub fn save_settings(
    storage: State<'_, ClientStorage>,
    settings: Settings,
) -> std::result::Result<Settings, String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    validate_settings(&settings).map_err(|error| error.to_string())?;
    atomic_write_json(
        &storage.settings_path().map_err(|error| error.to_string())?,
        &settings,
    )
    .map_err(|error| error.to_string())?;
    Ok(settings)
}

#[tauri::command]
pub fn upsert_profile(
    storage: State<'_, ClientStorage>,
    mut profile: ServerProfile,
) -> std::result::Result<ServerProfile, String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    if profile.id.is_empty() {
        profile.id = Uuid::now_v7().to_string();
    }
    let timestamp = now();
    if profile.created_at.is_empty() {
        profile.created_at.clone_from(&timestamp);
    }
    profile.updated_at = timestamp;
    validate_profile(&profile).map_err(|error| error.to_string())?;
    let path = storage.profiles_path().map_err(|error| error.to_string())?;
    let mut diagnostics = Vec::new();
    let mut store = storage.load_or_default::<ProfileStore>(
        &path,
        "profiles",
        ProfileStore::default,
        &mut diagnostics,
    );
    if let Some(saved) = store
        .profiles
        .iter_mut()
        .find(|saved| saved.id == profile.id)
    {
        *saved = profile.clone();
    } else {
        store.profiles.push(profile.clone());
    }
    store.profiles.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.id.cmp(&right.id))
    });
    atomic_write_json(&path, &store).map_err(|error| error.to_string())?;
    Ok(profile)
}

#[tauri::command]
pub fn delete_profile(
    storage: State<'_, ClientStorage>,
    id: String,
) -> std::result::Result<(), String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    validate_id(&id).map_err(|error| error.to_string())?;
    let path = storage.profiles_path().map_err(|error| error.to_string())?;
    let mut diagnostics = Vec::new();
    let mut store = storage.load_or_default::<ProfileStore>(
        &path,
        "profiles",
        ProfileStore::default,
        &mut diagnostics,
    );
    store.profiles.retain(|profile| profile.id != id);
    atomic_write_json(&path, &store).map_err(|error| error.to_string())?;
    let settings_path = storage.settings_path().map_err(|error| error.to_string())?;
    let mut settings = storage.load_or_default::<Settings>(
        &settings_path,
        "settings",
        Settings::default,
        &mut diagnostics,
    );
    if settings.default_profile_id.as_deref() == Some(&id) {
        settings.default_profile_id = None;
    }
    if settings.last_used_profile_id.as_deref() == Some(&id) {
        settings.last_used_profile_id = None;
    }
    settings.default_models.remove(&id);
    atomic_write_json(&settings_path, &settings).map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn set_event_cursor(
    storage: State<'_, ClientStorage>,
    profile_id: String,
    event_id: i64,
) -> std::result::Result<(), String> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    validate_id(&profile_id).map_err(|error| error.to_string())?;
    if event_id < 0 {
        return Err("event cursor cannot be negative".into());
    }
    let path = storage.profiles_path().map_err(|error| error.to_string())?;
    let mut diagnostics = Vec::new();
    let mut store = storage.load_or_default::<ProfileStore>(
        &path,
        "profiles",
        ProfileStore::default,
        &mut diagnostics,
    );
    let profile = store
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "profile was not found".to_owned())?;
    profile.last_event_id = profile.last_event_id.max(event_id);
    profile.updated_at = now();
    atomic_write_json(&path, &store).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn store_attachment(
    storage: State<'_, ClientStorage>,
    original_name: String,
    declared_media_type: Option<String>,
    bytes: Vec<u8>,
    storage_mode: AttachmentStorageMode,
    chat_id: Option<String>,
    message_id: Option<String>,
) -> std::result::Result<AttachmentMetadata, String> {
    store_attachment_inner(
        &storage,
        original_name,
        declared_media_type,
        bytes,
        storage_mode,
        chat_id,
        message_id,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_partial_response(
    storage: State<'_, ClientStorage>,
    chat_id: String,
    message_id: String,
    job_id: Option<String>,
    content: String,
) -> std::result::Result<PartialResponse, String> {
    save_partial_response_inner(&storage, &chat_id, &message_id, job_id.as_deref(), &content)
        .map_err(|error| error.to_string())
}

fn save_partial_response_inner(
    storage: &ClientStorage,
    chat_id: &str,
    message_id: &str,
    job_id: Option<&str>,
    content: &str,
) -> Result<PartialResponse> {
    validate_id(chat_id)?;
    validate_id(message_id)?;
    if let Some(id) = job_id {
        validate_id(id)?;
    }
    if content.len() > MAX_IMPORT_BYTES {
        return Err(StorageError::Invalid("partial response size"));
    }
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let index_path = storage.safe_path("partials/index.json")?;
    let mut diagnostics = Vec::new();
    let mut index = storage.load_or_default_validated::<PartialResponseStore>(
        &index_path,
        "partial responses",
        PartialResponseStore::default,
        &mut diagnostics,
        validate_partial_store,
    );
    let previous = index
        .responses
        .iter()
        .find(|item| item.chat_id == chat_id && item.message_id == message_id)
        .cloned();
    let response_id = previous
        .as_ref()
        .map_or_else(|| Uuid::now_v7().to_string(), |item| item.id.clone());
    let checkpoint_id = Uuid::now_v7();
    let relative_path = format!("partials/content/{response_id}-{checkpoint_id}.txt");
    atomic_write_bytes(&storage.safe_path(&relative_path)?, content.as_bytes())?;
    let timestamp = now();
    let metadata = PartialResponseMetadata {
        id: response_id.clone(),
        chat_id: chat_id.to_owned(),
        message_id: message_id.to_owned(),
        job_id: job_id.map(str::to_owned),
        byte_length: content.len() as u64,
        relative_path: relative_path.clone(),
        updated_at: timestamp.clone(),
    };
    index
        .responses
        .retain(|item| !(item.chat_id == chat_id && item.message_id == message_id));
    index.responses.push(metadata);
    atomic_write_json(&index_path, &index)?;
    if let Some(previous) = previous
        && previous.relative_path != relative_path
    {
        remove_managed_file(storage, &previous.relative_path)?;
    }
    Ok(PartialResponse {
        id: response_id,
        chat_id: chat_id.to_owned(),
        message_id: message_id.to_owned(),
        job_id: job_id.map(str::to_owned),
        content: content.to_owned(),
        updated_at: timestamp,
    })
}

#[tauri::command]
pub fn load_partial_responses(
    storage: State<'_, ClientStorage>,
    chat_id: String,
) -> std::result::Result<Vec<PartialResponse>, String> {
    validate_id(&chat_id).map_err(|error| error.to_string())?;
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let mut diagnostics = Vec::new();
    let index = storage.load_or_default_validated::<PartialResponseStore>(
        &storage
            .safe_path("partials/index.json")
            .map_err(|error| error.to_string())?,
        "partial responses",
        PartialResponseStore::default,
        &mut diagnostics,
        validate_partial_store,
    );
    let mut responses = Vec::new();
    for item in index
        .responses
        .into_iter()
        .filter(|item| item.chat_id == chat_id)
    {
        let bytes = fs::read(
            storage
                .safe_path(&item.relative_path)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if bytes.len() as u64 != item.byte_length || bytes.len() > MAX_IMPORT_BYTES {
            return Err("partial response content is invalid".to_owned());
        }
        let content = String::from_utf8(bytes)
            .map_err(|_| "partial response content is not UTF-8".to_owned())?;
        responses.push(PartialResponse {
            id: item.id,
            chat_id: item.chat_id,
            message_id: item.message_id,
            job_id: item.job_id,
            content,
            updated_at: item.updated_at,
        });
    }
    Ok(responses)
}

#[tauri::command]
pub fn clear_partial_response(
    storage: State<'_, ClientStorage>,
    chat_id: String,
    message_id: String,
) -> std::result::Result<(), String> {
    validate_id(&chat_id).map_err(|error| error.to_string())?;
    validate_id(&message_id).map_err(|error| error.to_string())?;
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let index_path = storage
        .safe_path("partials/index.json")
        .map_err(|error| error.to_string())?;
    let mut diagnostics = Vec::new();
    let mut index = storage.load_or_default_validated::<PartialResponseStore>(
        &index_path,
        "partial responses",
        PartialResponseStore::default,
        &mut diagnostics,
        validate_partial_store,
    );
    let removed = index
        .responses
        .iter()
        .filter(|item| item.chat_id == chat_id && item.message_id == message_id)
        .cloned()
        .collect::<Vec<_>>();
    index
        .responses
        .retain(|item| !(item.chat_id == chat_id && item.message_id == message_id));
    for item in removed {
        remove_managed_file(&storage, &item.relative_path).map_err(|error| error.to_string())?;
    }
    atomic_write_json(&index_path, &index).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn write_interaction_export(
    storage: State<'_, ClientStorage>,
    destination: String,
    content: String,
) -> std::result::Result<String, String> {
    if content.len() > MAX_IMPORT_BYTES {
        return Err("interaction export exceeds 16 MiB".to_owned());
    }
    let destination = Path::new(&destination);
    if !destination.is_absolute() || destination.file_name().is_none() {
        return Err("invalid export destination".to_owned());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "invalid export destination".to_owned())?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| error.to_string())?;
    if canonical_parent.starts_with(&storage.root) {
        return Err("export destination must be outside managed Tome data".to_owned());
    }
    let final_path = canonical_parent.join(
        destination
            .file_name()
            .ok_or_else(|| "invalid export file name".to_owned())?,
    );
    let sanitized = sanitize_export_text(&content);
    let temporary = canonical_parent.join(format!(".tome-export-{}.tmp", Uuid::now_v7()));
    atomic_write_bytes(&temporary, sanitized.as_bytes()).map_err(|error| error.to_string())?;
    if final_path.exists() {
        fs::remove_file(&final_path).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, &final_path).map_err(|error| error.to_string())?;
    sync_directory(&canonical_parent);
    Ok(final_path.display().to_string())
}

fn store_attachment_inner(
    storage: &ClientStorage,
    original_name: String,
    declared_media_type: Option<String>,
    bytes: Vec<u8>,
    storage_mode: AttachmentStorageMode,
    chat_id: Option<String>,
    message_id: Option<String>,
) -> Result<AttachmentMetadata> {
    if original_name.trim().is_empty()
        || original_name.len() > 255
        || original_name.contains(['/', '\\', '\0'])
    {
        return Err(StorageError::Invalid("attachment name"));
    }
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(StorageError::Invalid("attachment size"));
    }
    if let Some(id) = &chat_id {
        validate_id(id)?;
    }
    if let Some(id) = &message_id {
        validate_id(id)?;
    }
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let id = Uuid::now_v7().to_string();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let (relative, stored_size) = match storage_mode {
        AttachmentStorageMode::MetadataOnly => (None, 0),
        AttachmentStorageMode::PreserveOriginals => {
            let relative = format!("attachments/content/{id}.original");
            atomic_write_bytes(&storage.safe_path(&relative)?, &bytes)?;
            (Some(relative), bytes.len() as u64)
        }
        AttachmentStorageMode::Optimized => {
            let relative = format!("attachments/content/{id}.gz");
            let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
            for chunk in bytes.chunks(64 * 1024) {
                encoder.write_all(chunk)?;
            }
            let compressed = encoder.finish()?;
            atomic_write_bytes(&storage.safe_path(&relative)?, &compressed)?;
            (Some(relative), compressed.len() as u64)
        }
    };
    let timestamp = now();
    let metadata = AttachmentMetadata {
        id,
        original_name,
        declared_media_type,
        detected_media_type: detect_media_type(&bytes).map(str::to_owned),
        original_size: bytes.len() as u64,
        stored_size,
        sha256: hash,
        storage_mode,
        local_relative_path: relative,
        created_at: timestamp.clone(),
        updated_at: timestamp,
        chat_id,
        message_id,
        processing_status: "not_started".into(),
        derivative: None,
    };
    let index_path = storage.safe_path("attachments/index.json")?;
    let mut diagnostics = Vec::new();
    let mut index = storage.load_or_default_validated::<AttachmentStore>(
        &index_path,
        "attachments",
        AttachmentStore::default,
        &mut diagnostics,
        validate_attachment_store,
    );
    index.attachments.push(metadata.clone());
    atomic_write_json(&index_path, &index)?;
    Ok(metadata)
}

#[tauri::command]
pub fn delete_all_preview(
    storage: State<'_, ClientStorage>,
) -> std::result::Result<DeletePreview, String> {
    delete_preview_inner(&storage).map_err(|error| error.to_string())
}

fn delete_preview_inner(storage: &ClientStorage) -> Result<DeletePreview> {
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    let categories = [
        ("Chats", "chats"),
        ("Attachments", "attachments"),
        ("Partial responses", "partials"),
        ("Export cache", "export-cache"),
        ("Saved connections", "profiles.json"),
        ("Settings", "settings.json"),
        ("Recovery diagnostics", "delete-diagnostic.json"),
    ]
    .into_iter()
    .map(|(name, relative)| {
        let path = storage.safe_path(relative)?;
        Ok(DeleteCategory {
            name: name.into(),
            records: count_files(&path)?,
            bytes: path_size(&path)?,
        })
    })
    .collect::<Result<Vec<_>>>()?;
    Ok(DeletePreview {
        total_bytes: categories.iter().map(|category| category.bytes).sum(),
        categories,
        confirmation: DELETE_CONFIRMATION,
    })
}

#[tauri::command]
pub fn delete_all_data(
    storage: State<'_, ClientStorage>,
    confirmation: String,
) -> std::result::Result<DeleteSummary, String> {
    delete_all_inner(&storage, &confirmation).map_err(|error| error.to_string())
}

fn delete_all_inner(storage: &ClientStorage, confirmation: &str) -> Result<DeleteSummary> {
    if confirmation != DELETE_CONFIRMATION {
        return Err(StorageError::Invalid("delete-all confirmation"));
    }
    let _lock = storage.gate.lock().expect("storage lock poisoned");
    validate_delete_root(&storage.root)?;
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for relative in [
        "chats",
        "attachments",
        "partials",
        "export-cache",
        "quarantine",
        "profiles.json",
        "settings.json",
        "delete-diagnostic.json",
    ] {
        let path = storage.safe_path(relative)?;
        let result = match fs::symlink_metadata(&path) {
            // Unlink the managed directory entry itself; never follow it to its target.
            Ok(metadata) if metadata.file_type().is_symlink() => fs::remove_file(&path),
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&path),
            Ok(_) => fs::remove_file(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => succeeded.push(relative.into()),
            Err(error) => failed.push(format!("{relative}: {error}")),
        }
        for sibling in [temp_path(&path), backup_path(&path)] {
            if sibling.exists() {
                let _ = fs::remove_file(sibling);
            }
        }
    }
    storage.ensure_layout()?;
    if failed.is_empty() {
        let diagnostic = storage.safe_path("delete-diagnostic.json")?;
        if diagnostic.exists() {
            fs::remove_file(diagnostic)?;
        }
    } else {
        atomic_write_json(
            &storage.safe_path("delete-diagnostic.json")?,
            &json!({
                "schemaVersion": SCHEMA_VERSION,
                "occurredAt": now(),
                "failedCategories": failed.clone(),
            }),
        )?;
    }
    let reset_to_first_run = failed.is_empty()
        && [
            "chats",
            "attachments/content",
            "partials/content",
            "export-cache/content",
        ]
        .into_iter()
        .all(|relative| {
            matches!(
                storage
                    .safe_path(relative)
                    .and_then(|path| count_files(&path)),
                Ok(0)
            )
        });
    Ok(DeleteSummary {
        succeeded,
        failed,
        reset_to_first_run,
    })
}

fn validate_root(path: &Path) -> std::io::Result<()> {
    if path.as_os_str().is_empty() || path.parent().is_none() || path == Path::new("/") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "application data root is too broad",
        ));
    }
    Ok(())
}

fn validate_delete_root(path: &Path) -> Result<()> {
    validate_root(path)?;
    let canonical = fs::canonicalize(path)?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if home.as_deref() == Some(canonical.as_path())
        || canonical.parent().is_none()
        || canonical.components().count() < 3
    {
        return Err(StorageError::Invalid("delete-all root"));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(StorageError::Invalid("symlinked data root"));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    let parsed = Uuid::parse_str(id).map_err(|_| StorageError::Invalid("stable ID"))?;
    if parsed.to_string() != id {
        return Err(StorageError::Invalid("canonical stable ID"));
    }
    Ok(())
}

fn validate_opaque_reference(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.contains(['/', '\\', '\0'])
        || matches!(value, "." | "..")
    {
        return Err(StorageError::Invalid("opaque reference"));
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() || title.len() > 200 || title.contains('\0') {
        return Err(StorageError::Invalid("chat title"));
    }
    Ok(())
}

fn validate_profile(profile: &ServerProfile) -> Result<()> {
    validate_id(&profile.id)?;
    if profile.name.trim().is_empty() || profile.name.len() > 100 {
        return Err(StorageError::Invalid("profile name"));
    }
    let host = profile.host.trim();
    if host.is_empty()
        || host.len() > 253
        || host.contains(['/', '\\', '?', '#', '\0'])
        || host.starts_with("http:")
        || host.starts_with("https:")
        || matches!(host, "0.0.0.0" | "::" | "[::]")
    {
        return Err(StorageError::Invalid("private server host"));
    }
    if profile.port == 0 {
        return Err(StorageError::Invalid("server port"));
    }
    validate_timestamp(&profile.created_at)?;
    validate_timestamp(&profile.updated_at)?;
    if let Some(timestamp) = &profile.last_connected_at {
        validate_timestamp(timestamp)?;
    }
    if profile.last_event_id < 0 {
        return Err(StorageError::Invalid("event cursor"));
    }
    if let Some(compatibility) = &profile.compatibility {
        validate_timestamp(&compatibility.checked_at)?;
    }
    if let Ok(address) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        let allowed = match (profile.mode, address) {
            (NetworkMode::Lan, IpAddr::V4(address)) => {
                address.is_private() || address.is_loopback() || address.is_link_local()
            }
            (NetworkMode::Lan, IpAddr::V6(address)) => {
                address.is_loopback()
                    || address.is_unique_local()
                    || address.is_unicast_link_local()
            }
            (NetworkMode::Tailscale, IpAddr::V4(address)) => {
                let octets = address.octets();
                octets[0] == 100 && (64..=127).contains(&octets[1])
            }
            (NetworkMode::Tailscale, IpAddr::V6(address)) => {
                let segments = address.segments();
                segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
            }
        };
        if !allowed {
            return Err(StorageError::Invalid("private server address"));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_chat(chat: &Chat) -> Result<()> {
    if chat.schema_version != SCHEMA_VERSION {
        return Err(StorageError::Unsupported {
            record: "chat",
            found: u64::from(chat.schema_version),
            supported: SCHEMA_VERSION,
        });
    }
    validate_id(&chat.id)?;
    validate_title(&chat.title)?;
    if chat
        .extra
        .iter()
        .any(|(key, value)| sensitive_key(key) || contains_sensitive_fields(value))
    {
        return Err(StorageError::Invalid("credential-like chat field"));
    }
    validate_timestamp(&chat.created_at)?;
    validate_timestamp(&chat.updated_at)?;
    if let Some(id) = &chat.server_profile_id {
        validate_id(id)?;
    }
    if let Some(id) = &chat.model_id {
        validate_opaque_reference(id)?;
    }
    let mut ids = HashSet::new();
    for message in &chat.messages {
        validate_id(&message.id)?;
        if !ids.insert(&message.id) {
            return Err(StorageError::Invalid("duplicate message ID"));
        }
        if message.content.len() > MAX_IMPORT_BYTES {
            return Err(StorageError::Invalid("message size"));
        }
        validate_timestamp(&message.created_at)?;
        if let Some(id) = &message.parent_message_id {
            validate_id(id)?;
        }
        for id in &message.attachment_ids {
            validate_id(id)?;
        }
        if let Some(generation) = &message.generation {
            validate_opaque_reference(&generation.model_id)?;
            if let Some(hash) = &generation.model_artifact_sha256
                && (hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err(StorageError::Invalid("model artifact hash"));
            }
            if !generation.temperature.is_finite()
                || !(0.0..=2.0).contains(&generation.temperature)
                || generation.max_output_tokens == 0
                || generation.max_output_tokens > 4096
            {
                return Err(StorageError::Invalid("generation settings"));
            }
            for id in [&generation.job_id, &generation.retry_of_job_id]
                .into_iter()
                .flatten()
            {
                validate_id(id)?;
            }
            if generation.state.len() > 32
                || generation
                    .completion_reason
                    .as_ref()
                    .is_some_and(|value| value.len() > 64)
                || generation.context_warnings.len() > 64
                || generation
                    .context_warnings
                    .iter()
                    .any(|value| value.len() > 1024)
            {
                return Err(StorageError::Invalid("generation metadata"));
            }
            if generation
                .usage
                .as_ref()
                .is_some_and(contains_sensitive_fields)
            {
                return Err(StorageError::Invalid("credential-like generation field"));
            }
        }
    }
    for reference in &chat.job_references {
        validate_id(&reference.job_id)?;
        validate_id(&reference.server_profile_id)?;
        for id in [
            &reference.message_id,
            &reference.parent_job_id,
            &reference.retry_of_job_id,
        ]
        .into_iter()
        .flatten()
        {
            validate_id(id)?;
        }
        validate_timestamp(&reference.updated_at)?;
    }
    for id in &chat.attachment_ids {
        validate_id(id)?;
    }
    Ok(())
}

fn validate_settings(settings: &Settings) -> Result<()> {
    if settings.schema_version != SCHEMA_VERSION {
        return Err(StorageError::Unsupported {
            record: "settings",
            found: u64::from(settings.schema_version),
            supported: SCHEMA_VERSION,
        });
    }
    if !(0.0..=2.0).contains(&settings.default_temperature)
        || !settings.default_temperature.is_finite()
    {
        return Err(StorageError::Invalid("temperature"));
    }
    if !(250..=60_000).contains(&settings.connection.retry_delay_ms) {
        return Err(StorageError::Invalid("reconnect delay"));
    }
    if settings
        .extra
        .iter()
        .any(|(key, value)| sensitive_key(key) || contains_sensitive_fields(value))
    {
        return Err(StorageError::Invalid("credential-like settings field"));
    }
    if let Some(id) = &settings.default_profile_id {
        validate_id(id)?;
    }
    if let Some(id) = &settings.last_used_profile_id {
        validate_id(id)?;
    }
    for (id, model_id) in &settings.default_models {
        validate_id(id)?;
        if !model_id.is_empty() {
            validate_opaque_reference(model_id)?;
        }
    }
    Ok(())
}

fn validate_attachment_store(store: &AttachmentStore) -> Result<()> {
    if store.schema_version != SCHEMA_VERSION {
        return Err(StorageError::Unsupported {
            record: "attachments",
            found: u64::from(store.schema_version),
            supported: SCHEMA_VERSION,
        });
    }
    let mut ids = HashSet::new();
    for attachment in &store.attachments {
        validate_id(&attachment.id)?;
        if !ids.insert(&attachment.id) {
            return Err(StorageError::Invalid("duplicate attachment ID"));
        }
        if attachment.original_name.is_empty()
            || attachment.original_name.contains(['/', '\\', '\0'])
        {
            return Err(StorageError::Invalid("attachment name"));
        }
        if attachment.sha256.len() != 64
            || !attachment
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(StorageError::Invalid("attachment hash"));
        }
        for id in [&attachment.chat_id, &attachment.message_id]
            .into_iter()
            .flatten()
        {
            validate_id(id)?;
        }
        if let Some(relative) = &attachment.local_relative_path {
            validate_managed_relative(relative, "attachments/content")?;
        }
        if let Some(derivative) = &attachment.derivative {
            validate_managed_relative(&derivative.relative_path, "attachments/content")?;
        }
        validate_timestamp(&attachment.created_at)?;
        validate_timestamp(&attachment.updated_at)?;
    }
    Ok(())
}

fn validate_partial_store(store: &PartialResponseStore) -> Result<()> {
    if store.schema_version != SCHEMA_VERSION {
        return Err(StorageError::Unsupported {
            record: "partial responses",
            found: u64::from(store.schema_version),
            supported: SCHEMA_VERSION,
        });
    }
    let mut ids = HashSet::new();
    for response in &store.responses {
        for id in [&response.id, &response.chat_id, &response.message_id] {
            validate_id(id)?;
        }
        if !ids.insert(&response.id) {
            return Err(StorageError::Invalid("duplicate partial response ID"));
        }
        if let Some(id) = &response.job_id {
            validate_id(id)?;
        }
        validate_managed_relative(&response.relative_path, "partials/content")?;
        validate_timestamp(&response.updated_at)?;
    }
    Ok(())
}

fn validate_export_cache(store: &ExportCacheStore) -> Result<()> {
    if store.schema_version != SCHEMA_VERSION {
        return Err(StorageError::Unsupported {
            record: "export cache",
            found: u64::from(store.schema_version),
            supported: SCHEMA_VERSION,
        });
    }
    let mut ids = HashSet::new();
    for entry in &store.entries {
        validate_id(&entry.id)?;
        validate_id(&entry.chat_id)?;
        if !ids.insert(&entry.id) {
            return Err(StorageError::Invalid("duplicate export cache ID"));
        }
        validate_managed_relative(&entry.relative_path, "export-cache/content")?;
        validate_timestamp(&entry.created_at)?;
    }
    Ok(())
}

fn validate_managed_relative(relative: &str, required_prefix: &str) -> Result<()> {
    let path = Path::new(relative);
    if path.is_absolute()
        || !relative.starts_with(&format!("{required_prefix}/"))
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(StorageError::Invalid("managed relative path"));
    }
    Ok(())
}

fn validate_timestamp(timestamp: &str) -> Result<()> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|_| ())
        .map_err(|_| StorageError::Invalid("RFC 3339 timestamp"))
}

fn migrate_value(mut value: Value, record: &'static str) -> Result<Value> {
    let version = value
        .get("schemaVersion")
        .or_else(|| value.get("schema_version"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if version > u64::from(SCHEMA_VERSION) {
        return Err(StorageError::Unsupported {
            record,
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version == 0 {
        let object = value
            .as_object_mut()
            .ok_or(StorageError::Invalid("versioned JSON object"))?;
        object.insert("schemaVersion".into(), json!(SCHEMA_VERSION));
    }
    Ok(value)
}

fn recover_json<T: DeserializeOwned>(path: &Path, record: &'static str) -> Result<T> {
    let candidates = [path.to_path_buf(), temp_path(path), backup_path(path)];
    let mut last_error = None;
    for (position, candidate) in candidates.iter().enumerate() {
        if !candidate.exists() {
            continue;
        }
        match read_json::<T>(candidate, record) {
            Ok(value) => {
                if position != 0 {
                    let bytes = fs::read(candidate)?;
                    atomic_write_bytes(path, &bytes)?;
                } else if temp_path(path).exists() {
                    let _ = fs::remove_file(temp_path(path));
                }
                return Ok(value);
            }
            Err(error @ StorageError::Unsupported { .. }) if position == 0 => {
                return Err(error);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or(StorageError::NotFound))
}

fn recover_json_validated<T: DeserializeOwned>(
    path: &Path,
    record: &'static str,
    validator: fn(&T) -> Result<()>,
) -> Result<T> {
    let candidates = [path.to_path_buf(), temp_path(path), backup_path(path)];
    let mut last_error = None;
    for (position, candidate) in candidates.iter().enumerate() {
        if !candidate.exists() {
            continue;
        }
        match read_json::<T>(candidate, record).and_then(|value| {
            validator(&value)?;
            Ok(value)
        }) {
            Ok(value) => {
                if position != 0 {
                    let bytes = fs::read(candidate)?;
                    atomic_write_bytes(path, &bytes)?;
                } else if temp_path(path).exists() {
                    let _ = fs::remove_file(temp_path(path));
                }
                return Ok(value);
            }
            Err(error @ StorageError::Unsupported { .. }) if position == 0 => {
                return Err(error);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or(StorageError::NotFound))
}

fn read_json<T: DeserializeOwned>(path: &Path, record: &'static str) -> Result<T> {
    let file = File::open(path)?;
    let mut text = String::new();
    file.take((MAX_IMPORT_BYTES + 1) as u64)
        .read_to_string(&mut text)?;
    if text.len() > MAX_IMPORT_BYTES {
        return Err(StorageError::Invalid("local record size"));
    }
    let value: Value = serde_json::from_str(&text)?;
    let value = migrate_value(value, record)?;
    Ok(serde_json::from_value(value)?)
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic_write_bytes(path, &bytes)
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or(StorageError::Invalid("managed file parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = temp_path(path);
    let backup = backup_path(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() && !path.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(error.into());
    }
    sync_directory(parent);
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|v| v.to_str()).unwrap_or("data")
    ))
}
fn backup_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|v| v.to_str()).unwrap_or("data")
    ))
}

fn sync_directory(path: &Path) {
    #[cfg(unix)]
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

fn remove_managed_file(storage: &ClientStorage, relative: &str) -> Result<()> {
    let path = storage.safe_path(relative)?;
    if path.exists() {
        let canonical = fs::canonicalize(&path)?;
        if !canonical.starts_with(&storage.root) {
            return Err(StorageError::Invalid("attachment path escape"));
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

fn directory_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if path.is_file() {
        return Ok(fs::metadata(path)?.len());
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        total += if metadata.is_dir() {
            directory_size(&path)?
        } else {
            metadata.len()
        };
    }
    Ok(total)
}

fn path_size(path: &Path) -> Result<u64> {
    directory_size(path)
}
fn count_files(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if path.is_file() {
        return Ok(1);
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        total += if metadata.is_dir() {
            count_files(&path)?
        } else {
            1
        };
    }
    Ok(total)
}

fn redact_sensitive(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| !sensitive_key(key));
            for nested in object.values_mut() {
                redact_sensitive(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                redact_sensitive(nested);
            }
        }
        _ => {}
    }
}

fn sanitize_export_text(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.contains("authorization:") || lower.contains("bearer ") {
                "[redacted credential]".to_owned()
            } else {
                let mut sanitized = line.to_owned();
                for key in ["token=", "access_token=", "signature=", "x-amz-signature="] {
                    if let Some(start) = sanitized.to_ascii_lowercase().find(key) {
                        let value_start = start + key.len();
                        let value_end = sanitized[value_start..]
                            .find(['&', ' ', '\"'])
                            .map_or(sanitized.len(), |offset| value_start + offset);
                        sanitized.replace_range(value_start..value_end, "[redacted]");
                    }
                }
                sanitized
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn contains_sensitive_fields(value: &Value) -> bool {
    match value {
        Value::Object(object) => object
            .iter()
            .any(|(key, value)| sensitive_key(key) || contains_sensitive_fields(value)),
        Value::Array(values) => values.iter().any(contains_sensitive_fields),
        _ => false,
    }
}

fn sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "authorization"
            | "token"
            | "access_token"
            | "accesstoken"
            | "signed_url"
            | "signedurl"
            | "credential"
            | "credentials"
    )
}

fn detect_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if !bytes.is_empty()
        && bytes
            .iter()
            .all(|byte| matches!(byte, b'\t' | b'\n' | b'\r' | 0x20..=0x7e))
    {
        Some("text/plain")
    } else {
        None
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn storage() -> (TempDir, ClientStorage) {
        let temporary = tempfile::tempdir().unwrap();
        let storage = ClientStorage::open(temporary.path().join("Tome")).unwrap();
        (temporary, storage)
    }

    #[test]
    fn chat_crud_persists_and_sorts() {
        let (_temporary, storage) = storage();
        let chat = create_chat_inner(&storage, "Research".into(), None).unwrap();
        assert_eq!(storage.bootstrap().unwrap().chats[0].title, "Research");
        assert_eq!(
            recover_json::<Chat>(&storage.chat_path(&chat.id).unwrap(), "chat")
                .unwrap()
                .id,
            chat.id
        );
        delete_chat_inner(&storage, &chat.id).unwrap();
        assert!(storage.bootstrap().unwrap().chats.is_empty());
    }

    #[test]
    fn corrupt_chat_is_isolated_and_quarantined() {
        let (_temporary, storage) = storage();
        let valid = create_chat_inner(&storage, "Valid".into(), None).unwrap();
        let corrupt_id = Uuid::now_v7().to_string();
        fs::write(storage.chat_path(&corrupt_id).unwrap(), b"{truncated").unwrap();
        let bootstrap = storage.bootstrap().unwrap();
        assert_eq!(bootstrap.chats[0].id, valid.id);
        assert!(
            bootstrap
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.record_id.as_deref() == Some(&corrupt_id))
        );
    }

    #[test]
    fn interrupted_write_recovers_backup_and_removes_stale_temp() {
        let (_temporary, storage) = storage();
        let chat = create_chat_inner(&storage, "Backup".into(), None).unwrap();
        let path = storage.chat_path(&chat.id).unwrap();
        fs::copy(&path, backup_path(&path)).unwrap();
        fs::write(&path, b"{").unwrap();
        fs::write(temp_path(&path), b"also bad").unwrap();
        let recovered = recover_json::<Chat>(&path, "chat").unwrap();
        assert_eq!(recovered.title, "Backup");
    }

    #[test]
    fn restart_recovers_an_orphan_temporary_chat() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("Tome");
        let storage = ClientStorage::open(root.clone()).unwrap();
        let chat = create_chat_inner(&storage, "Interrupted".into(), None).unwrap();
        let path = storage.chat_path(&chat.id).unwrap();
        fs::rename(&path, temp_path(&path)).unwrap();
        drop(storage);

        let restarted = ClientStorage::open(root).unwrap();
        let bootstrap = restarted.bootstrap().unwrap();
        assert_eq!(bootstrap.chats[0].title, "Interrupted");
        assert!(path.exists());
    }

    #[test]
    fn partial_response_checkpoint_is_atomic_and_reusable() {
        let (_temporary, storage) = storage();
        let chat = create_chat_inner(&storage, "Streaming".into(), None).unwrap();
        let message_id = Uuid::now_v7().to_string();
        let job_id = Uuid::now_v7().to_string();
        let first =
            save_partial_response_inner(&storage, &chat.id, &message_id, Some(&job_id), "partial")
                .unwrap();
        let second = save_partial_response_inner(
            &storage,
            &chat.id,
            &message_id,
            Some(&job_id),
            "partial response",
        )
        .unwrap();
        assert_eq!(first.id, second.id);
        let index: PartialResponseStore = recover_json_validated(
            &storage.safe_path("partials/index.json").unwrap(),
            "partial responses",
            validate_partial_store,
        )
        .unwrap();
        assert_eq!(index.responses.len(), 1);
        assert_eq!(
            fs::read_to_string(
                storage
                    .safe_path(&index.responses[0].relative_path)
                    .unwrap()
            )
            .unwrap(),
            "partial response"
        );
        assert_eq!(
            fs::read_dir(storage.safe_path("partials/content").unwrap())
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn interaction_export_text_redacts_credentials_and_signed_queries() {
        let sanitized = sanitize_export_text(
            "safe\nAuthorization: Bearer secret\nhttps://example.invalid/?token=secret&ok=1",
        );
        assert!(sanitized.contains("safe"));
        assert!(sanitized.contains("token=[redacted]&ok=1"));
        assert!(!sanitized.contains("Bearer secret"));
    }

    #[test]
    fn duplicate_message_ids_are_rejected() {
        let id = Uuid::now_v7().to_string();
        let timestamp = now();
        let message = Message {
            id: Uuid::now_v7().to_string(),
            role: MessageRole::User,
            content: "hello".into(),
            created_at: timestamp.clone(),
            parent_message_id: None,
            attachment_ids: Vec::new(),
            generation: None,
        };
        let chat = Chat {
            schema_version: SCHEMA_VERSION,
            id,
            title: "Duplicate".into(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
            server_profile_id: None,
            model_id: None,
            messages: vec![message.clone(), message],
            job_references: Vec::new(),
            attachment_ids: Vec::new(),
            extra: serde_json::Map::new(),
        };
        assert!(matches!(
            validate_chat(&chat),
            Err(StorageError::Invalid("duplicate message ID"))
        ));
    }

    #[test]
    fn corrupt_settings_do_not_block_the_shell() {
        let (_temporary, storage) = storage();
        fs::write(storage.settings_path().unwrap(), b"{broken").unwrap();
        let bootstrap = storage.bootstrap().unwrap();
        assert!((bootstrap.settings.default_temperature - 0.7).abs() < f64::EPSILON);
        assert!(
            bootstrap
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.record_type == "settings")
        );
    }

    #[test]
    fn duplicate_profile_ids_are_isolated() {
        let (_temporary, storage) = storage();
        let timestamp = now();
        let profile = ServerProfile {
            id: Uuid::now_v7().to_string(),
            name: "Private".into(),
            mode: NetworkMode::Lan,
            host: "192.168.1.20".into(),
            port: 7331,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            last_connected_at: None,
            last_event_id: 0,
            compatibility: None,
        };
        atomic_write_json(
            &storage.profiles_path().unwrap(),
            &ProfileStore {
                schema_version: SCHEMA_VERSION,
                profiles: vec![profile.clone(), profile],
            },
        )
        .unwrap();
        let bootstrap = storage.bootstrap().unwrap();
        assert_eq!(bootstrap.profiles.len(), 1);
        assert!(
            bootstrap
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "invalid_or_duplicate_id")
        );
    }

    #[test]
    fn failed_migration_preserves_the_original() {
        let (_temporary, storage) = storage();
        let path = storage.settings_path().unwrap();
        fs::write(&path, br#"{"legacy":"missing required settings"}"#).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(read_json::<Settings>(&path, "settings").is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn settings_preserve_unknown_forward_compatible_fields() {
        let (_temporary, storage) = storage();
        let mut settings = Settings::default();
        settings
            .extra
            .insert("futurePreference".into(), json!({ "enabled": true }));
        let path = storage.settings_path().unwrap();
        atomic_write_json(&path, &settings).unwrap();
        let loaded =
            recover_json_validated::<Settings>(&path, "settings", validate_settings).unwrap();
        assert_eq!(
            loaded.extra.get("futurePreference"),
            Some(&json!({ "enabled": true }))
        );
    }

    #[test]
    fn unsupported_versions_fail_without_replacing_original() {
        let (_temporary, storage) = storage();
        let id = Uuid::now_v7().to_string();
        let path = storage.chat_path(&id).unwrap();
        fs::write(&path, format!(r#"{{"schemaVersion":99,"id":"{id}"}}"#)).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(matches!(
            recover_json::<Chat>(&path, "chat"),
            Err(StorageError::Unsupported { .. })
        ));
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn import_collision_gets_a_new_local_id() {
        let (_temporary, storage) = storage();
        let chat = create_chat_inner(&storage, "Imported".into(), None).unwrap();
        let json = serde_json::to_string(&chat).unwrap();
        let imported = import_chat_inner(&storage, &json).unwrap();
        assert_ne!(imported.id, chat.id);
        assert_eq!(storage.bootstrap().unwrap().chats.len(), 2);
    }

    #[test]
    fn imports_reject_credential_fields() {
        let (_temporary, storage) = storage();
        let chat = create_chat_inner(&storage, "Safe".into(), None).unwrap();
        let mut value = serde_json::to_value(chat).unwrap();
        value["authorization"] = Value::String("secret".into());
        assert!(matches!(
            import_chat_inner(&storage, &value.to_string()),
            Err(StorageError::Invalid("credential-like chat field"))
        ));
    }

    #[test]
    fn native_export_writes_only_to_a_user_selected_external_directory() {
        let (temporary, storage) = storage();
        let chat = create_chat_inner(&storage, "Export me".into(), None).unwrap();
        let export_directory = temporary.path().join("exports");
        fs::create_dir(&export_directory).unwrap();
        let destination = export_directory.join("chat.tome.json");
        let written = export_chat_file_inner(&storage, &chat.id, &destination).unwrap();
        assert_eq!(written, fs::canonicalize(&destination).unwrap());
        assert!(
            fs::read_to_string(destination)
                .unwrap()
                .contains("Export me")
        );
        assert!(
            export_chat_file_inner(
                &storage,
                &chat.id,
                &storage.safe_path("export-cache/chat.json").unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn attachment_modes_are_bounded_and_explicit() {
        let (_temporary, storage) = storage();
        let metadata = store_attachment_inner(
            &storage,
            "notes.txt".into(),
            Some("text/plain".into()),
            b"hello hello hello".to_vec(),
            AttachmentStorageMode::Optimized,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            Path::new(metadata.local_relative_path.as_deref().unwrap())
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("gz")
        );
        let only = store_attachment_inner(
            &storage,
            "external.pdf".into(),
            None,
            vec![],
            AttachmentStorageMode::MetadataOnly,
            None,
            None,
        )
        .unwrap();
        assert!(only.local_relative_path.is_none());
    }

    #[test]
    fn delete_all_needs_exact_confirmation_and_never_touches_external_exports() {
        let (temporary, storage) = storage();
        create_chat_inner(&storage, "Delete me".into(), None).unwrap();
        let external = temporary.path().join("exported-chat.json");
        fs::write(&external, b"keep").unwrap();
        assert!(delete_all_inner(&storage, "delete").is_err());
        let summary = delete_all_inner(&storage, DELETE_CONFIRMATION).unwrap();
        assert!(summary.reset_to_first_run);
        assert_eq!(fs::read(external).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn delete_all_unlinks_a_managed_symlink_without_following_it() {
        let (temporary, storage) = storage();
        let external = temporary.path().join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("keep.txt"), b"keep").unwrap();
        let attachments = storage.safe_path("attachments").unwrap();
        fs::remove_dir_all(&attachments).unwrap();
        std::os::unix::fs::symlink(&external, &attachments).unwrap();

        let summary = delete_all_inner(&storage, DELETE_CONFIRMATION).unwrap();
        assert!(summary.reset_to_first_run);
        assert_eq!(fs::read(external.join("keep.txt")).unwrap(), b"keep");
        assert_eq!(count_files(&external).unwrap(), 1);
    }

    #[test]
    fn rejects_path_ids_and_symlinked_delete_roots() {
        assert!(validate_id("../../escape").is_err());
        let timestamp = now();
        let public_profile = ServerProfile {
            id: Uuid::now_v7().to_string(),
            name: "Public".into(),
            mode: NetworkMode::Lan,
            host: "8.8.8.8".into(),
            port: 7331,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            last_connected_at: None,
            last_event_id: 0,
            compatibility: None,
        };
        assert!(validate_profile(&public_profile).is_err());
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            let target = temporary.path().join("target");
            fs::create_dir(&target).unwrap();
            let link = temporary.path().join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(validate_delete_root(&link).is_err());
            assert!(ClientStorage::open(link).is_err());
        }
    }
}
