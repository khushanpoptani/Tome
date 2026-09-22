use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    model::{EventType, Job, JobState},
    models::{InstalledModel, ModelError, ModelManager},
    store::{JobStore, StoreError},
};

pub const INFERENCE_SCHEMA_VERSION: u32 = 1;
pub const CONTEXT_STRATEGY: &str = "recent-complete-messages";
pub const CONTEXT_STRATEGY_VERSION: u32 = 1;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 512;
pub const DEFAULT_AGENT_TOOL_RESERVE: u32 = 256;
pub const MAX_OUTPUT_TOKENS: u32 = 4096;
pub const MAX_MESSAGES: usize = 256;
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_PERSISTED_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const CHECKPOINT_BYTES: usize = 1024;

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("inference request is invalid: {0}")]
    Invalid(String),
    #[error("context overflow: {0}")]
    ContextOverflow(String),
    #[error("runtime protocol failed: {0}")]
    Runtime(String),
    #[error("stored inference data is invalid: {0}")]
    Stored(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemInstruction {
    pub id: String,
    pub content: String,
    #[serde(default = "default_true")]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationSettings {
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default = "default_agent_tool_reserve")]
    pub agent_tool_reserve: u32,
}

impl Default for GenerationSettings {
    fn default() -> Self {
        Self {
            temperature: default_temperature(),
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            agent_tool_reserve: DEFAULT_AGENT_TOOL_RESERVE,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CorrelationMetadata {
    pub chat_id: Option<String>,
    pub user_message_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub client_revision_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceRequest {
    pub schema_version: u32,
    pub client_request_id: String,
    pub model_id: String,
    pub messages: Vec<InferenceMessage>,
    #[serde(default)]
    pub system_instructions: Vec<SystemInstruction>,
    #[serde(default)]
    pub settings: GenerationSettings,
    #[serde(default)]
    pub correlation: CorrelationMetadata,
    pub parent_job_id: Option<String>,
    pub retry_of_job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenCount {
    pub id: String,
    pub category: String,
    pub tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcludedMessage {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextManifest {
    pub schema_version: u32,
    pub strategy: String,
    pub strategy_version: u32,
    pub model_id: String,
    pub model_artifact_sha256: String,
    pub tokenizer_identity: String,
    pub chat_template_identity: String,
    pub runtime_version: String,
    pub context_limit: u32,
    pub input_token_total: u32,
    pub output_token_reserve: u32,
    pub agent_tool_reserve: u32,
    pub token_counts: Vec<TokenCount>,
    pub included_message_ids: Vec<String>,
    pub excluded_messages: Vec<ExcludedMessage>,
    pub truncation_decisions: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceDetails {
    pub schema_version: u32,
    pub job_id: String,
    pub model_id: String,
    pub model_artifact_sha256: String,
    pub request: InferenceRequest,
    pub settings: GenerationSettings,
    pub context_manifest: Option<ContextManifest>,
    pub output_text: String,
    pub output_sequence: u64,
    pub usage: Option<Value>,
    pub completion_reason: Option<String>,
    pub warnings: Vec<String>,
}

pub trait ExactTokenizer: Send + Sync {
    fn count<'a>(
        &'a self,
        messages: &'a [InferenceMessage],
    ) -> Pin<Box<dyn Future<Output = Result<u32, InferenceError>> + Send + 'a>>;
}

/// Selects whole messages and builds the exact, deterministic context manifest.
///
/// # Errors
///
/// Returns validation, tokenizer/runtime, or context-overflow failures.
#[allow(clippy::too_many_lines)]
pub async fn build_context_manifest<T: ExactTokenizer + ?Sized>(
    tokenizer: &T,
    model: &InstalledModel,
    runtime_version: &str,
    request: &InferenceRequest,
) -> Result<(ContextManifest, Vec<InferenceMessage>), InferenceError> {
    validate_request(request)?;
    let available = model
        .context_limit
        .checked_sub(request.settings.max_output_tokens)
        .and_then(|value| value.checked_sub(request.settings.agent_tool_reserve))
        .ok_or_else(|| {
            InferenceError::ContextOverflow(
                "output and agent/tool reserves exceed the model context limit".to_owned(),
            )
        })?;
    let current = request
        .messages
        .last()
        .ok_or_else(|| InferenceError::Invalid("at least one message is required".to_owned()))?;
    let pinned = request
        .system_instructions
        .iter()
        .filter(|instruction| instruction.pinned)
        .map(|instruction| InferenceMessage {
            id: instruction.id.clone(),
            role: MessageRole::System,
            content: instruction.content.clone(),
        })
        .collect::<Vec<_>>();
    let mut selected = pinned.clone();
    selected.push(current.clone());
    let required_tokens = tokenizer.count(&selected).await?;
    if required_tokens > available {
        return Err(InferenceError::ContextOverflow(format!(
            "pinned instructions and the current user message require {required_tokens} tokens, but {available} are available after reserves"
        )));
    }

    let mut excluded = Vec::new();
    let mut system_context = pinned;
    for instruction in request
        .system_instructions
        .iter()
        .filter(|instruction| !instruction.pinned)
    {
        let message = InferenceMessage {
            id: instruction.id.clone(),
            role: MessageRole::System,
            content: instruction.content.clone(),
        };
        let mut candidate = system_context.clone();
        candidate.push(message.clone());
        candidate.push(current.clone());
        if tokenizer.count(&candidate).await? <= available {
            system_context.push(message);
        } else {
            excluded.push(ExcludedMessage {
                id: instruction.id.clone(),
                reason: "unpinned_system_instruction_excluded_by_context_budget".to_owned(),
            });
        }
    }

    let previous = &request.messages[..request.messages.len() - 1];
    let mut chosen_reverse = Vec::new();
    let mut budget_exhausted = false;
    for message in previous.iter().rev() {
        if budget_exhausted {
            excluded.push(ExcludedMessage {
                id: message.id.clone(),
                reason: "older_than_context_cutoff".to_owned(),
            });
            continue;
        }
        let mut candidate = system_context.clone();
        candidate.extend(chosen_reverse.iter().rev().cloned());
        candidate.insert(system_context.len(), message.clone());
        candidate.push(current.clone());
        if tokenizer.count(&candidate).await? <= available {
            chosen_reverse.push(message.clone());
        } else {
            budget_exhausted = true;
            excluded.push(ExcludedMessage {
                id: message.id.clone(),
                reason: "excluded_by_context_budget".to_owned(),
            });
        }
    }
    selected = system_context;
    selected.extend(chosen_reverse.into_iter().rev());
    selected.push(current.clone());
    let input_token_total = tokenizer.count(&selected).await?;
    let mut token_counts = Vec::with_capacity(selected.len());
    let mut prefix = Vec::with_capacity(selected.len());
    let mut prior_prefix_tokens = 0_u32;
    for message in &selected {
        prefix.push(message.clone());
        let prefix_tokens = tokenizer.count(&prefix).await?;
        let count = prefix_tokens.saturating_sub(prior_prefix_tokens);
        prior_prefix_tokens = prefix_tokens;
        token_counts.push(TokenCount {
            id: message.id.clone(),
            category: match message.role {
                MessageRole::System => "system_instruction",
                MessageRole::User => "user_message",
                MessageRole::Assistant => "assistant_message",
            }
            .to_owned(),
            tokens: count,
        });
    }
    excluded.sort_by(|left, right| left.id.cmp(&right.id));
    let included_message_ids = selected.iter().map(|message| message.id.clone()).collect();
    let truncation_decisions = if excluded.is_empty() {
        Vec::new()
    } else {
        vec!["Older complete messages were excluded; no message content was truncated.".to_owned()]
    };
    let manifest = ContextManifest {
        schema_version: 1,
        strategy: CONTEXT_STRATEGY.to_owned(),
        strategy_version: CONTEXT_STRATEGY_VERSION,
        model_id: model.id.clone(),
        model_artifact_sha256: model.sha256.clone(),
        tokenizer_identity: model.tokenizer.clone(),
        chat_template_identity: model
            .chat_template
            .clone()
            .unwrap_or_else(|| "runtime-embedded".to_owned()),
        runtime_version: runtime_version.to_owned(),
        context_limit: model.context_limit,
        input_token_total,
        output_token_reserve: request.settings.max_output_tokens,
        agent_tool_reserve: request.settings.agent_tool_reserve,
        token_counts,
        included_message_ids,
        excluded_messages: excluded,
        truncation_decisions,
        warnings: Vec::new(),
    };
    Ok((manifest, selected))
}

/// Validates the complete versioned inference request and safety limits.
///
/// # Errors
///
/// Returns [`InferenceError::Invalid`] for any unsupported or unsafe field.
pub fn validate_request(request: &InferenceRequest) -> Result<(), InferenceError> {
    if request.schema_version != INFERENCE_SCHEMA_VERSION {
        return Err(InferenceError::Invalid(format!(
            "schema_version must be {INFERENCE_SCHEMA_VERSION}"
        )));
    }
    if request.client_request_id.trim().is_empty() || request.client_request_id.len() > 128 {
        return Err(InferenceError::Invalid(
            "client_request_id must contain 1 to 128 characters".to_owned(),
        ));
    }
    if request.model_id.trim().is_empty() || request.model_id.len() > 256 {
        return Err(InferenceError::Invalid("model_id is required".to_owned()));
    }
    if request.messages.is_empty() || request.messages.len() > MAX_MESSAGES {
        return Err(InferenceError::Invalid(format!(
            "messages must contain 1 to {MAX_MESSAGES} entries"
        )));
    }
    if request.messages.last().map(|message| &message.role) != Some(&MessageRole::User) {
        return Err(InferenceError::Invalid(
            "the current (last) message must have role user".to_owned(),
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for (index, message) in request.messages.iter().enumerate() {
        validate_content(&message.id, &message.content)?;
        let expected = if index % 2 == 0 {
            MessageRole::User
        } else {
            MessageRole::Assistant
        };
        if message.role != expected {
            return Err(InferenceError::Invalid(
                "messages must begin with user and alternate user/assistant; system content belongs in system_instructions"
                    .to_owned(),
            ));
        }
        if !ids.insert(&message.id) {
            return Err(InferenceError::Invalid(
                "message IDs must be unique".to_owned(),
            ));
        }
    }
    for instruction in &request.system_instructions {
        validate_content(&instruction.id, &instruction.content)?;
        if !ids.insert(&instruction.id) {
            return Err(InferenceError::Invalid(
                "instruction and message IDs must be unique".to_owned(),
            ));
        }
    }
    if !request.settings.temperature.is_finite()
        || !(0.0..=2.0).contains(&request.settings.temperature)
    {
        return Err(InferenceError::Invalid(
            "temperature must be between 0 and 2".to_owned(),
        ));
    }
    if request.settings.max_output_tokens == 0
        || request.settings.max_output_tokens > MAX_OUTPUT_TOKENS
    {
        return Err(InferenceError::Invalid(format!(
            "max_output_tokens must be between 1 and {MAX_OUTPUT_TOKENS}"
        )));
    }
    if request.settings.agent_tool_reserve > MAX_OUTPUT_TOKENS {
        return Err(InferenceError::Invalid(format!(
            "agent_tool_reserve must not exceed {MAX_OUTPUT_TOKENS}"
        )));
    }
    let bytes = serde_json::to_vec(request)
        .map_err(|error| InferenceError::Invalid(error.to_string()))?
        .len();
    if bytes > MAX_REQUEST_BYTES {
        return Err(InferenceError::Invalid(format!(
            "request exceeds the {MAX_REQUEST_BYTES} byte limit"
        )));
    }
    Ok(())
}

fn validate_content(id: &str, content: &str) -> Result<(), InferenceError> {
    if id.trim().is_empty() || id.len() > 128 {
        return Err(InferenceError::Invalid(
            "message and instruction IDs must contain 1 to 128 characters".to_owned(),
        ));
    }
    if content.is_empty() || content.len() > MAX_MESSAGE_BYTES {
        return Err(InferenceError::Invalid(format!(
            "message content must contain 1 to {MAX_MESSAGE_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

#[derive(Clone)]
pub struct InferenceService {
    store: JobStore,
    models: ModelManager,
    execution_lane: Arc<Mutex<()>>,
}

impl InferenceService {
    #[must_use]
    pub fn new(store: JobStore, models: ModelManager) -> Self {
        Self {
            store,
            models,
            execution_lane: Arc::new(Mutex::new(())),
        }
    }

    /// Validates and durably submits one idempotent inference job.
    ///
    /// # Errors
    ///
    /// Returns request, model-readiness, idempotency, or persistence failures.
    pub async fn submit(&self, request: InferenceRequest) -> Result<(Job, bool), InferenceError> {
        validate_request(&request)?;
        let model = self.models.model_for_inference(&request.model_id).await?;
        let (job, created) = self
            .store
            .create_inference_job(&request, &model.id, &model.sha256)
            .await?;
        Ok((job, created))
    }

    /// Reads the authoritative inference record for a job.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when the job is absent or malformed.
    pub async fn details(&self, job_id: &str) -> Result<InferenceDetails, InferenceError> {
        Ok(self.store.inference_details(job_id).await?)
    }

    /// Executes one claimed job through the managed runtime and safe lane.
    ///
    /// # Errors
    ///
    /// Returns context, runtime, model, cancellation-state, or persistence failures.
    pub async fn execute(&self, job: &Job) -> Result<Value, InferenceError> {
        let _lane = self.execution_lane.lock().await;
        if self.store.get_job(&job.id).await?.state != JobState::Running {
            return Err(InferenceError::Invalid(
                "job is no longer running".to_owned(),
            ));
        }
        let request: InferenceRequest = serde_json::from_value(job.input.clone())
            .map_err(|error| InferenceError::Stored(error.to_string()))?;
        validate_request(&request)?;
        let runtime = self.models.runtime_for_inference(&request.model_id).await?;
        let tokenizer = RuntimeTokenizer {
            client: self.models.http_client(),
            base_url: runtime.base_url.clone(),
        };
        let (manifest, selected) = build_context_manifest(
            &tokenizer,
            &runtime.model,
            &runtime.runtime_version,
            &request,
        )
        .await?;
        self.store.save_context_manifest(&job.id, &manifest).await?;
        self.store
            .inference_event(
                &job.id,
                EventType::InferenceContextPrepared,
                json!({
                    "schema_version": 1,
                    "input_tokens": manifest.input_token_total,
                    "included_message_ids": manifest.included_message_ids,
                    "excluded_messages": manifest.excluded_messages,
                    "output_reserve": manifest.output_token_reserve,
                    "agent_tool_reserve": manifest.agent_tool_reserve,
                }),
            )
            .await?;
        self.store
            .inference_event(
                &job.id,
                EventType::InferenceGenerationStarted,
                json!({ "schema_version": 1, "output_sequence": 0 }),
            )
            .await?;
        self.models.mark_in_use(&request.model_id, 1).await?;
        let result = self
            .stream_generation(job, &request, &runtime, &selected)
            .await;
        self.models.mark_in_use(&request.model_id, -1).await?;
        result
    }

    #[allow(clippy::too_many_lines)]
    async fn stream_generation(
        &self,
        job: &Job,
        request: &InferenceRequest,
        runtime: &crate::models::InferenceRuntime,
        messages: &[InferenceMessage],
    ) -> Result<Value, InferenceError> {
        let body_messages = messages
            .iter()
            .map(|message| {
                json!({
                    "role": match message.role { MessageRole::System => "system", MessageRole::User => "user", MessageRole::Assistant => "assistant" },
                    "content": message.content,
                })
            })
            .collect::<Vec<_>>();
        let response = self
            .models
            .http_client()
            .post(format!("{}/v1/chat/completions", runtime.base_url))
            .timeout(Duration::from_hours(1))
            .json(&json!({
                "messages": body_messages,
                "temperature": request.settings.temperature,
                "max_tokens": request.settings.max_output_tokens,
                "stream": true,
                "stream_options": { "include_usage": true },
            }))
            .send()
            .await
            .map_err(|error| InferenceError::Runtime(redact_runtime_error(&error.to_string())))?;
        if !response.status().is_success() {
            return Err(InferenceError::Runtime(format!(
                "llama.cpp returned HTTP {}",
                response.status()
            )));
        }
        let mut bytes = response.bytes_stream();
        let mut pending = String::new();
        let mut output = String::new();
        let mut coalesced_delta = String::new();
        let mut sequence = 0_u64;
        let mut persisted_at = 0_usize;
        let mut last_delta_event = Instant::now();
        let mut finish_reason = "stop".to_owned();
        let mut usage = None;
        loop {
            if self.store.get_job(&job.id).await?.state == JobState::Cancelled {
                flush_delta(&self.store, &job.id, &mut coalesced_delta, &mut sequence).await?;
                self.store
                    .save_output_checkpoint(&job.id, &output, sequence, usage.as_ref())
                    .await?;
                self.store
                    .save_inference_terminal(&job.id, &output, sequence, usage.as_ref(), "stopped")
                    .await?;
                return Ok(json!({
                    "output_sequence": sequence,
                    "output_bytes": output.len(),
                    "completion_reason": "stopped"
                }));
            }
            let chunk = match tokio::time::timeout(Duration::from_millis(100), bytes.next()).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => continue,
            };
            let chunk = chunk.map_err(|error| {
                InferenceError::Runtime(redact_runtime_error(&error.to_string()))
            })?;
            let text = std::str::from_utf8(&chunk)
                .map_err(|_| InferenceError::Runtime("runtime emitted invalid UTF-8".to_owned()))?;
            pending.push_str(text);
            while let Some(end) = pending.find('\n') {
                let line = pending[..end].trim_end_matches('\r').to_owned();
                pending.drain(..=end);
                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                if data == "[DONE]" {
                    continue;
                }
                let value: Value = serde_json::from_str(data).map_err(|_| {
                    InferenceError::Runtime("runtime emitted malformed stream JSON".to_owned())
                })?;
                if let Some(reason) = value
                    .pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str)
                {
                    finish_reason = reason.to_owned();
                }
                if let Some(value_usage) = value.get("usage") {
                    usage = Some(value_usage.clone());
                }
                let delta = value
                    .pointer("/choices/0/delta/content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    continue;
                }
                if output.len().saturating_add(delta.len()) > MAX_PERSISTED_OUTPUT_BYTES {
                    return Err(InferenceError::Runtime(
                        "generated output exceeded the server safety limit".to_owned(),
                    ));
                }
                output.push_str(delta);
                coalesced_delta.push_str(delta);
                if coalesced_delta.len() >= 128
                    || last_delta_event.elapsed() >= Duration::from_millis(50)
                {
                    flush_delta(&self.store, &job.id, &mut coalesced_delta, &mut sequence).await?;
                    last_delta_event = Instant::now();
                }
                if output.len().saturating_sub(persisted_at) >= CHECKPOINT_BYTES {
                    flush_delta(&self.store, &job.id, &mut coalesced_delta, &mut sequence).await?;
                    self.store
                        .save_output_checkpoint(&job.id, &output, sequence, usage.as_ref())
                        .await?;
                    persisted_at = output.len();
                }
            }
        }
        flush_delta(&self.store, &job.id, &mut coalesced_delta, &mut sequence).await?;
        self.store
            .save_output_checkpoint(&job.id, &output, sequence, usage.as_ref())
            .await?;
        if let Some(usage) = usage.as_ref() {
            self.store
                .inference_event(
                    &job.id,
                    EventType::InferenceUsageUpdated,
                    json!({ "schema_version": 1, "usage": usage }),
                )
                .await?;
        }
        self.store
            .save_inference_terminal(&job.id, &output, sequence, usage.as_ref(), &finish_reason)
            .await?;
        Ok(json!({
            "output_sequence": sequence,
            "output_bytes": output.len(),
            "usage": usage,
            "completion_reason": finish_reason,
        }))
    }
}

async fn flush_delta(
    store: &JobStore,
    job_id: &str,
    delta: &mut String,
    sequence: &mut u64,
) -> Result<(), InferenceError> {
    if delta.is_empty() {
        return Ok(());
    }
    *sequence = sequence.saturating_add(1);
    let text = std::mem::take(delta);
    store
        .append_inference_delta(job_id, &text, *sequence)
        .await?;
    Ok(())
}

struct RuntimeTokenizer {
    client: reqwest::Client,
    base_url: String,
}

impl ExactTokenizer for RuntimeTokenizer {
    fn count<'a>(
        &'a self,
        messages: &'a [InferenceMessage],
    ) -> Pin<Box<dyn Future<Output = Result<u32, InferenceError>> + Send + 'a>> {
        Box::pin(async move {
            let messages = messages
            .iter()
            .map(|message| {
                json!({
                    "role": match message.role { MessageRole::System => "system", MessageRole::User => "user", MessageRole::Assistant => "assistant" },
                    "content": message.content,
                })
            })
            .collect::<Vec<_>>();
            let applied = self
                .client
                .post(format!("{}/apply-template", self.base_url))
                .json(&json!({ "messages": messages, "add_generation_prompt": true }))
                .send()
                .await
                .map_err(|error| {
                    InferenceError::Runtime(redact_runtime_error(&error.to_string()))
                })?;
            if !applied.status().is_success() {
                return Err(InferenceError::Runtime(format!(
                    "chat template endpoint returned HTTP {}",
                    applied.status()
                )));
            }
            let applied: Value = applied.json().await.map_err(|_| {
                InferenceError::Runtime("chat template response was invalid".to_owned())
            })?;
            let prompt = applied
                .get("prompt")
                .and_then(Value::as_str)
                .or_else(|| applied.get("content").and_then(Value::as_str))
                .ok_or_else(|| {
                    InferenceError::Runtime("chat template response omitted prompt".to_owned())
                })?;
            let tokenized = self
                .client
                .post(format!("{}/tokenize", self.base_url))
                .json(&json!({ "content": prompt, "add_special": true }))
                .send()
                .await
                .map_err(|error| {
                    InferenceError::Runtime(redact_runtime_error(&error.to_string()))
                })?;
            if !tokenized.status().is_success() {
                return Err(InferenceError::Runtime(format!(
                    "tokenizer endpoint returned HTTP {}",
                    tokenized.status()
                )));
            }
            let tokenized: Value = tokenized.json().await.map_err(|_| {
                InferenceError::Runtime("tokenizer response was invalid".to_owned())
            })?;
            let count = tokenized
                .get("tokens")
                .and_then(Value::as_array)
                .map(Vec::len)
                .or_else(|| {
                    tokenized
                        .get("count")
                        .and_then(Value::as_u64)
                        .and_then(|value| usize::try_from(value).ok())
                })
                .ok_or_else(|| {
                    InferenceError::Runtime("tokenizer response omitted token count".to_owned())
                })?;
            u32::try_from(count).map_err(|_| {
                InferenceError::Runtime("token count exceeded supported range".to_owned())
            })
        })
    }
}

fn redact_runtime_error(message: &str) -> String {
    let mut safe = message.replace("Authorization", "[redacted]");
    if safe.len() > 512 {
        safe.truncate(512);
    }
    safe
}

fn default_true() -> bool {
    true
}
const fn default_temperature() -> f32 {
    0.7
}
const fn default_max_output_tokens() -> u32 {
    DEFAULT_MAX_OUTPUT_TOKENS
}
const fn default_agent_tool_reserve() -> u32 {
    DEFAULT_AGENT_TOOL_RESERVE
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Words;
    impl ExactTokenizer for Words {
        fn count<'a>(
            &'a self,
            messages: &'a [InferenceMessage],
        ) -> Pin<Box<dyn Future<Output = Result<u32, InferenceError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(messages
                    .iter()
                    .map(|message| {
                        u32::try_from(message.content.split_whitespace().count()).unwrap() + 1
                    })
                    .sum::<u32>()
                    + 1)
            })
        }
    }

    fn model(limit: u32) -> InstalledModel {
        InstalledModel {
            id: "model-a".into(),
            catalog_id: "a".into(),
            logical_model_id: "a/rev".into(),
            display_name: "A".into(),
            catalog_version: "test".into(),
            provider: "test".into(),
            repository: "test/a".into(),
            revision: "rev".into(),
            artifact_name: "a.gguf".into(),
            format: "gguf".into(),
            quantization: "Q4_K_M".into(),
            byte_size: 4,
            sha256: "abc".into(),
            local_path: "/safe/a.gguf".into(),
            verification_state: "verified".into(),
            capabilities: vec!["text_output".into()],
            compatibility_state: "compatible".into(),
            compatibility_reason: "ok".into(),
            context_limit: limit,
            tokenizer: "fixture".into(),
            chat_template: Some("fixture-template".into()),
            estimated_ram_bytes: 1,
            estimated_vram_bytes: None,
            loaded: false,
            in_use_count: 0,
            installed_at: "now".into(),
            verified_at: "now".into(),
            last_loaded_at: None,
            updated_at: "now".into(),
        }
    }

    fn request() -> InferenceRequest {
        InferenceRequest {
            schema_version: 1,
            client_request_id: "request-1".into(),
            model_id: "model-a".into(),
            messages: vec![
                InferenceMessage {
                    id: "u1".into(),
                    role: MessageRole::User,
                    content: "old question here".into(),
                },
                InferenceMessage {
                    id: "a1".into(),
                    role: MessageRole::Assistant,
                    content: "old answer here".into(),
                },
                InferenceMessage {
                    id: "u2".into(),
                    role: MessageRole::User,
                    content: "current question".into(),
                },
            ],
            system_instructions: vec![SystemInstruction {
                id: "s1".into(),
                content: "be useful".into(),
                pinned: true,
            }],
            settings: GenerationSettings {
                temperature: 0.7,
                max_output_tokens: 4,
                agent_tool_reserve: 3,
            },
            correlation: CorrelationMetadata::default(),
            parent_job_id: None,
            retry_of_job_id: None,
        }
    }

    #[tokio::test]
    async fn manifest_is_deterministic_and_never_truncates_messages() {
        let first = build_context_manifest(&Words, &model(18), "fixture-runtime", &request())
            .await
            .unwrap();
        let second = build_context_manifest(&Words, &model(18), "fixture-runtime", &request())
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.0.included_message_ids, vec!["s1", "a1", "u2"]);
        assert_eq!(first.0.excluded_messages[0].id, "u1");
    }

    #[tokio::test]
    async fn pinned_and_current_overflow_is_clear() {
        let error = build_context_manifest(&Words, &model(10), "fixture-runtime", &request())
            .await
            .unwrap_err();
        assert!(matches!(error, InferenceError::ContextOverflow(_)));
    }
}
