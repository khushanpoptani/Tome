use std::{collections::HashMap, str::FromStr, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        rejection::{JsonRejection, QueryRejection},
        ws::Message,
    },
    http::{HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use uuid::Uuid;

use crate::{
    config::NetworkMode,
    inference::{InferenceError, InferenceRequest, InferenceService},
    model::{API_VERSION, Job, JobEvent, JobState, JobType, PROTOCOL_VERSION},
    models::{DownloadSubmission, ModelError, ModelManager},
    store::{CreateJob, JobStore, StoreError},
};

#[derive(Clone)]
pub struct AppState {
    pub store: JobStore,
    pub network_mode: NetworkMode,
    pub models: ModelManager,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
    request_id: String,
    details: Value,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Value,
}

impl ApiError {
    fn validation(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: message.into(),
            details: json!({}),
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        let (status, code, message) = match error {
            StoreError::NotFound => (
                StatusCode::NOT_FOUND,
                "job_not_found",
                "job was not found".to_owned(),
            ),
            StoreError::InvalidState => (
                StatusCode::CONFLICT,
                "invalid_job_state",
                "the job state does not allow this operation".to_owned(),
            ),
            StoreError::IdempotencyConflict => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                error.to_string(),
            ),
            StoreError::InvalidData(_) | StoreError::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "the server could not complete the operation".to_owned(),
            ),
        };
        Self {
            status,
            code,
            message,
            details: json!({}),
        }
    }
}

