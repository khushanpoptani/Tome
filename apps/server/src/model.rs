use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const API_VERSION: &str = "v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobType {
    Inference,
    ModelDownload,
    ModelVerification,
    ModelLoad,
    ModelUnload,
    AttachmentProcessing,
    Ocr,
    Transcription,
}

impl JobType {
    pub const ALL: [Self; 8] = [
        Self::Inference,
        Self::ModelDownload,
        Self::ModelVerification,
        Self::ModelLoad,
        Self::ModelUnload,
        Self::AttachmentProcessing,
        Self::Ocr,
        Self::Transcription,
    ];
}

impl fmt::Display for JobType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Inference => "inference",
            Self::ModelDownload => "model_download",
            Self::ModelVerification => "model_verification",
            Self::ModelLoad => "model_load",
            Self::ModelUnload => "model_unload",
            Self::AttachmentProcessing => "attachment_processing",
            Self::Ocr => "ocr",
            Self::Transcription => "transcription",
        };
        formatter.write_str(value)
    }
}

impl FromStr for JobType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "inference" => Ok(Self::Inference),
            "model_download" => Ok(Self::ModelDownload),
            "model_verification" => Ok(Self::ModelVerification),
            "model_load" => Ok(Self::ModelLoad),
            "model_unload" => Ok(Self::ModelUnload),
            "attachment_processing" => Ok(Self::AttachmentProcessing),
            "ocr" => Ok(Self::Ocr),
            "transcription" => Ok(Self::Transcription),
            _ => Err(format!("unknown job type: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl JobState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        };
        formatter.write_str(value)
    }
}

impl FromStr for JobState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(format!("unknown job state: {value}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: String,
    pub idempotency_key: String,
    pub job_type: JobType,
    pub state: JobState,
    pub progress: f64,
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_of_job_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "job.created")]
    JobCreated,
    #[serde(rename = "job.queued")]
    JobQueued,
    #[serde(rename = "job.started")]
    JobStarted,
    #[serde(rename = "job.progress")]
    JobProgress,
    #[serde(rename = "job.completed")]
    JobCompleted,
    #[serde(rename = "job.failed")]
    JobFailed,
    #[serde(rename = "job.cancelled")]
    JobCancelled,
    #[serde(rename = "job.interrupted")]
    JobInterrupted,
    #[serde(rename = "inference.context_prepared")]
    InferenceContextPrepared,
    #[serde(rename = "inference.generation_started")]
    InferenceGenerationStarted,
    #[serde(rename = "inference.text_delta")]
    InferenceTextDelta,
    #[serde(rename = "inference.output_checkpoint")]
    InferenceOutputCheckpoint,
    #[serde(rename = "inference.usage_updated")]
    InferenceUsageUpdated,
    #[serde(rename = "inference.stopped")]
    InferenceStopped,
    #[serde(rename = "inference.completed")]
    InferenceCompleted,
    #[serde(rename = "inference.failed")]
    InferenceFailed,
    #[serde(rename = "inference.interrupted")]
    InferenceInterrupted,
    #[serde(rename = "server.warning")]
    ServerWarning,
}

impl fmt::Display for EventType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::JobCreated => "job.created",
            Self::JobQueued => "job.queued",
            Self::JobStarted => "job.started",
            Self::JobProgress => "job.progress",
            Self::JobCompleted => "job.completed",
            Self::JobFailed => "job.failed",
            Self::JobCancelled => "job.cancelled",
            Self::JobInterrupted => "job.interrupted",
            Self::InferenceContextPrepared => "inference.context_prepared",
            Self::InferenceGenerationStarted => "inference.generation_started",
            Self::InferenceTextDelta => "inference.text_delta",
            Self::InferenceOutputCheckpoint => "inference.output_checkpoint",
            Self::InferenceUsageUpdated => "inference.usage_updated",
            Self::InferenceStopped => "inference.stopped",
            Self::InferenceCompleted => "inference.completed",
            Self::InferenceFailed => "inference.failed",
            Self::InferenceInterrupted => "inference.interrupted",
            Self::ServerWarning => "server.warning",
        };
        formatter.write_str(value)
    }
}

impl FromStr for EventType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "job.created" => Ok(Self::JobCreated),
            "job.queued" => Ok(Self::JobQueued),
            "job.started" => Ok(Self::JobStarted),
            "job.progress" => Ok(Self::JobProgress),
            "job.completed" => Ok(Self::JobCompleted),
            "job.failed" => Ok(Self::JobFailed),
            "job.cancelled" => Ok(Self::JobCancelled),
            "job.interrupted" => Ok(Self::JobInterrupted),
            "inference.context_prepared" => Ok(Self::InferenceContextPrepared),
            "inference.generation_started" => Ok(Self::InferenceGenerationStarted),
            "inference.text_delta" => Ok(Self::InferenceTextDelta),
            "inference.output_checkpoint" => Ok(Self::InferenceOutputCheckpoint),
            "inference.usage_updated" => Ok(Self::InferenceUsageUpdated),
            "inference.stopped" => Ok(Self::InferenceStopped),
            "inference.completed" => Ok(Self::InferenceCompleted),
            "inference.failed" => Ok(Self::InferenceFailed),
            "inference.interrupted" => Ok(Self::InferenceInterrupted),
            "server.warning" => Ok(Self::ServerWarning),
            _ => Err(format!("unknown event type: {value}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobEvent {
    pub event_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub event_type: EventType,
    pub payload: Value,
    pub occurred_at: String,
}
