use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{
    Client, StatusCode,
    header::{CONTENT_LENGTH, ETAG, RANGE},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex, RwLock},
};

use crate::{
    catalog::{CatalogEntry, ModelCatalog, RuntimeStatus},
    hardware::{HardwareCapabilities, detect},
    model::{Job, JobState, JobType},
    store::{CreateJob, JobStore, StoreError},
};

#[derive(Debug, Error)]
pub enum ModelError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("model catalog is invalid: {0}")]
    Catalog(#[from] serde_json::Error),
    #[error("model I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("model network request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("model catalog entry was not found")]
    CatalogEntryNotFound,
    #[error("installed model was not found")]
    ModelNotFound,
    #[error("model operation is not allowed: {0}")]
    Conflict(String),
    #[error("model request is invalid: {0}")]
    Invalid(String),
    #[error("model artifact verification failed: {0}")]
    Verification(String),
    #[error("model runtime is unavailable: {0}")]
    RuntimeUnavailable(String),
    #[error("model runtime failed: {0}")]
    Runtime(String),
    #[error("time formatting failed: {0}")]
    Time(#[from] time::error::Format),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModel {
    pub id: String,
    pub catalog_id: String,
    pub logical_model_id: String,
    pub display_name: String,
    pub catalog_version: String,
    pub provider: String,
    pub repository: String,
    pub revision: String,
    pub artifact_name: String,
    pub format: String,
    pub quantization: String,
    pub byte_size: u64,
    pub sha256: String,
    pub local_path: String,
    pub verification_state: String,
    pub capabilities: Vec<String>,
    pub compatibility_state: String,
    pub compatibility_reason: String,
    pub context_limit: u32,
    pub tokenizer: String,
    pub chat_template: Option<String>,
    pub estimated_ram_bytes: u64,
    pub estimated_vram_bytes: Option<u64>,
    pub loaded: bool,
    pub in_use_count: u32,
    pub installed_at: String,
    pub verified_at: String,
    pub last_loaded_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inventory {
    pub schema_version: u32,
    pub catalog_version: String,
    pub ready: bool,
    pub default_model_id: Option<String>,
    pub models: Vec<InstalledModel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupPlan {
    pub schema_version: u32,
    pub ready: bool,
    pub profiles: Vec<ProfileAssessment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileAssessment {
    pub id: String,
    pub name: String,
    pub entry_ids: Vec<String>,
    pub compatible: bool,
    pub exclusion_reasons: Vec<String>,
    pub download_bytes: u64,
    pub required_disk_bytes: u64,
    pub estimated_ram_bytes: u64,
    pub estimated_vram_bytes: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadSubmission {
    pub catalog_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub license_accepted: bool,
}

#[derive(Clone)]
pub struct ModelManager {
    store: JobStore,
    catalog: Arc<ModelCatalog>,
    model_root: Arc<PathBuf>,
    temp_root: Arc<PathBuf>,
    trash_root: Arc<PathBuf>,
    hardware: Arc<RwLock<HardwareCapabilities>>,
    client: Client,
    processes: Arc<Mutex<HashMap<String, Child>>>,
}

#[allow(clippy::missing_errors_doc)]
impl ModelManager {
    pub async fn open(store: JobStore, data_root: &Path) -> Result<Self, ModelError> {
        let model_root = data_root.join("models");
        let temp_root = data_root.join("model-downloads");
        let trash_root = data_root.join("model-trash");
        for root in [&model_root, &temp_root, &trash_root] {
            fs::create_dir_all(root).await?;
        }
        let model_root = fs::canonicalize(model_root).await?;
        let temp_root = fs::canonicalize(temp_root).await?;
        let trash_root = fs::canonicalize(trash_root).await?;
        let hardware = detect(&model_root).await;
        let catalog = ModelCatalog::embedded()?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(30))
            .timeout(Duration::from_hours(6))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent(concat!("Tome/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let manager = Self {
            store,
            catalog: Arc::new(catalog),
            model_root: Arc::new(model_root),
            temp_root: Arc::new(temp_root),
            trash_root: Arc::new(trash_root),
            hardware: Arc::new(RwLock::new(hardware)),
            client,
            processes: Arc::new(Mutex::new(HashMap::new())),
        };
        manager.reconcile().await?;
        Ok(manager)
    }

    #[must_use]
    pub fn catalog(&self) -> &ModelCatalog {
        &self.catalog
    }

    pub async fn hardware(&self) -> HardwareCapabilities {
        self.hardware.read().await.clone()
    }

    pub async fn refresh_hardware(&self) -> Result<HardwareCapabilities, ModelError> {
        let detected = detect(&self.model_root).await;
        *self.hardware.write().await = detected.clone();
        self.reassess_compatibility(&detected).await?;
        Ok(detected)
    }

    pub async fn setup_plan(&self) -> Result<SetupPlan, ModelError> {
        let hardware = self.hardware().await;
        let ready = self.inventory().await?.ready;
        let profiles = self.catalog.profiles.iter().map(|profile| {
            let entries = profile.entry_ids.iter().filter_map(|id| self.catalog.entry(id)).collect::<Vec<_>>();
            let download_bytes = entries.iter().map(|entry| entry.bytes).sum();
            let overhead: u64 = entries.iter().map(|entry| entry.disk_overhead_bytes).sum();
            let peak_system_memory = entries.iter().map(|entry| entry.estimated_ram_bytes).max().unwrap_or(0);
            let peak_gpu_memory = entries.iter().filter_map(|entry| entry.estimated_vram_bytes).max();
            let mut exclusion_reasons = Vec::new();
            if entries.iter().any(|entry| entry.estimated_ram_bytes > hardware.total_memory_bytes) {
                exclusion_reasons.push("Estimated model RAM exceeds detected system RAM.".to_owned());
            }
            if let Some(free) = hardware.model_storage.free_bytes
                && download_bytes + overhead > free
            {
                exclusion_reasons.push("Required disk space exceeds currently reported free space.".to_owned());
            }
            if profile.id != "manual" && entries.iter().any(|entry| entry.runtime_status == RuntimeStatus::Executable)
                && !hardware.runtime.llama_cpp.available
            {
                exclusion_reasons.push("llama-server is unavailable; install the reviewed runtime before loading text models.".to_owned());
            }
            ProfileAssessment {
                id: profile.id.clone(),
                name: profile.name.clone(),
                entry_ids: profile.entry_ids.clone(),
                compatible: exclusion_reasons.is_empty(),
                exclusion_reasons,
                download_bytes,
                required_disk_bytes: download_bytes + overhead,
                estimated_ram_bytes: peak_system_memory,
                estimated_vram_bytes: peak_gpu_memory,
            }
        }).collect();
        Ok(SetupPlan {
            schema_version: 1,
            ready,
            profiles,
        })
    }

    pub async fn submit_download(
        &self,
        request: DownloadSubmission,
    ) -> Result<(Job, bool), ModelError> {
        if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 128 {
            return Err(ModelError::Invalid(
                "idempotency_key must contain 1 to 128 characters".to_owned(),
            ));
        }
        let entry = self
            .catalog
            .entry(&request.catalog_id)
            .ok_or(ModelError::CatalogEntryNotFound)?;
        if entry.access != "public" {
            return Err(ModelError::Invalid(
                "restricted artifacts require OS-backed credential storage, which is not enabled"
                    .to_owned(),
            ));
        }
        if !request.license_accepted {
            return Err(ModelError::Invalid(format!(
                "accept the {} license before downloading",
                entry.license
            )));
        }
        sqlx::query(
            "INSERT OR IGNORE INTO model_license_acceptances (catalog_id, revision, license, accepted_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(&entry.id)
        .bind(&entry.revision)
        .bind(&entry.license)
        .bind(now()?)
        .execute(&self.store.pool)
        .await?;
        Ok(self
            .store
            .create_job(CreateJob {
                idempotency_key: request.idempotency_key,
                job_type: JobType::ModelDownload,
                input: json!({ "catalog_id": request.catalog_id }),
                parent_job_id: None,
                retry_of_job_id: None,
            })
            .await?)
    }

    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    pub async fn execute_download(&self, job: &Job) -> Result<Value, ModelError> {
        let catalog_id = job
            .input
            .get("catalog_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ModelError::Invalid("model_download input requires catalog_id".to_owned())
            })?;
        let entry = self
            .catalog
            .entry(catalog_id)
            .cloned()
            .ok_or(ModelError::CatalogEntryNotFound)?;
        let safe_name = safe_component(&entry.id)?;
        let part_path = self.temp_root.join(format!("{safe_name}.part"));
        let final_directory = self.model_root.join(safe_name);
        fs::create_dir_all(&final_directory).await?;
        let final_path = final_directory.join(safe_component(&entry.artifact)?);
        self.assert_contained(&part_path, &self.temp_root, false)
            .await?;
        self.assert_contained(&final_path, &self.model_root, false)
            .await?;

        if final_path.is_file() && verify_file(&final_path, &entry).await.is_ok() {
            let model = self.register(&entry, &final_path).await?;
            return Ok(json!({ "model_id": model.id, "already_present": true }));
        }
        let existing = fs::metadata(&part_path)
            .await
            .map_or(0, |metadata| metadata.len());
        if existing > entry.bytes {
            fs::remove_file(&part_path).await?;
        }
        let mut offset = fs::metadata(&part_path)
            .await
            .map_or(0, |metadata| metadata.len());
        let required = entry.bytes.saturating_sub(offset) + entry.disk_overhead_bytes;
        if let Ok(free) = fs2::available_space(&*self.model_root)
            && free < required
        {
            return Err(ModelError::Conflict(format!(
                "low disk: {required} bytes required but {free} bytes are free"
            )));
        }
        self.record_download(job, &entry, &part_path, &final_path, offset, None, None)
            .await?;

        let mut request = self.client.get(&entry.download_url);
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let response = request.send().await?;
        let range_supported = response.status() == StatusCode::PARTIAL_CONTENT;
        if !response.status().is_success() {
            return Err(ModelError::Http(response.error_for_status().unwrap_err()));
        }
        if offset > 0 && !range_supported {
            offset = 0;
            let _ = fs::remove_file(&part_path).await;
        }
        let response_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let expected_response = entry.bytes.saturating_sub(offset);
        if let Some(length) = response_length
            && length != expected_response
        {
            return Err(ModelError::Verification(format!(
                "content length {length} did not match expected {expected_response}"
            )));
        }
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(offset > 0)
            .truncate(offset == 0)
            .open(&part_path)
            .await?;
        let mut stream = response.bytes_stream();
        let mut downloaded = offset;
        let mut last_checked = offset;
        let mut last_reported = offset;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            if downloaded > entry.bytes {
                return Err(ModelError::Verification(
                    "download exceeded expected artifact size".to_owned(),
                ));
            }
            file.write_all(&chunk).await?;
            if downloaded.saturating_sub(last_checked) >= 256 * 1024 {
                last_checked = downloaded;
                let state = self.store.get_job(&job.id).await?.state;
                if state != JobState::Running {
                    file.flush().await?;
                    return Ok(json!({ "partial": true, "bytes_downloaded": downloaded }));
                }
            }
            if downloaded.saturating_sub(last_reported) >= 4 * 1024 * 1024
                || downloaded == entry.bytes
            {
                last_reported = downloaded;
                self.record_download(
                    job,
                    &entry,
                    &part_path,
                    &final_path,
                    downloaded,
                    etag.as_deref(),
                    Some(range_supported),
                )
                .await?;
                self.store
                    .update_progress(
                        &job.id,
                        (downloaded as f64 / entry.bytes as f64).clamp(0.0, 0.99),
                    )
                    .await?;
            }
        }
        file.flush().await?;
        drop(file);
        if self.store.get_job(&job.id).await?.state != JobState::Running {
            return Ok(json!({ "partial": true, "bytes_downloaded": downloaded }));
        }
        if downloaded != entry.bytes {
            return Err(ModelError::Verification(format!(
                "download ended at {downloaded} bytes; expected {}",
                entry.bytes
            )));
        }
        verify_file(&part_path, &entry).await?;
        if final_path.exists() {
            fs::remove_file(&final_path).await?;
        }
        fs::rename(&part_path, &final_path).await?;
        let model = self.register(&entry, &final_path).await?;
        Ok(json!({ "model_id": model.id, "bytes": entry.bytes, "verified": true }))
    }

    pub async fn inventory(&self) -> Result<Inventory, ModelError> {
        let rows = sqlx::query("SELECT * FROM model_artifacts ORDER BY id ASC")
            .fetch_all(&self.store.pool)
            .await?;
        let models = rows
            .iter()
            .map(model_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let default_model_id =
            sqlx::query_scalar("SELECT default_model_id FROM model_settings WHERE singleton = 1")
                .fetch_optional(&self.store.pool)
                .await?
                .flatten();
        let ready = models.iter().any(|model| {
            model.verification_state == "verified"
                && model.compatibility_state == "compatible"
                && model
                    .capabilities
                    .iter()
                    .any(|capability| capability == "text_output")
        });
        Ok(Inventory {
            schema_version: 1,
            catalog_version: self.catalog.catalog_version.clone(),
            ready,
            default_model_id,
            models,
        })
    }

    pub async fn set_default(&self, model_id: &str) -> Result<Inventory, ModelError> {
        let model = self.model(model_id).await?;
        if model.verification_state != "verified"
            || model.compatibility_state != "compatible"
            || !model
                .capabilities
                .iter()
                .any(|capability| capability == "text_output")
        {
            return Err(ModelError::Conflict(
                "default model must be a verified, compatible text-output model".to_owned(),
            ));
        }
        sqlx::query(
            "UPDATE model_settings SET default_model_id = ?, updated_at = ? WHERE singleton = 1",
        )
        .bind(model_id)
        .bind(now()?)
        .execute(&self.store.pool)
        .await?;
        self.inventory().await
    }

    pub async fn load(&self, model_id: &str) -> Result<InstalledModel, ModelError> {
        let model = self.model(model_id).await?;
        if model.loaded {
            return Ok(model);
        }
        if model.compatibility_state != "compatible" {
            return Err(ModelError::Conflict(model.compatibility_reason));
        }
        let hardware = self.hardware().await;
        let executable = hardware
            .runtime
            .llama_cpp
            .executable
            .ok_or_else(|| ModelError::RuntimeUnavailable(hardware.runtime.llama_cpp.reason))?;
        let adapter = LlamaCppAdapter { executable };
        let child = adapter.start(&model, &self.client).await?;
        self.processes
            .lock()
            .await
            .insert(model_id.to_owned(), child);
        sqlx::query("UPDATE model_artifacts SET loaded = 1, last_loaded_at = ?, updated_at = ? WHERE id = ?")
            .bind(now()?).bind(now()?).bind(model_id).execute(&self.store.pool).await?;
        self.model(model_id).await
    }

    pub async fn unload(&self, model_id: &str) -> Result<InstalledModel, ModelError> {
        let model = self.model(model_id).await?;
        if model.in_use_count > 0 {
            return Err(ModelError::Conflict(
                "model is actively referenced and cannot be unloaded".to_owned(),
            ));
        }
        if let Some(mut child) = self.processes.lock().await.remove(model_id) {
            child.kill().await?;
            let _ = child.wait().await;
        }
        sqlx::query("UPDATE model_artifacts SET loaded = 0, updated_at = ? WHERE id = ?")
            .bind(now()?)
            .bind(model_id)
            .execute(&self.store.pool)
            .await?;
        self.model(model_id).await
    }

    pub async fn delete(&self, model_id: &str) -> Result<Inventory, ModelError> {
        let model = self.model(model_id).await?;
        if model.loaded || model.in_use_count > 0 {
            return Err(ModelError::Conflict(
                "loaded or in-use models cannot be deleted".to_owned(),
            ));
        }
        if model.verification_state != "verified" {
            return Err(ModelError::Conflict(
                "only known verified installations can be deleted".to_owned(),
            ));
        }
        let path = fs::canonicalize(&model.local_path).await?;
        self.assert_contained(&path, &self.model_root, true).await?;
        let trash_name = format!(
            "{}-{}-{}",
            safe_component(&model.catalog_id)?,
            OffsetDateTime::now_utc().unix_timestamp(),
            safe_component(&model.artifact_name)?
        );
        let trash_path = self.trash_root.join(trash_name);
        fs::rename(&path, &trash_path).await?;
        let mut transaction = self.store.pool.begin().await?;
        let current_default: Option<String> =
            sqlx::query_scalar("SELECT default_model_id FROM model_settings WHERE singleton = 1")
                .fetch_one(&mut *transaction)
                .await?;
        sqlx::query("DELETE FROM model_artifacts WHERE id = ?")
            .bind(model_id)
            .execute(&mut *transaction)
            .await?;
        if current_default.as_deref() == Some(model_id) {
            let fallback: Option<String> = sqlx::query_scalar(
                "SELECT id FROM model_artifacts WHERE verification_state = 'verified'
                 AND compatibility_state = 'compatible' AND capabilities_json LIKE '%text_output%'
                 ORDER BY installed_at ASC LIMIT 1",
            )
            .fetch_optional(&mut *transaction)
            .await?
            .flatten();
            sqlx::query("UPDATE model_settings SET default_model_id = ?, updated_at = ? WHERE singleton = 1")
                .bind(fallback).bind(now()?).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        self.inventory().await
    }

    pub async fn shutdown(&self) -> Result<(), ModelError> {
        let mut processes = self.processes.lock().await;
        for child in processes.values_mut() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        processes.clear();
        sqlx::query("UPDATE model_artifacts SET loaded = 0, updated_at = ? WHERE loaded != 0")
            .bind(now()?)
            .execute(&self.store.pool)
            .await?;
        Ok(())
    }

    async fn reconcile(&self) -> Result<(), ModelError> {
        sqlx::query("UPDATE model_artifacts SET loaded = 0, in_use_count = 0, updated_at = ? WHERE loaded != 0 OR in_use_count != 0")
            .bind(now()?).execute(&self.store.pool).await?;
        self.store.resume_interrupted_downloads().await?;
        let models = self.inventory().await?.models;
        for model in models {
            let path = PathBuf::from(&model.local_path);
            if !path.is_file() {
                sqlx::query("UPDATE model_artifacts SET verification_state = 'failed', compatibility_state = 'incompatible', compatibility_reason = 'registered artifact is missing', updated_at = ? WHERE id = ?")
                    .bind(now()?).bind(&model.id).execute(&self.store.pool).await?;
            }
        }
        for entry in &self.catalog.entries {
            let final_path = self
                .model_root
                .join(safe_component(&entry.id)?)
                .join(safe_component(&entry.artifact)?);
            let registered: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM model_artifacts WHERE catalog_id = ?")
                    .bind(&entry.id)
                    .fetch_one(&self.store.pool)
                    .await?;
            if registered == 0
                && final_path.is_file()
                && verify_file(&final_path, entry).await.is_ok()
            {
                self.register(entry, &final_path).await?;
            }
        }
        let mut partials = fs::read_dir(&*self.temp_root).await?;
        while let Some(partial) = partials.next_entry().await? {
            let name = partial.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".part")
                && self.catalog.entry(stem).is_none()
            {
                self.store
                    .warning(
                        "unknown_model_partial",
                        "an unrecognized model partial was preserved for operator review",
                    )
                    .await?;
            }
        }
        self.reassess_compatibility(&self.hardware().await).await?;
        Ok(())
    }

    async fn reassess_compatibility(
        &self,
        hardware: &HardwareCapabilities,
    ) -> Result<(), ModelError> {
        for entry in &self.catalog.entries {
            let (state, reason) = compatibility(entry, hardware);
            sqlx::query(
                "UPDATE model_artifacts SET compatibility_state = ?, compatibility_reason = ?,
                 updated_at = ? WHERE catalog_id = ? AND verification_state = 'verified'",
            )
            .bind(state)
            .bind(reason)
            .bind(now()?)
            .bind(&entry.id)
            .execute(&self.store.pool)
            .await?;
        }
        sqlx::query(
            "UPDATE model_settings SET default_model_id = (
                 SELECT id FROM model_artifacts WHERE verification_state = 'verified'
                 AND compatibility_state = 'compatible' AND capabilities_json LIKE '%text_output%'
                 ORDER BY installed_at ASC LIMIT 1
             ), updated_at = ? WHERE singleton = 1 AND (
                 default_model_id IS NULL OR NOT EXISTS (
                     SELECT 1 FROM model_artifacts WHERE id = model_settings.default_model_id
                     AND verification_state = 'verified' AND compatibility_state = 'compatible'
                     AND capabilities_json LIKE '%text_output%'
                 )
             )",
        )
        .bind(now()?)
        .execute(&self.store.pool)
        .await?;
        Ok(())
    }

    async fn register(
        &self,
        entry: &CatalogEntry,
        path: &Path,
    ) -> Result<InstalledModel, ModelError> {
        let hardware = self.hardware().await;
        let (state, reason) = compatibility(entry, &hardware);
        let timestamp = now()?;
        let id = format!("catalog:{}", entry.id);
        sqlx::query(
            "INSERT INTO model_artifacts
             (id, catalog_id, logical_model_id, display_name, catalog_version, provider, repository,
              revision, artifact_name, format, quantization, byte_size, sha256, local_path,
              verification_state, capabilities_json, compatibility_state, compatibility_reason,
              context_limit, tokenizer, chat_template, estimated_ram_bytes, estimated_vram_bytes,
              loaded, in_use_count, installed_at, verified_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'verified', ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, ?, ?, ?)
             ON CONFLICT(catalog_id) DO UPDATE SET local_path = excluded.local_path,
              verification_state = 'verified', compatibility_state = excluded.compatibility_state,
              compatibility_reason = excluded.compatibility_reason, verified_at = excluded.verified_at,
              updated_at = excluded.updated_at",
        )
        .bind(&id).bind(&entry.id).bind(format!("{}/{}", entry.repository, entry.revision))
        .bind(&entry.name).bind(&self.catalog.catalog_version).bind(&entry.provider).bind(&entry.repository)
        .bind(&entry.revision).bind(&entry.artifact).bind(&entry.format).bind(&entry.quantization)
        .bind(to_i64(entry.bytes, "artifact bytes")?).bind(&entry.sha256).bind(path.display().to_string())
        .bind(serde_json::to_string(&entry.capabilities)?).bind(state).bind(reason)
        .bind(i64::from(entry.context_limit)).bind(&entry.tokenizer).bind(&entry.chat_template)
        .bind(to_i64(entry.estimated_ram_bytes, "estimated RAM")?).bind(entry.estimated_vram_bytes.map(|value| to_i64(value, "estimated VRAM")).transpose()?)
        .bind(&timestamp).bind(&timestamp).bind(&timestamp).execute(&self.store.pool).await?;
        let default: Option<String> =
            sqlx::query_scalar("SELECT default_model_id FROM model_settings WHERE singleton = 1")
                .fetch_one(&self.store.pool)
                .await?;
        if default.is_none()
            && entry
                .capabilities
                .iter()
                .any(|capability| capability == "text_output")
            && state == "compatible"
        {
            sqlx::query("UPDATE model_settings SET default_model_id = ?, updated_at = ? WHERE singleton = 1")
                .bind(&id).bind(now()?).execute(&self.store.pool).await?;
        }
        self.model(&id).await
    }

    async fn model(&self, id: &str) -> Result<InstalledModel, ModelError> {
        let row = sqlx::query("SELECT * FROM model_artifacts WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.store.pool)
            .await?
            .ok_or(ModelError::ModelNotFound)?;
        model_from_row(&row)
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_download(
        &self,
        job: &Job,
        entry: &CatalogEntry,
        part: &Path,
        final_path: &Path,
        downloaded: u64,
        etag: Option<&str>,
        range: Option<bool>,
    ) -> Result<(), ModelError> {
        sqlx::query(
            "INSERT INTO model_downloads (job_id, catalog_id, part_path, final_path, bytes_downloaded, expected_bytes, etag, range_supported, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(job_id) DO UPDATE SET bytes_downloaded = excluded.bytes_downloaded,
              etag = COALESCE(excluded.etag, model_downloads.etag), range_supported = COALESCE(excluded.range_supported, model_downloads.range_supported), updated_at = excluded.updated_at",
        ).bind(&job.id).bind(&entry.id).bind(part.display().to_string()).bind(final_path.display().to_string())
            .bind(to_i64(downloaded, "downloaded bytes")?).bind(to_i64(entry.bytes, "expected bytes")?).bind(etag).bind(range.map(i32::from))
            .bind(now()?).bind(now()?).execute(&self.store.pool).await?;
        Ok(())
    }

    async fn assert_contained(
        &self,
        path: &Path,
        root: &Path,
        must_exist: bool,
    ) -> Result<(), ModelError> {
        let checked = if must_exist {
            fs::canonicalize(path).await?
        } else {
            let parent = path
                .parent()
                .ok_or_else(|| ModelError::Invalid("path has no parent".to_owned()))?;
            fs::canonicalize(parent).await?.join(
                path.file_name()
                    .ok_or_else(|| ModelError::Invalid("path has no filename".to_owned()))?,
            )
        };
        if !checked.starts_with(root) {
            return Err(ModelError::Invalid(
                "model path escapes its configured storage root".to_owned(),
            ));
        }
        Ok(())
    }
}

fn compatibility(entry: &CatalogEntry, hardware: &HardwareCapabilities) -> (&'static str, String) {
    if entry.runtime_status == RuntimeStatus::CatalogOnly {
        return (
            "catalog_only",
            "Cataloged for a future runtime adapter; Tome cannot execute this artifact in Phase 2."
                .to_owned(),
        );
    }
    if entry.estimated_ram_bytes > hardware.total_memory_bytes {
        return (
            "incompatible",
            "Estimated RAM exceeds detected system RAM.".to_owned(),
        );
    }
    if !hardware.runtime.llama_cpp.available {
        return (
            "runtime_unavailable",
            hardware.runtime.llama_cpp.reason.clone(),
        );
    }
    (
        "compatible",
        "Verified GGUF supported by the detected llama.cpp runtime.".to_owned(),
    )
}

trait RuntimeAdapter {
    async fn start(&self, model: &InstalledModel, client: &Client) -> Result<Child, ModelError>;
}

struct LlamaCppAdapter {
    executable: String,
}

impl RuntimeAdapter for LlamaCppAdapter {
    async fn start(&self, model: &InstalledModel, client: &Client) -> Result<Child, ModelError> {
        let port = reserve_loopback_port()?;
        let mut child = Command::new(&self.executable)
            .args([
                "-m",
                &model.local_path,
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--no-webui",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let health_url = format!("http://127.0.0.1:{port}/health");
        for _ in 0..40 {
            if child.try_wait()?.is_some() {
                return Err(ModelError::Runtime(
                    "llama-server exited before the model became ready".to_owned(),
                ));
            }
            if client
                .get(&health_url)
                .timeout(Duration::from_millis(500))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(child);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let _ = child.kill().await;
        Err(ModelError::Runtime(
            "llama-server did not become healthy within ten seconds".to_owned(),
        ))
    }
}

async fn verify_file(path: &Path, entry: &CatalogEntry) -> Result<(), ModelError> {
    let metadata = fs::metadata(path).await?;
    if !metadata.is_file() || metadata.len() != entry.bytes {
        return Err(ModelError::Verification(format!(
            "artifact size {} did not match expected {}",
            metadata.len(),
            entry.bytes
        )));
    }
    let mut file = fs::File::open(path).await?;
    let mut header = [0_u8; 8];
    let read = file.read(&mut header).await?;
    if entry.format == "gguf" && (read < 4 || &header[..4] != b"GGUF") {
        return Err(ModelError::Verification(
            "artifact does not contain a GGUF header".to_owned(),
        ));
    }
    if entry.format == "safetensors"
        && (read < 8 || u64::from_le_bytes(header) > entry.bytes.saturating_sub(8))
    {
        return Err(ModelError::Verification(
            "artifact does not contain a bounded safetensors header".to_owned(),
        ));
    }
    file.rewind().await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != entry.sha256 {
        return Err(ModelError::Verification(format!(
            "SHA-256 mismatch: expected {}, received {actual}",
            entry.sha256
        )));
    }
    Ok(())
}

fn safe_component(value: &str) -> Result<&str, ModelError> {
    if value.is_empty()
        || value.len() > 200
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(ModelError::Invalid(
            "unsafe artifact path component".to_owned(),
        ));
    }
    Ok(value)
}

fn reserve_loopback_port() -> Result<u16, ModelError> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

fn to_i64(value: u64, name: &str) -> Result<i64, ModelError> {
    i64::try_from(value).map_err(|_| ModelError::Invalid(format!("{name} exceeds SQLite range")))
}

fn to_u64(value: i64, name: &str) -> Result<u64, ModelError> {
    u64::try_from(value).map_err(|_| ModelError::Invalid(format!("stored {name} is negative")))
}

fn to_u32(value: i64, name: &str) -> Result<u32, ModelError> {
    u32::try_from(value).map_err(|_| ModelError::Invalid(format!("stored {name} is out of range")))
}

#[allow(clippy::similar_names)]
fn model_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<InstalledModel, ModelError> {
    let capabilities_json: String = row.try_get("capabilities_json")?;
    let byte_size = to_u64(row.try_get("byte_size")?, "byte size")?;
    let context_limit = to_u32(row.try_get("context_limit")?, "context limit")?;
    let estimated_ram_bytes = to_u64(row.try_get("estimated_ram_bytes")?, "RAM estimate")?;
    let estimated_vram_bytes = row
        .try_get::<Option<i64>, _>("estimated_vram_bytes")?
        .map(|value| to_u64(value, "VRAM estimate"))
        .transpose()?;
    let in_use_count = to_u32(row.try_get("in_use_count")?, "in-use count")?;
    Ok(InstalledModel {
        id: row.try_get("id")?,
        catalog_id: row.try_get("catalog_id")?,
        logical_model_id: row.try_get("logical_model_id")?,
        display_name: row.try_get("display_name")?,
        catalog_version: row.try_get("catalog_version")?,
        provider: row.try_get("provider")?,
        repository: row.try_get("repository")?,
        revision: row.try_get("revision")?,
        artifact_name: row.try_get("artifact_name")?,
        format: row.try_get("format")?,
        quantization: row.try_get("quantization")?,
        byte_size,
        sha256: row.try_get("sha256")?,
        local_path: row.try_get("local_path")?,
        verification_state: row.try_get("verification_state")?,
        capabilities: serde_json::from_str(&capabilities_json)?,
        compatibility_state: row.try_get("compatibility_state")?,
        compatibility_reason: row.try_get("compatibility_reason")?,
        context_limit,
        tokenizer: row.try_get("tokenizer")?,
        chat_template: row.try_get("chat_template")?,
        estimated_ram_bytes,
        estimated_vram_bytes,
        loaded: row.try_get::<i64, _>("loaded")? != 0,
        in_use_count,
        installed_at: row.try_get("installed_at")?,
        verified_at: row.try_get("verified_at")?,
        last_loaded_at: row.try_get("last_loaded_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn now() -> Result<String, time::error::Format> {
    OffsetDateTime::now_utc().format(&Rfc3339)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{HeaderMap, Response, StatusCode},
        routing::get,
    };

    use crate::catalog::{RuntimeStatus, SetupProfile};

    #[test]
    fn rejects_path_traversal_components() {
        assert!(safe_component("model.gguf").is_ok());
        assert!(safe_component("../model.gguf").is_err());
        assert!(safe_component("folder/model.gguf").is_err());
        assert!(safe_component("folder\\model.gguf").is_err());
    }

    #[derive(Clone)]
    struct Fixture {
        bytes: Arc<Vec<u8>>,
        saw_range: Arc<AtomicBool>,
    }

    async fn fixture(State(fixture): State<Fixture>, headers: HeaderMap) -> Response<Body> {
        let offset = headers
            .get(RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("bytes="))
            .and_then(|value| value.strip_suffix('-'))
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        fixture.saw_range.store(offset > 0, Ordering::SeqCst);
        let status = if offset > 0 {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        Response::builder()
            .status(status)
            .header(CONTENT_LENGTH, fixture.bytes.len().saturating_sub(offset))
            .header("accept-ranges", "bytes")
            .body(Body::from(fixture.bytes[offset..].to_vec()))
            .unwrap()
    }

    fn test_entry(url: String, bytes: &[u8], sha256: String) -> CatalogEntry {
        CatalogEntry {
            id: "fixture-model".to_owned(),
            name: "Fixture model".to_owned(),
            roles: vec!["minimal_text_chat".to_owned()],
            provider: "test".to_owned(),
            repository: "test/fixture".to_owned(),
            revision: "0123456789012345678901234567890123456789".to_owned(),
            artifact: "fixture.gguf".to_owned(),
            download_url: url,
            format: "gguf".to_owned(),
            quantization: "Q4_K_M".to_owned(),
            bytes: bytes.len() as u64,
            sha256,
            license: "Apache-2.0".to_owned(),
            license_url: "https://example.invalid/license".to_owned(),
            access: "public".to_owned(),
            runtime: "llama_cpp".to_owned(),
            runtime_status: RuntimeStatus::Executable,
            context_limit: 128,
            tokenizer: "test".to_owned(),
            chat_template: Some("test".to_owned()),
            capabilities: vec!["text_input".to_owned(), "text_output".to_owned()],
            estimated_ram_bytes: 1,
            estimated_vram_bytes: None,
            disk_overhead_bytes: 1,
            reason: "test fixture".to_owned(),
        }
    }

    async fn test_manager(directory: &tempfile::TempDir, entry: CatalogEntry) -> ModelManager {
        let store = JobStore::open_path(&directory.path().join("test.sqlite3"))
            .await
            .unwrap();
        let mut manager = ModelManager::open(store, directory.path()).await.unwrap();
        manager.catalog = Arc::new(ModelCatalog {
            schema_version: 1,
            catalog_version: "test".to_owned(),
            entries: vec![entry],
            profiles: vec![SetupProfile {
                id: "minimal".to_owned(),
                name: "Minimal".to_owned(),
                entry_ids: vec!["fixture-model".to_owned()],
            }],
        });
        manager
    }

    #[tokio::test]
    async fn range_resume_verifies_then_atomically_registers() {
        let bytes = b"GGUF-small-controlled-fixture".to_vec();
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let saw_range = Arc::new(AtomicBool::new(false));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/model", get(fixture))
                    .with_state(Fixture {
                        bytes: Arc::new(bytes.clone()),
                        saw_range: saw_range.clone(),
                    }),
            )
            .into_future(),
        );
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager(
            &directory,
            test_entry(format!("http://{address}/model"), &bytes, sha256),
        )
        .await;
        fs::write(manager.temp_root.join("fixture-model.part"), &bytes[..4])
            .await
            .unwrap();
        let (job, _) = manager
            .submit_download(DownloadSubmission {
                catalog_id: "fixture-model".to_owned(),
                idempotency_key: "range-resume".to_owned(),
                license_accepted: true,
            })
            .await
            .unwrap();
        let claimed = manager.store.claim_next_queued().await.unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        manager.execute_download(&claimed).await.unwrap();
        assert!(saw_range.load(Ordering::SeqCst));
        let inventory = manager.inventory().await.unwrap();
        assert_eq!(inventory.models.len(), 1);
        assert!(!manager.temp_root.join("fixture-model.part").exists());
        server.abort();
    }

    #[tokio::test]
    async fn hash_failure_never_registers_or_finalizes() {
        let bytes = b"GGUF-corrupt-controlled-fixture".to_vec();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/model", get(fixture))
                    .with_state(Fixture {
                        bytes: Arc::new(bytes.clone()),
                        saw_range: Arc::new(AtomicBool::new(false)),
                    }),
            )
            .into_future(),
        );
        let directory = tempfile::tempdir().unwrap();
        let manager = test_manager(
            &directory,
            test_entry(format!("http://{address}/model"), &bytes, "0".repeat(64)),
        )
        .await;
        manager
            .submit_download(DownloadSubmission {
                catalog_id: "fixture-model".to_owned(),
                idempotency_key: "hash-failure".to_owned(),
                license_accepted: true,
            })
            .await
            .unwrap();
        let claimed = manager.store.claim_next_queued().await.unwrap().unwrap();
        assert!(matches!(
            manager.execute_download(&claimed).await,
            Err(ModelError::Verification(_))
        ));
        assert!(manager.inventory().await.unwrap().models.is_empty());
        assert!(
            !manager
                .model_root
                .join("fixture-model/fixture.gguf")
                .exists()
        );
        server.abort();
    }
}