impl From<ModelError> for ApiError {
    fn from(error: ModelError) -> Self {
        let (status, code) = match &error {
            ModelError::CatalogEntryNotFound | ModelError::ModelNotFound => {
                (StatusCode::NOT_FOUND, "model_not_found")
            }
            ModelError::Conflict(_) => (StatusCode::CONFLICT, "model_state_conflict"),
            ModelError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_model_request"),
            ModelError::Verification(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "model_verification_failed",
            ),
            ModelError::RuntimeUnavailable(_) => (StatusCode::CONFLICT, "runtime_unavailable"),
            ModelError::Runtime(_) => (StatusCode::BAD_GATEWAY, "runtime_failed"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
            "the server could not complete the model operation".to_owned()
        } else {
            error.to_string()
        };
        Self {
            status,
            code,
            message,
            details: json!({}),
        }
    }
}

impl From<InferenceError> for ApiError {
    fn from(error: InferenceError) -> Self {
        let (status, code) = match &error {
            InferenceError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_inference_request"),
            InferenceError::ContextOverflow(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "context_overflow")
            }
            InferenceError::Model(ModelError::ModelNotFound) => {
                (StatusCode::NOT_FOUND, "model_not_found")
            }
            InferenceError::Model(ModelError::Conflict(_)) => {
                (StatusCode::CONFLICT, "model_not_ready")
            }
            InferenceError::Model(ModelError::RuntimeUnavailable(_)) => {
                (StatusCode::CONFLICT, "runtime_unavailable")
            }
            InferenceError::Runtime(_) => (StatusCode::BAD_GATEWAY, "runtime_failed"),
            InferenceError::Store(StoreError::IdempotencyConflict) => {
                (StatusCode::CONFLICT, "idempotency_conflict")
            }
            InferenceError::Store(StoreError::NotFound) => {
                (StatusCode::NOT_FOUND, "inference_not_found")
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
            "the server could not complete the inference operation".to_owned()
        } else {
            error.to_string()
        };
        Self {
            status,
            code,
            message,
            details: json!({}),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                    request_id: Uuid::now_v7().to_string(),
                    details: self.details,
                },
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    protocol_version: u32,
}

#[derive(Debug, Serialize)]
struct VersionResponse {
    server_version: &'static str,
    api_version: &'static str,
    protocol: ProtocolRange,
}

#[derive(Debug, Serialize)]
struct ProtocolRange {
    current: u32,
    minimum: u32,
    maximum: u32,
}

#[derive(Debug, Serialize)]
struct CapabilitiesResponse {
    protocol: ProtocolRange,
    authentication: &'static str,
    network_mode: NetworkMode,
    features: Features,
    job_types: Vec<JobTypeCapability>,
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct Features {
    persistent_jobs: bool,
    event_replay: bool,
    websocket_events: bool,
    inference_token_streaming: bool,
    model_management: bool,
    hardware_discovery: bool,
    durable_model_downloads: bool,
}

#[derive(Debug, Serialize)]
struct JobTypeCapability {
    job_type: JobType,
    implemented: bool,
}

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    idempotency_key: String,
    job_type: JobType,
    #[serde(default = "empty_object")]
    input: Value,
    parent_job_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateJobResponse {
    job: Job,
    duplicate: bool,
}

#[derive(Debug, Deserialize)]
struct RetryJobRequest {
    idempotency_key: String,
}

#[derive(Debug, Deserialize)]
struct ListJobsQuery {
    state: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct JobsResponse {
    jobs: Vec<Job>,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    after_event_id: Option<i64>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct EventsResponse {
    events: Vec<JobEvent>,
}

#[derive(Debug, Deserialize)]
struct CleanupRequest {
    before: String,
}

#[derive(Debug, Serialize)]
struct CleanupResponse {
    removed_jobs: u64,
}

pub fn router(state: AppState) -> Router {
    let allowed_origins = [
        HeaderValue::from_static("tauri://localhost"),
        HeaderValue::from_static("http://tauri.localhost"),
        HeaderValue::from_static("http://127.0.0.1:1420"),
        HeaderValue::from_static("http://localhost:1420"),
        HeaderValue::from_static("http://127.0.0.1:1430"),
        HeaderValue::from_static("http://localhost:1430"),
    ];
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins))
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/version", get(version))
        .route("/api/v1/capabilities", get(capabilities))
        .route("/api/v1/jobs", post(create_job).get(list_jobs))
        .route(
            "/api/v1/inference",
            post(create_inference)
                .layer(DefaultBodyLimit::max(crate::inference::MAX_REQUEST_BYTES)),
        )
        .route("/api/v1/jobs/{id}/inference", get(get_inference))
        .route("/api/v1/jobs/cleanup", post(cleanup_jobs))
        .route("/api/v1/jobs/{id}", get(get_job))
        .route("/api/v1/jobs/{id}/cancel", post(cancel_job))
        .route("/api/v1/jobs/{id}/pause", post(pause_job))
        .route("/api/v1/jobs/{id}/resume", post(resume_job))
        .route("/api/v1/jobs/{id}/retry", post(retry_job))
        .route("/api/v1/events", get(list_events))
        .route("/api/v1/events/ws", get(events_socket))
        .route("/api/v1/hardware", get(hardware))
        .route("/api/v1/hardware/refresh", post(refresh_hardware))
        .route("/api/v1/model-catalog", get(model_catalog))
        .route("/api/v1/model-setup", get(model_setup))
        .route("/api/v1/model-downloads", post(create_model_download))
        .route("/api/v1/models", get(installed_models))
        .route("/api/v1/models/export", get(export_inventory))
        .route("/api/v1/models/default", put(set_default_model))
        .route("/api/v1/models/{id}/load", post(load_model))
        .route("/api/v1/models/{id}/unload", post(unload_model))
        .route("/api/v1/models/{id}", delete(delete_model))
        .fallback(not_found)
        .layer(middleware::from_fn(check_protocol_header))
        .layer(cors)
        .with_state(Arc::new(state))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        protocol_version: PROTOCOL_VERSION,
    })
}

async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        server_version: env!("CARGO_PKG_VERSION"),
        api_version: API_VERSION,
        protocol: protocol_range(),
    })
}

async fn capabilities(State(state): State<Arc<AppState>>) -> Json<CapabilitiesResponse> {
    Json(CapabilitiesResponse {
        protocol: protocol_range(),
        authentication: "none",
        network_mode: state.network_mode,
        features: Features {
            persistent_jobs: true,
            event_replay: true,
            websocket_events: true,
            inference_token_streaming: true,
            model_management: true,
            hardware_discovery: true,
            durable_model_downloads: true,
        },
        job_types: JobType::ALL
            .into_iter()
            .map(|job_type| JobTypeCapability {
                job_type,
                implemented: matches!(job_type, JobType::ModelDownload | JobType::Inference),
            })
            .collect(),
    })
}

