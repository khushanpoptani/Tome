use std::path::Path;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tome_server::{
    api::{AppState, router},
    config::NetworkMode,
    model::{EventType, JobEvent, JobState, JobType},
    models::ModelManager,
    store::{CreateJob, JobStore, StoreError},
};
use tower::ServiceExt;

async fn store_at(path: &Path) -> JobStore {
    JobStore::open_path(path).await.unwrap()
}

fn request(key: &str) -> CreateJob {
    CreateJob {
        idempotency_key: key.to_owned(),
        job_type: JobType::Inference,
        input: json!({ "prompt": "not executed in Phase 1" }),
        parent_job_id: None,
        retry_of_job_id: None,
    }
}

fn download_request(key: &str) -> CreateJob {
    CreateJob {
        idempotency_key: key.to_owned(),
        job_type: JobType::ModelDownload,
        input: json!({ "catalog_id": "fixture" }),
        parent_job_id: None,
        retry_of_job_id: None,
    }
}

#[tokio::test]
async fn migrations_create_a_usable_database() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let (job, created) = store.create_job(request("migration-test")).await.unwrap();
    assert!(created);
    assert_eq!(job.state, JobState::Queued);
}

#[tokio::test]
async fn duplicate_idempotent_submission_returns_original_job() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let (first, created) = store.create_job(request("same-request")).await.unwrap();
    assert!(created);
    let (second, created) = store.create_job(request("same-request")).await.unwrap();
    assert!(!created);
    assert_eq!(first.id, second.id);

    let mut changed = request("same-request");
    changed.input = json!({ "prompt": "different" });
    assert!(matches!(
        store.create_job(changed).await,
        Err(StoreError::IdempotencyConflict)
    ));
}

#[tokio::test]
async fn queued_job_can_be_cancelled_and_is_not_claimed() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let (job, _) = store.create_job(request("cancel-me")).await.unwrap();
    let cancelled = store.cancel_job(&job.id).await.unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert!(store.claim_next_queued().await.unwrap().is_none());
    assert_eq!(
        store
            .events_after(0, 20)
            .await
            .unwrap()
            .last()
            .unwrap()
            .event_type,
        EventType::JobCancelled
    );
}

#[tokio::test]
async fn restart_preserves_progress_and_interrupts_running_only_once() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("jobs.sqlite3");
    let store = store_at(&path).await;
    let (running, _) = store.create_job(request("running")).await.unwrap();
    let (queued, _) = store.create_job(request("queued")).await.unwrap();
    let claimed = store.claim_next_queued().await.unwrap().unwrap();
    assert_eq!(claimed.id, running.id);
    store.update_progress(&running.id, 0.4).await.unwrap();
    drop(store);

    let reopened = store_at(&path).await;
    assert_eq!(reopened.recover_interrupted().await.unwrap(), 1);
    assert_eq!(reopened.recover_interrupted().await.unwrap(), 0);
    let interrupted = reopened.get_job(&running.id).await.unwrap();
    assert_eq!(interrupted.state, JobState::Interrupted);
    assert!((interrupted.progress - 0.4).abs() < f64::EPSILON);
    assert_eq!(
        reopened.get_job(&queued.id).await.unwrap().state,
        JobState::Queued
    );

    let interruption_events = reopened
        .events_after(0, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == EventType::JobInterrupted)
        .count();
    assert_eq!(interruption_events, 1);
}

#[tokio::test]
async fn model_download_pause_resume_and_restart_requeue_are_durable() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("jobs.sqlite3");
    let store = store_at(&path).await;
    let (paused, _) = store
        .create_job(download_request("paused-download"))
        .await
        .unwrap();
    assert_eq!(
        store.pause_job(&paused.id).await.unwrap().state,
        JobState::Interrupted
    );
    assert_eq!(
        store.resume_job(&paused.id).await.unwrap().state,
        JobState::Queued
    );
    let claimed = store.claim_next_queued().await.unwrap().unwrap();
    assert_eq!(claimed.id, paused.id);
    store.update_progress(&claimed.id, 0.4).await.unwrap();
    assert_eq!(store.recover_interrupted().await.unwrap(), 1);
    assert_eq!(store.resume_interrupted_downloads().await.unwrap(), 1);
    let resumed = store.get_job(&claimed.id).await.unwrap();
    assert_eq!(resumed.state, JobState::Queued);
    assert!((resumed.progress - 0.4).abs() < f64::EPSILON);
}

