use std::{path::Path, str::FromStr, time::Duration};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    inference::{ContextManifest, GenerationSettings, InferenceDetails, InferenceRequest},
    model::{EventType, Job, JobEvent, JobState, JobType},
};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("stored data is invalid: {0}")]
    InvalidData(String),
    #[error("job was not found")]
    NotFound,
    #[error("job state does not allow this operation")]
    InvalidState,
    #[error("idempotency key is already associated with a different request")]
    IdempotencyConflict,
}

#[derive(Debug, Clone)]
pub struct CreateJob {
    pub idempotency_key: String,
    pub job_type: JobType,
    pub input: Value,
    pub parent_job_id: Option<String>,
    pub retry_of_job_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JobStore {
    pub(crate) pool: SqlitePool,
    sender: broadcast::Sender<JobEvent>,
}

#[allow(clippy::missing_errors_doc)]
impl JobStore {
    pub async fn open(database_url: &str) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        Self::connect(options).await
    }

    pub async fn open_path(path: &Path) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        Self::connect(options).await
    }

    async fn connect(options: SqliteConnectOptions) -> Result<Self, StoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        run_migrations(&pool).await?;
        let (sender, _) = broadcast::channel(512);
        Ok(Self { pool, sender })
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.sender.subscribe()
    }

    pub async fn create_job(&self, request: CreateJob) -> Result<(Job, bool), StoreError> {
        let now = now()?;
        let id = Uuid::now_v7().to_string();
        let input_json = serde_json::to_string(&request.input)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let mut transaction = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO jobs
             (id, idempotency_key, job_type, state, progress, input_json, parent_job_id,
              retry_of_job_id, created_at, updated_at)
             VALUES (?, ?, ?, 'queued', 0.0, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&request.idempotency_key)
        .bind(request.job_type.to_string())
        .bind(input_json)
        .bind(&request.parent_job_id)
        .bind(&request.retry_of_job_id)
        .bind(&now)
        .bind(&now)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;

        if !inserted {
            let row = sqlx::query("SELECT * FROM jobs WHERE idempotency_key = ?")
                .bind(&request.idempotency_key)
                .fetch_one(&mut *transaction)
                .await?;
            let existing = job_from_row(&row)?;
            if existing.job_type != request.job_type
                || existing.input != request.input
                || existing.parent_job_id != request.parent_job_id
                || existing.retry_of_job_id != request.retry_of_job_id
            {
                return Err(StoreError::IdempotencyConflict);
            }
            transaction.commit().await?;
            return Ok((existing, false));
        }

        let created = insert_event(
            &mut transaction,
            Some(&id),
            EventType::JobCreated,
            json!({ "job_type": request.job_type }),
            &now,
        )
        .await?;
        let queued = insert_event(
            &mut transaction,
            Some(&id),
            EventType::JobQueued,
            json!({}),
            &now,
        )
        .await?;
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
            .bind(&id)
            .fetch_one(&mut *transaction)
            .await?;
        let job = job_from_row(&row)?;
        transaction.commit().await?;
        self.publish([created, queued]);
        Ok((job, true))
    }

    pub async fn create_inference_job(
        &self,
        request: &InferenceRequest,
        model_id: &str,
        artifact_sha256: &str,
    ) -> Result<(Job, bool), StoreError> {
        let now = now()?;
        let id = Uuid::now_v7().to_string();
        let input = serde_json::to_value(request)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let input_json = serialize(&input)?;
        let mut transaction = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO jobs
             (id, idempotency_key, job_type, state, progress, input_json, parent_job_id,
              retry_of_job_id, created_at, updated_at)
             VALUES (?, ?, 'inference', 'queued', 0.0, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&request.client_request_id)
        .bind(&input_json)
        .bind(&request.parent_job_id)
        .bind(&request.retry_of_job_id)
        .bind(&now)
        .bind(&now)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;

        if !inserted {
            let row = sqlx::query("SELECT * FROM jobs WHERE idempotency_key = ?")
                .bind(&request.client_request_id)
                .fetch_one(&mut *transaction)
                .await?;
            let existing = job_from_row(&row)?;
            if existing.job_type != JobType::Inference
                || existing.input != input
                || existing.parent_job_id != request.parent_job_id
                || existing.retry_of_job_id != request.retry_of_job_id
            {
                return Err(StoreError::IdempotencyConflict);
            }
            let stored: Option<(String, String)> = sqlx::query_as(
                "SELECT model_id, model_artifact_sha256 FROM inference_jobs WHERE job_id = ?",
            )
            .bind(&existing.id)
            .fetch_optional(&mut *transaction)
            .await?;
            if stored
                .as_ref()
                .map(|(stored_id, stored_hash)| (stored_id.as_str(), stored_hash.as_str()))
                != Some((model_id, artifact_sha256))
            {
                return Err(StoreError::IdempotencyConflict);
            }
            transaction.commit().await?;
            return Ok((existing, false));
        }

        sqlx::query(
            "INSERT INTO inference_jobs
             (job_id, schema_version, model_id, model_artifact_sha256, request_json,
              settings_json, created_at, updated_at)
             VALUES (?, 1, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(model_id)
        .bind(artifact_sha256)
        .bind(serialize(request)?)
        .bind(serialize(&request.settings)?)
        .bind(&now)
        .bind(&now)
        .execute(&mut *transaction)
        .await?;
        let created = insert_event(
            &mut transaction,
            Some(&id),
            EventType::JobCreated,
            json!({ "job_type": JobType::Inference }),
            &now,
        )
        .await?;
        let queued = insert_event(
            &mut transaction,
            Some(&id),
            EventType::JobQueued,
            json!({}),
            &now,
        )
        .await?;
        let job = fetch_job_in(&mut transaction, &id).await?;
        transaction.commit().await?;
        self.publish([created, queued]);
        Ok((job, true))
    }

    pub async fn get_job(&self, id: &str) -> Result<Job, StoreError> {
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        job_from_row(&row)
    }

    pub async fn list_jobs(
        &self,
        state: Option<JobState>,
        limit: u32,
    ) -> Result<Vec<Job>, StoreError> {
        let rows = if let Some(state) = state {
            sqlx::query("SELECT * FROM jobs WHERE state = ? ORDER BY created_at DESC LIMIT ?")
                .bind(state.to_string())
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query("SELECT * FROM jobs ORDER BY created_at DESC LIMIT ?")
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
        };
        rows.iter().map(job_from_row).collect()
    }

    pub async fn cancel_job(&self, id: &str) -> Result<Job, StoreError> {
        let now = now()?;
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE jobs SET state = 'cancelled', updated_at = ?, finished_at = ?
             WHERE id = ? AND state IN ('queued', 'running')",
        )
        .bind(&now)
        .bind(&now)
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed == 0 {
            let job = fetch_job_in(&mut transaction, id).await?;
            if job.state.is_terminal() {
                transaction.commit().await?;
                return Ok(job);
            }
            return Err(StoreError::InvalidState);
        }
        let event = insert_event(
            &mut transaction,
            Some(id),
            EventType::JobCancelled,
            json!({}),
            &now,
        )
        .await?;
        let inference_event = if job_type_in(&mut transaction, id).await? == JobType::Inference {
            let recorded = sqlx::query(
                "UPDATE inference_jobs SET completion_reason = 'stopped', updated_at = ?
                 WHERE job_id = ?",
            )
            .bind(&now)
            .bind(id)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
            if recorded == 1 {
                Some(
                    insert_event(
                        &mut transaction,
                        Some(id),
                        EventType::InferenceStopped,
                        json!({ "schema_version": 1, "completion_reason": "stopped" }),
                        &now,
                    )
                    .await?,
                )
            } else {
                None
            }
        } else {
            None
        };
        let job = fetch_job_in(&mut transaction, id).await?;
        transaction.commit().await?;
        self.publish(std::iter::once(event).chain(inference_event));
        Ok(job)
    }

    pub async fn pause_job(&self, id: &str) -> Result<Job, StoreError> {
        let now = now()?;
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE jobs SET state = 'interrupted', updated_at = ?, finished_at = ?,
             error_json = ? WHERE id = ? AND job_type = 'model_download'
             AND state IN ('queued', 'running')",
        )
        .bind(&now)
        .bind(&now)
        .bind(
            json!({ "code": "download_paused", "message": "download paused by user" }).to_string(),
        )
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(StoreError::InvalidState);
        }
        let event = insert_event(
            &mut transaction,
            Some(id),
            EventType::JobInterrupted,
            json!({ "reason": "download_paused" }),
            &now,
        )
        .await?;
        let job = fetch_job_in(&mut transaction, id).await?;
        transaction.commit().await?;
        self.publish([event]);
        Ok(job)
    }

    pub async fn resume_job(&self, id: &str) -> Result<Job, StoreError> {
        let now = now()?;
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE jobs SET state = 'queued', error_json = NULL, finished_at = NULL,
             started_at = NULL, updated_at = ? WHERE id = ? AND job_type = 'model_download'
             AND state = 'interrupted'",
        )
        .bind(&now)
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(StoreError::InvalidState);
        }
        let event = insert_event(
            &mut transaction,
            Some(id),
            EventType::JobQueued,
            json!({ "reason": "download_resumed" }),
            &now,
        )
        .await?;
        let job = fetch_job_in(&mut transaction, id).await?;
        transaction.commit().await?;
        self.publish([event]);
        Ok(job)
    }

    pub async fn resume_interrupted_downloads(&self) -> Result<u64, StoreError> {
        let now = now()?;
        let result = sqlx::query(
            "UPDATE jobs SET state = 'queued', error_json = NULL, finished_at = NULL,
             started_at = NULL, updated_at = ? WHERE job_type = 'model_download'
             AND state = 'interrupted'
             AND json_extract(error_json, '$.code') = 'server_restarted'",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn claim_next_queued(&self) -> Result<Option<Job>, StoreError> {
        let mut transaction = self.pool.begin().await?;
        let Some(row) = sqlx::query(
            "SELECT id FROM jobs WHERE state = 'queued' ORDER BY created_at ASC LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?
        else {
            transaction.commit().await?;
            return Ok(None);
        };
        let id: String = row.try_get("id")?;
        let now = now()?;
        let changed = sqlx::query(
            "UPDATE jobs SET state = 'running', started_at = ?, updated_at = ?
             WHERE id = ? AND state = 'queued'",
        )
        .bind(&now)
        .bind(&now)
        .bind(&id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed == 0 {
            transaction.commit().await?;
            return Ok(None);
        }
        let event = insert_event(
            &mut transaction,
            Some(&id),
            EventType::JobStarted,
            json!({}),
            &now,
        )
        .await?;
        let job = fetch_job_in(&mut transaction, &id).await?;
        transaction.commit().await?;
        self.publish([event]);
        Ok(Some(job))
    }

    pub async fn update_progress(&self, id: &str, progress: f64) -> Result<Job, StoreError> {
        if !(0.0..=1.0).contains(&progress) {
            return Err(StoreError::InvalidData(
                "progress must be between zero and one".to_owned(),
            ));
        }
        let now = now()?;
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE jobs SET progress = ?, updated_at = ? WHERE id = ? AND state = 'running'",
        )
        .bind(progress)
        .bind(&now)
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(StoreError::InvalidState);
        }
        let event = insert_event(
            &mut transaction,
            Some(id),
            EventType::JobProgress,
            json!({ "progress": progress }),
            &now,
        )
        .await?;
        let job = fetch_job_in(&mut transaction, id).await?;
        transaction.commit().await?;
        self.publish([event]);
        Ok(job)
    }

    pub async fn complete_job(&self, id: &str, output: Value) -> Result<Job, StoreError> {
        self.finish_job(
            id,
            JobState::Completed,
            EventType::JobCompleted,
            Some(output),
            None,
        )
        .await
    }

    pub async fn fail_job(&self, id: &str, error: Value) -> Result<Job, StoreError> {
        self.finish_job(
            id,
            JobState::Failed,
            EventType::JobFailed,
            None,
            Some(error),
        )
        .await
    }

    async fn finish_job(
        &self,
        id: &str,
        state: JobState,
        event_type: EventType,
        output: Option<Value>,
        error: Option<Value>,
    ) -> Result<Job, StoreError> {
        let now = now()?;
        let output_json = output
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| {
                StoreError::InvalidData(format!("could not serialize output: {error}"))
            })?;
        let error_json = error
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| {
                StoreError::InvalidData(format!("could not serialize error: {error}"))
            })?;
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE jobs SET state = ?, progress = CASE WHEN ? = 'completed' THEN 1.0 ELSE progress END,
             output_json = ?, error_json = ?, updated_at = ?, finished_at = ?
             WHERE id = ? AND state = 'running'",
        )
        .bind(state.to_string())
        .bind(state.to_string())
        .bind(output_json)
        .bind(error_json)
        .bind(&now)
        .bind(&now)
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(StoreError::InvalidState);
        }
        let event = insert_event(
            &mut transaction,
            Some(id),
            event_type,
            error.or(output).unwrap_or_else(|| json!({})),
            &now,
        )
        .await?;
        let job = fetch_job_in(&mut transaction, id).await?;
        transaction.commit().await?;
        self.publish([event]);
        Ok(job)
    }

    pub async fn recover_interrupted(&self) -> Result<u64, StoreError> {
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query("SELECT id, job_type FROM jobs WHERE state = 'running'")
            .fetch_all(&mut *transaction)
            .await?;
        let now = now()?;
        let mut events = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row.try_get("id")?;
            let job_type: JobType = parse(&row.try_get::<String, _>("job_type")?)?;
            sqlx::query(
                "UPDATE jobs SET state = 'interrupted', updated_at = ?, finished_at = ?,
                 error_json = ? WHERE id = ? AND state = 'running'",
            )
            .bind(&now)
            .bind(&now)
            .bind(
                json!({
                    "code": "server_restarted",
                    "message": "job was running when the server stopped"
                })
                .to_string(),
            )
            .bind(&id)
            .execute(&mut *transaction)
            .await?;
            events.push(
                insert_event(
                    &mut transaction,
                    Some(&id),
                    EventType::JobInterrupted,
                    json!({ "reason": "server_restarted" }),
                    &now,
                )
                .await?,
            );
            if job_type == JobType::Inference {
                let recorded = sqlx::query(
                    "UPDATE inference_jobs SET completion_reason = 'interrupted', updated_at = ?
                     WHERE job_id = ?",
                )
                .bind(&now)
                .bind(&id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
                if recorded == 1 {
                    events.push(
                        insert_event(
                            &mut transaction,
                            Some(&id),
                            EventType::InferenceInterrupted,
                            json!({ "schema_version": 1, "completion_reason": "interrupted" }),
                            &now,
                        )
                        .await?,
                    );
                }
            }
        }
        transaction.commit().await?;
        self.publish(events);
        Ok(rows.len() as u64)
    }

    pub async fn events_after(
        &self,
        after_event_id: i64,
        limit: u32,
    ) -> Result<Vec<JobEvent>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM job_events WHERE event_id > ? ORDER BY event_id ASC LIMIT ?",
        )
        .bind(after_event_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    pub async fn warning(&self, code: &str, message: &str) -> Result<JobEvent, StoreError> {
        let now = now()?;
        let mut transaction = self.pool.begin().await?;
        let event = insert_event(
            &mut transaction,
            None,
            EventType::ServerWarning,
            json!({ "code": code, "message": message }),
            &now,
        )
        .await?;
        transaction.commit().await?;
        self.publish([event.clone()]);
        Ok(event)
    }

    pub async fn inference_event(
        &self,
        job_id: &str,
        event_type: EventType,
        payload: Value,
    ) -> Result<JobEvent, StoreError> {
        if payload.to_string().len() > 128 * 1024 {
            return Err(StoreError::InvalidData(
                "inference event payload exceeded 128 KiB".to_owned(),
            ));
        }
        let timestamp = now()?;
        let mut transaction = self.pool.begin().await?;
        let event = insert_event(
            &mut transaction,
            Some(job_id),
            event_type,
            payload,
            &timestamp,
        )
        .await?;
        transaction.commit().await?;
        self.publish([event.clone()]);
        Ok(event)
    }

    pub async fn save_context_manifest(
        &self,
        job_id: &str,
        manifest: &ContextManifest,
    ) -> Result<(), StoreError> {
        let changed = sqlx::query(
            "UPDATE inference_jobs SET context_manifest_json = ?, updated_at = ? WHERE job_id = ?",
        )
        .bind(serialize(manifest)?)
        .bind(now()?)
        .bind(job_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub async fn save_output_checkpoint(
        &self,
        job_id: &str,
        output: &str,
        sequence: u64,
        usage: Option<&Value>,
    ) -> Result<(), StoreError> {
        let sequence = i64::try_from(sequence).map_err(|_| {
            StoreError::InvalidData("output sequence exceeds SQLite range".to_owned())
        })?;
        let timestamp = now()?;
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE inference_jobs SET output_text = ?, output_sequence = ?, usage_json = ?,
             updated_at = ? WHERE job_id = ? AND output_sequence <= ?",
        )
        .bind(output)
        .bind(sequence)
        .bind(usage.map(serialize).transpose()?)
        .bind(&timestamp)
        .bind(job_id)
        .bind(sequence)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StoreError::InvalidState);
        }
        let checkpoint_text = (output.len() <= 96 * 1024).then_some(output);
        let event = insert_event(
            &mut transaction,
            Some(job_id),
            EventType::InferenceOutputCheckpoint,
            json!({
                "schema_version": 1,
                "output_sequence": sequence,
                "text": checkpoint_text,
                "output_bytes": output.len(),
                "output_sha256": format!("{:x}", Sha256::digest(output.as_bytes())),
                "requires_fetch": checkpoint_text.is_none(),
                "usage": usage,
            }),
            &timestamp,
        )
        .await?;
        transaction.commit().await?;
        self.publish([event]);
        Ok(())
    }

    pub async fn append_inference_delta(
        &self,
        job_id: &str,
        text: &str,
        sequence: u64,
    ) -> Result<(), StoreError> {
        if text.is_empty() || text.len() > 128 * 1024 {
            return Err(StoreError::InvalidData(
                "inference delta must contain 1 to 131072 bytes".to_owned(),
            ));
        }
        let sequence = i64::try_from(sequence).map_err(|_| {
            StoreError::InvalidData("output sequence exceeds SQLite range".to_owned())
        })?;
        let timestamp = now()?;
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE inference_jobs SET output_text = output_text || ?, output_sequence = ?,
             updated_at = ? WHERE job_id = ? AND output_sequence = ?",
        )
        .bind(text)
        .bind(sequence)
        .bind(&timestamp)
        .bind(job_id)
        .bind(sequence - 1)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StoreError::InvalidState);
        }
        let event = insert_event(
            &mut transaction,
            Some(job_id),
            EventType::InferenceTextDelta,
            json!({ "schema_version": 1, "output_sequence": sequence, "text": text }),
            &timestamp,
        )
        .await?;
        transaction.commit().await?;
        self.publish([event]);
        Ok(())
    }

    pub async fn save_inference_terminal(
        &self,
        job_id: &str,
        output: &str,
        sequence: u64,
        usage: Option<&Value>,
        completion_reason: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE inference_jobs SET output_text = ?, output_sequence = ?, usage_json = ?,
             completion_reason = ?, updated_at = ? WHERE job_id = ?",
        )
        .bind(output)
        .bind(i64::try_from(sequence).map_err(|_| {
            StoreError::InvalidData("output sequence exceeds SQLite range".to_owned())
        })?)
        .bind(usage.map(serialize).transpose()?)
        .bind(completion_reason)
        .bind(now()?)
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_inference_failed(&self, job_id: &str) -> Result<(), StoreError> {
        let changed = sqlx::query(
            "UPDATE inference_jobs SET completion_reason = 'failed', updated_at = ?
             WHERE job_id = ?",
        )
        .bind(now()?)
        .bind(job_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub async fn inference_details(&self, job_id: &str) -> Result<InferenceDetails, StoreError> {
        let row = sqlx::query("SELECT * FROM inference_jobs WHERE job_id = ?")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        let request: InferenceRequest = parse_typed(&row.try_get::<String, _>("request_json")?)?;
        let settings: GenerationSettings =
            parse_typed(&row.try_get::<String, _>("settings_json")?)?;
        let manifest = row
            .try_get::<Option<String>, _>("context_manifest_json")?
            .as_deref()
            .map(parse_typed)
            .transpose()?;
        let usage =
            parse_optional_json(row.try_get::<Option<String>, _>("usage_json")?.as_deref())?;
        let output_sequence =
            u64::try_from(row.try_get::<i64, _>("output_sequence")?).map_err(|_| {
                StoreError::InvalidData("stored output sequence is negative".to_owned())
            })?;
        let warnings: Vec<String> = parse_typed(&row.try_get::<String, _>("warnings_json")?)?;
        Ok(InferenceDetails {
            schema_version: u32::try_from(row.try_get::<i64, _>("schema_version")?).map_err(
                |_| StoreError::InvalidData("stored inference schema is invalid".to_owned()),
            )?,
            job_id: job_id.to_owned(),
            model_id: row.try_get("model_id")?,
            model_artifact_sha256: row.try_get("model_artifact_sha256")?,
            request,
            settings,
            context_manifest: manifest,
            output_text: row.try_get("output_text")?,
            output_sequence,
            usage,
            completion_reason: row.try_get("completion_reason")?,
            warnings,
        })
    }

    pub async fn cleanup_terminal_before(&self, before: &str) -> Result<u64, StoreError> {
        let mut transaction = self.pool.begin().await?;
        let ids = sqlx::query(
            "SELECT id FROM jobs WHERE state IN ('completed', 'failed', 'cancelled', 'interrupted')
             AND finished_at < ?",
        )
        .bind(before)
        .fetch_all(&mut *transaction)
        .await?;
        for row in &ids {
            let id: String = row.try_get("id")?;
            sqlx::query("DELETE FROM job_events WHERE job_id = ?")
                .bind(&id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM jobs WHERE id = ?")
                .bind(&id)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(ids.len() as u64)
    }

    fn publish(&self, events: impl IntoIterator<Item = JobEvent>) {
        for event in events {
            let _ignored = self.sender.send(event);
        }
    }
}

async fn run_migrations(pool: &SqlitePool) -> Result<(), StoreError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tome_migrations (
            version INTEGER PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tome_migrations WHERE version = 1")
        .fetch_one(pool)
        .await?;
    if applied == 0 {
        let mut transaction = pool.begin().await?;
        sqlx::raw_sql(include_str!("../migrations/0001_jobs.sql"))
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO tome_migrations (version, name, applied_at) VALUES (1, 'jobs', ?)",
        )
        .bind(now()?)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
    }
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tome_migrations WHERE version = 2")
        .fetch_one(pool)
        .await?;
    if applied == 0 {
        let mut transaction = pool.begin().await?;
        sqlx::raw_sql(include_str!("../migrations/0002_models.sql"))
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO tome_migrations (version, name, applied_at) VALUES (2, 'models', ?)",
        )
        .bind(now()?)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
    }
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tome_migrations WHERE version = 3")
        .fetch_one(pool)
        .await?;
    if applied == 0 {
        let mut transaction = pool.begin().await?;
        sqlx::raw_sql(include_str!("../migrations/0003_inference.sql"))
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO tome_migrations (version, name, applied_at) VALUES (3, 'inference', ?)",
        )
        .bind(now()?)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
    }
    Ok(())
}