async fn create_job(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<CreateJobRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(payload) = payload.map_err(|error| json_rejection(&error))?;
    if payload.job_type == JobType::Inference {
        return Err(ApiError::validation(
            "submit structured inference requests to /api/v1/inference",
        ));
    }
    validate_idempotency_key(&payload.idempotency_key)?;
    let (job, created) = state
        .store
        .create_job(CreateJob {
            idempotency_key: payload.idempotency_key,
            job_type: payload.job_type,
            input: payload.input,
            parent_job_id: payload.parent_job_id,
            retry_of_job_id: None,
        })
        .await?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(CreateJobResponse {
            job,
            duplicate: !created,
        }),
    ))
}

async fn create_inference(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<InferenceRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(payload) = payload.map_err(|error| json_rejection(&error))?;
    let (job, created) = InferenceService::new(state.store.clone(), state.models.clone())
        .submit(payload)
        .await?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(CreateJobResponse {
            job,
            duplicate: !created,
        }),
    ))
}

async fn get_inference(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        InferenceService::new(state.store.clone(), state.models.clone())
            .details(&id)
            .await?,
    ))
}

async fn get_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Job>, ApiError> {
    Ok(Json(state.store.get_job(&id).await?))
}

async fn list_jobs(
    State(state): State<Arc<AppState>>,
    query: Result<Query<ListJobsQuery>, QueryRejection>,
) -> Result<Json<JobsResponse>, ApiError> {
    let Query(query) = query.map_err(|error| query_rejection(&error))?;
    let state_filter = query
        .state
        .map(|value| JobState::from_str(&value).map_err(ApiError::validation))
        .transpose()?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    Ok(Json(JobsResponse {
        jobs: state.store.list_jobs(state_filter, limit).await?,
    }))
}

async fn cancel_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Job>, ApiError> {
    Ok(Json(state.store.cancel_job(&id).await?))
}

async fn pause_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Job>, ApiError> {
    Ok(Json(state.store.pause_job(&id).await?))
}

async fn resume_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Job>, ApiError> {
    Ok(Json(state.store.resume_job(&id).await?))
}

async fn hardware(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.models.hardware().await)
}

async fn refresh_hardware(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.models.refresh_hardware().await?))
}

async fn model_catalog(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.models.catalog().clone())
}

async fn model_setup(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.models.setup_plan().await?))
}

async fn create_model_download(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<DownloadSubmission>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(payload) = payload.map_err(|error| json_rejection(&error))?;
    let (job, created) = state.models.submit_download(payload).await?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(CreateJobResponse {
            job,
            duplicate: !created,
        }),
    ))
}

async fn installed_models(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.models.inventory().await?))
}

async fn export_inventory(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let inventory = state.models.inventory().await?;
    Ok((
        [(
            "content-disposition",
            "attachment; filename=\"tome-model-inventory.json\"",
        )],
        Json(inventory),
    ))
}

#[derive(Debug, Deserialize)]
struct DefaultModelRequest {
    model_id: String,
}

async fn set_default_model(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<DefaultModelRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(payload) = payload.map_err(|error| json_rejection(&error))?;
    Ok(Json(state.models.set_default(&payload.model_id).await?))
}

async fn load_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.models.load(&id).await?))
}

async fn unload_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.models.unload(&id).await?))
}

#[derive(Debug, Deserialize)]
struct DeleteModelRequest {
    confirmation: String,
}

async fn delete_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    payload: Result<Json<DeleteModelRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(payload) = payload.map_err(|error| json_rejection(&error))?;
    if payload.confirmation != id {
        return Err(ApiError::validation(
            "confirmation must exactly match the model id",
        ));
    }
    Ok(Json(state.models.delete(&id).await?))
}

async fn retry_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    payload: Result<Json<RetryJobRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(payload) = payload.map_err(|error| json_rejection(&error))?;
    validate_idempotency_key(&payload.idempotency_key)?;
    let original = state.store.get_job(&id).await?;
    if !original.state.is_terminal() {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "job_not_retryable",
            message: "only terminal jobs can be retried".to_owned(),
            details: json!({ "state": original.state }),
        });
    }
    let (job, created) = if original.job_type == JobType::Inference {
        let mut request: InferenceRequest = serde_json::from_value(original.input)
            .map_err(|_| ApiError::validation("stored inference request is invalid"))?;
        request.client_request_id = payload.idempotency_key;
        request.retry_of_job_id = Some(original.id);
        InferenceService::new(state.store.clone(), state.models.clone())
            .submit(request)
            .await?
    } else {
        state
            .store
            .create_job(CreateJob {
                idempotency_key: payload.idempotency_key,
                job_type: original.job_type,
                input: original.input,
                parent_job_id: original.parent_job_id,
                retry_of_job_id: Some(original.id),
            })
            .await?
    };
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(CreateJobResponse {
            job,
            duplicate: !created,
        }),
    ))
}