#[tokio::test]
async fn terminal_jobs_survive_reopen_and_event_replay_uses_cursor() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("jobs.sqlite3");
    let store = store_at(&path).await;
    let (job, _) = store.create_job(request("complete")).await.unwrap();
    store.claim_next_queued().await.unwrap();
    store.update_progress(&job.id, 0.75).await.unwrap();
    store
        .complete_job(&job.id, json!({ "ok": true }))
        .await
        .unwrap();
    let events = store.events_after(0, 100).await.unwrap();
    let cursor = events[1].event_id;
    drop(store);

    let reopened = store_at(&path).await;
    assert_eq!(
        reopened.get_job(&job.id).await.unwrap().state,
        JobState::Completed
    );
    let replay = reopened.events_after(cursor, 100).await.unwrap();
    assert!(replay.iter().all(|event| event.event_id > cursor));
    assert_eq!(replay.last().unwrap().event_type, EventType::JobCompleted);
}

#[tokio::test]
async fn api_uses_structured_errors_and_reports_capabilities() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let models = ModelManager::open(store.clone(), directory.path())
        .await
        .unwrap();
    let app = router(AppState {
        store,
        network_mode: NetworkMode::Lan,
        models,
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/capabilities")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["protocol"]["current"], 1);
    assert_eq!(body["network_mode"], "lan");
    assert_eq!(body["features"]["inference_token_streaming"], false);
    let job_types = body["job_types"].as_array().unwrap();
    assert!(job_types.iter().any(|capability| {
        capability["job_type"] == "model_download" && capability["implemented"] == true
    }));
    assert!(job_types.iter().any(|capability| {
        capability["job_type"] == "inference" && capability["implemented"] == false
    }));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/capabilities")
                .header("X-Tome-Protocol-Version", "999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "protocol_incompatible");
    assert_eq!(body["error"]["details"]["minimum"], 1);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/jobs/missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "job_not_found");
    assert!(body["error"]["request_id"].is_string());
}

#[tokio::test]
async fn model_management_api_exposes_hardware_catalog_setup_and_inventory() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let models = ModelManager::open(store.clone(), directory.path())
        .await
        .unwrap();
    let app = router(AppState {
        store,
        network_mode: NetworkMode::Loopback,
        models,
    });

    for (path, expected_key) in [
        ("/api/v1/hardware", "schema_version"),
        ("/api/v1/model-catalog", "catalog_version"),
        ("/api/v1/model-setup", "profiles"),
        ("/api/v1/models", "models"),
        ("/api/v1/models/export", "models"),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(body.get(expected_key).is_some(), "{path}");
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/model-downloads")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "catalog_id": "qwen2.5-0.5b-instruct-q4-k-m",
                        "idempotency_key": "license-required",
                        "license_accepted": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "invalid_model_request");

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/model-search?q=")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn websocket_replays_then_delivers_live_events() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let models = ModelManager::open(store.clone(), directory.path())
        .await
        .unwrap();
    let (existing, _) = store.create_job(request("before-connect")).await.unwrap();
    let existing_events = store.events_after(0, 10).await.unwrap();
    let first_cursor = existing_events[0].event_id;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(AppState {
        store: store.clone(),
        network_mode: NetworkMode::Loopback,
        models,
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (mut socket, _) = connect_async(format!(
        "ws://{address}/api/v1/events/ws?after_event_id={first_cursor}"
    ))
    .await
    .unwrap();

    let replayed = next_event(&mut socket).await;
    assert_eq!(replayed.job_id.as_deref(), Some(existing.id.as_str()));
    assert!(replayed.event_id > first_cursor);

    let (live_job, _) = store.create_job(request("after-connect")).await.unwrap();
    let live = loop {
        let event = next_event(&mut socket).await;
        if event.job_id.as_deref() == Some(live_job.id.as_str()) {
            break event;
        }
    };
    assert_eq!(live.event_type, EventType::JobCreated);
    server.abort();
}

async fn next_event<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> JobEvent
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = message else {
        panic!("expected a text event");
    };
    serde_json::from_str(&text).unwrap()
}