async fn fetch_job_in(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Job, StoreError> {
    let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(StoreError::NotFound)?;
    job_from_row(&row)
}

async fn job_type_in(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<JobType, StoreError> {
    let value: String = sqlx::query_scalar("SELECT job_type FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(StoreError::NotFound)?;
    parse(&value)
}

async fn insert_event(
    transaction: &mut Transaction<'_, Sqlite>,
    job_id: Option<&str>,
    event_type: EventType,
    payload: Value,
    occurred_at: &str,
) -> Result<JobEvent, StoreError> {
    let payload_json = payload.to_string();
    let result = sqlx::query(
        "INSERT INTO job_events (job_id, event_type, payload_json, occurred_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(event_type.to_string())
    .bind(payload_json)
    .bind(occurred_at)
    .execute(&mut **transaction)
    .await?;
    Ok(JobEvent {
        event_id: result.last_insert_rowid(),
        job_id: job_id.map(ToOwned::to_owned),
        event_type,
        payload,
        occurred_at: occurred_at.to_owned(),
    })
}

fn job_from_row(row: &SqliteRow) -> Result<Job, StoreError> {
    Ok(Job {
        id: row.try_get("id")?,
        idempotency_key: row.try_get("idempotency_key")?,
        job_type: parse(&row.try_get::<String, _>("job_type")?)?,
        state: parse(&row.try_get::<String, _>("state")?)?,
        progress: row.try_get("progress")?,
        input: parse_json(&row.try_get::<String, _>("input_json")?)?,
        output: parse_optional_json(row.try_get::<Option<String>, _>("output_json")?.as_deref())?,
        error: parse_optional_json(row.try_get::<Option<String>, _>("error_json")?.as_deref())?,
        parent_job_id: row.try_get("parent_job_id")?,
        retry_of_job_id: row.try_get("retry_of_job_id")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
    })
}

fn event_from_row(row: &SqliteRow) -> Result<JobEvent, StoreError> {
    Ok(JobEvent {
        event_id: row.try_get("event_id")?,
        job_id: row.try_get("job_id")?,
        event_type: parse(&row.try_get::<String, _>("event_type")?)?,
        payload: parse_json(&row.try_get::<String, _>("payload_json")?)?,
        occurred_at: row.try_get("occurred_at")?,
    })
}

fn parse<T>(value: &str) -> Result<T, StoreError>
where
    T: FromStr<Err = String>,
{
    value.parse().map_err(StoreError::InvalidData)
}

fn parse_json(value: &str) -> Result<Value, StoreError> {
    serde_json::from_str(value).map_err(|error| StoreError::InvalidData(error.to_string()))
}

fn parse_optional_json(value: Option<&str>) -> Result<Option<Value>, StoreError> {
    value.map(parse_json).transpose()
}

fn serialize<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| StoreError::InvalidData(error.to_string()))
}

fn parse_typed<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|error| StoreError::InvalidData(error.to_string()))
}

fn now() -> Result<String, StoreError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| StoreError::InvalidData(error.to_string()))
}
