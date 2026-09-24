use std::path::Path;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sqlx::{Row, sqlite::SqlitePoolOptions};
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
    let empty = store.clear_terminal_job_logs().await.unwrap();
    assert_eq!(empty.removed_terminal_jobs, 0);
    assert_eq!(empty.removed_job_events, 0);
    assert_eq!(empty.retained_active_jobs, 0);
    let (job, created) = store.create_job(request("migration-test")).await.unwrap();
    assert!(created);
    assert_eq!(job.state, JobState::Queued);
}

#[tokio::test]
async fn migration_three_preserves_existing_download_metadata() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("upgrade.sqlite3");
    let pool = SqlitePoolOptions::new()
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE tome_migrations (
            version INTEGER PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0001_jobs.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0002_models.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tome_migrations (version, name, applied_at) VALUES
         (1, 'jobs', '2026-01-01T00:00:00Z'),
         (2, 'models', '2026-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs
         (id, idempotency_key, job_type, state, input_json, created_at, updated_at, finished_at)
         VALUES ('download-job', 'upgrade-download', 'model_download', 'interrupted', '{}',
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO model_downloads
         (job_id, catalog_id, part_path, final_path, bytes_downloaded, expected_bytes,
          created_at, updated_at)
         VALUES ('download-job', 'fixture', 'partial.part', 'model.gguf', 25, 100,
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let store = store_at(&database_path).await;
    assert_eq!(
        store.get_job("download-job").await.unwrap().state,
        JobState::Interrupted
    );
    drop(store);
    let reopened = SqlitePoolOptions::new()
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let row =
        sqlx::query("SELECT job_id, catalog_id, part_path, bytes_downloaded FROM model_downloads")
            .fetch_one(&reopened)
            .await
            .unwrap();
    assert_eq!(row.try_get::<String, _>("job_id").unwrap(), "download-job");
    assert_eq!(row.try_get::<String, _>("catalog_id").unwrap(), "fixture");
    assert_eq!(
        row.try_get::<String, _>("part_path").unwrap(),
        "partial.part"
    );
    assert_eq!(row.try_get::<i64, _>("bytes_downloaded").unwrap(), 25);
    let migration_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tome_migrations WHERE version = 3")
            .fetch_one(&reopened)
            .await
            .unwrap();
    assert_eq!(migration_count, 1);
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
async fn clear_job_logs_removes_terminal_history_and_retains_active_jobs() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let (terminal, _) = store.create_job(request("clear-terminal")).await.unwrap();
    assert_eq!(
        store.claim_next_queued().await.unwrap().unwrap().id,
        terminal.id
    );
    store
        .complete_job(&terminal.id, json!({ "ok": true }))
        .await
        .unwrap();
    let (interrupted, _) = store
        .create_job(download_request("clear-interrupted"))
        .await
        .unwrap();
    store.pause_job(&interrupted.id).await.unwrap();
    let (running, _) = store.create_job(request("clear-running")).await.unwrap();
    assert_eq!(
        store.claim_next_queued().await.unwrap().unwrap().id,
        running.id
    );
    let (queued, _) = store.create_job(request("clear-queued")).await.unwrap();

    let cleared = store.clear_terminal_job_logs().await.unwrap();

    assert_eq!(cleared.removed_terminal_jobs, 2);
    assert_eq!(cleared.removed_job_events, 7);
    assert_eq!(cleared.retained_active_jobs, 2);
    assert_eq!(cleared.retained_active_job_events, 5);
    assert_eq!(cleared.retained_linked_terminal_jobs, 0);
    assert!(matches!(
        store.get_job(&terminal.id).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(
        store.get_job(&running.id).await.unwrap().state,
        JobState::Running
    );
    assert_eq!(
        store.get_job(&queued.id).await.unwrap().state,
        JobState::Queued
    );
    assert!(
        store
            .events_after(0, 100)
            .await
            .unwrap()
            .iter()
            .all(|event| !matches!(
                event.job_id.as_deref(),
                Some(id) if id == terminal.id || id == interrupted.id
            ))
    );

    let repeated = store.clear_terminal_job_logs().await.unwrap();
    assert_eq!(repeated.removed_terminal_jobs, 0);
    assert_eq!(repeated.removed_job_events, 0);
    assert_eq!(repeated.retained_active_jobs, 2);
}

#[tokio::test]
async fn clear_job_logs_preserves_terminal_links_required_by_active_jobs() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let (parent, _) = store.create_job(request("linked-parent")).await.unwrap();
    store.claim_next_queued().await.unwrap();
    store
        .complete_job(&parent.id, json!({ "ok": true }))
        .await
        .unwrap();
    let (child, _) = store
        .create_job(CreateJob {
            idempotency_key: "linked-child".to_owned(),
            job_type: JobType::Inference,
            input: json!({}),
            parent_job_id: Some(parent.id.clone()),
            retry_of_job_id: Some(parent.id.clone()),
        })
        .await
        .unwrap();

    let cleared = store.clear_terminal_job_logs().await.unwrap();

    assert_eq!(cleared.removed_terminal_jobs, 0);
    assert_eq!(cleared.retained_active_jobs, 1);
    assert_eq!(cleared.retained_linked_terminal_jobs, 1);
    assert_eq!(
        store.get_job(&parent.id).await.unwrap().state,
        JobState::Completed
    );
    let retained_child = store.get_job(&child.id).await.unwrap();
    assert_eq!(retained_child.state, JobState::Queued);
    assert_eq!(
        retained_child.parent_job_id.as_deref(),
        Some(parent.id.as_str())
    );
    assert_eq!(
        retained_child.retry_of_job_id.as_deref(),
        Some(parent.id.as_str())
    );
}

#[tokio::test]
async fn clear_job_logs_detaches_download_metadata_and_preserves_partial_file() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("jobs.sqlite3");
    let partial_path = directory.path().join("model-download.part");
    std::fs::write(&partial_path, b"partial model bytes").unwrap();
    let store = store_at(&database_path).await;
    let (download, _) = store
        .create_job(download_request("clear-download"))
        .await
        .unwrap();
    store.claim_next_queued().await.unwrap();
    let pool = SqlitePoolOptions::new()
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO model_downloads
         (job_id, catalog_id, part_path, final_path, bytes_downloaded, expected_bytes,
          created_at, updated_at)
         VALUES (?, 'fixture', ?, ?, 19, 100, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .bind(&download.id)
    .bind(partial_path.display().to_string())
    .bind(directory.path().join("model.gguf").display().to_string())
    .execute(&pool)
    .await
    .unwrap();
    store
        .fail_job(&download.id, json!({ "message": "network stopped" }))
        .await
        .unwrap();

    let cleared = store.clear_terminal_job_logs().await.unwrap();

    assert_eq!(cleared.removed_terminal_jobs, 1);
    let row = sqlx::query("SELECT job_id, part_path, bytes_downloaded FROM model_downloads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.try_get::<Option<String>, _>("job_id").unwrap(), None);
    assert_eq!(
        row.try_get::<String, _>("part_path").unwrap(),
        partial_path.display().to_string()
    );
    assert_eq!(row.try_get::<i64, _>("bytes_downloaded").unwrap(), 19);
    assert_eq!(
        std::fs::read(&partial_path).unwrap(),
        b"partial model bytes"
    );
}

#[tokio::test]
async fn clear_job_logs_api_returns_removed_and_retained_counts() {
    let directory = TempDir::new().unwrap();
    let store = store_at(&directory.path().join("jobs.sqlite3")).await;
    let (terminal, _) = store
        .create_job(request("api-clear-terminal"))
        .await
        .unwrap();
    store.claim_next_queued().await.unwrap();
    store.cancel_job(&terminal.id).await.unwrap();
    store.create_job(request("api-clear-queued")).await.unwrap();
    let models = ModelManager::open(store.clone(), directory.path())
        .await
        .unwrap();
    let app = router(AppState {
        store,
        network_mode: NetworkMode::Loopback,
        models,
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/jobs/clear")
                .header("X-Tome-Protocol-Version", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["removed_terminal_jobs"], 1);
    assert_eq!(body["removed_job_events"], 4);
    assert_eq!(body["retained_active_jobs"], 1);
    assert_eq!(body["retained_active_job_events"], 2);
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