async fn list_events(
    State(state): State<Arc<AppState>>,
    query: Result<Query<EventsQuery>, QueryRejection>,
) -> Result<Json<EventsResponse>, ApiError> {
    let Query(query) = query.map_err(|error| query_rejection(&error))?;
    Ok(Json(EventsResponse {
        events: state
            .store
            .events_after(
                query.after_event_id.unwrap_or(0).max(0),
                query.limit.unwrap_or(200).clamp(1, 1_000),
            )
            .await?,
    }))
}

async fn cleanup_jobs(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<CleanupRequest>, JsonRejection>,
) -> Result<Json<CleanupResponse>, ApiError> {
    let Json(payload) = payload.map_err(|error| json_rejection(&error))?;
    if payload.before.trim().is_empty() {
        return Err(ApiError::validation("before is required"));
    }
    Ok(Json(CleanupResponse {
        removed_jobs: state.store.cleanup_terminal_before(&payload.before).await?,
    }))
}

async fn events_socket(
    State(state): State<Arc<AppState>>,
    query: Result<Query<HashMap<String, String>>, QueryRejection>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|error| query_rejection(&error))?;
    let after_event_id = query
        .get("after_event_id")
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| ApiError::validation("after_event_id must be an integer"))
        })
        .transpose()?
        .unwrap_or(0)
        .max(0);
    Ok(
        upgrade
            .on_upgrade(move |socket| stream_events(socket, state.store.clone(), after_event_id)),
    )
}

async fn stream_events(mut socket: axum::extract::ws::WebSocket, store: JobStore, mut cursor: i64) {
    let mut receiver = store.subscribe();
    loop {
        match store.events_after(cursor, 1_000).await {
            Ok(events) if !events.is_empty() => {
                for event in events {
                    cursor = cursor.max(event.event_id);
                    if send_event(&mut socket, &event).await.is_err() {
                        return;
                    }
                }
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(error = %error, "could not replay job events");
                return;
            }
        }

        match receiver.recv().await {
            Ok(event) if event.event_id > cursor => {
                cursor = event.event_id;
                if send_event(&mut socket, &event).await.is_err() {
                    return;
                }
            }
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn send_event(
    socket: &mut axum::extract::ws::WebSocket,
    event: &JobEvent,
) -> Result<(), axum::Error> {
    let payload = serde_json::to_string(event).expect("job events are serializable");
    socket.send(Message::Text(payload.into())).await
}

async fn not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "route_not_found",
        message: "the requested API route does not exist".to_owned(),
        details: json!({}),
    }
}

fn json_rejection(rejection: &JsonRejection) -> ApiError {
    ApiError::validation(format!("invalid JSON body: {rejection}"))
}

fn query_rejection(rejection: &QueryRejection) -> ApiError {
    ApiError::validation(format!("invalid query string: {rejection}"))
}

async fn check_protocol_header(request: Request<Body>, next: Next) -> Result<Response, ApiError> {
    let Some(value) = request.headers().get("x-tome-protocol-version") else {
        return Ok(next.run(request).await);
    };
    let compatible = value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(PROTOCOL_VERSION);
    if !compatible {
        return Err(ApiError {
            status: StatusCode::UPGRADE_REQUIRED,
            code: "protocol_incompatible",
            message: "the client protocol version is not supported".to_owned(),
            details: json!({
                "minimum": PROTOCOL_VERSION,
                "maximum": PROTOCOL_VERSION,
                "current": PROTOCOL_VERSION,
            }),
        });
    }
    Ok(next.run(request).await)
}

fn validate_idempotency_key(value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.len() > 128 {
        return Err(ApiError::validation(
            "idempotency_key must contain between 1 and 128 characters",
        ));
    }
    Ok(())
}

fn protocol_range() -> ProtocolRange {
    ProtocolRange {
        current: PROTOCOL_VERSION,
        minimum: PROTOCOL_VERSION,
        maximum: PROTOCOL_VERSION,
    }
}

fn empty_object() -> Value {
    json!({})
}
