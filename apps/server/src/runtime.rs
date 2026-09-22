use std::{net::IpAddr, path::PathBuf, time::Duration};

use serde_json::json;
use thiserror::Error;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    api::{AppState, router},
    config::NetworkMode,
    inference::InferenceService,
    model::{JobState, JobType, PROTOCOL_VERSION},
    models::ModelManager,
    network::{AddressKind, classify_address},
    store::{JobStore, StoreError},
};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub listener_addresses: Vec<IpAddr>,
    pub port: u16,
    pub database_path: PathBuf,
    pub temp_directory: PathBuf,
    pub retention_days: u32,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("server I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("server task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("server time formatting failed: {0}")]
    Time(#[from] time::error::Format),
    #[error("at least one explicit listener address is required")]
    NoListeners,
    #[error("port must be between 1 and 65535")]
    InvalidPort,
    #[error("public, wildcard, multicast, or otherwise unsafe listener rejected: {0}")]
    UnsafeListener(IpAddr),
}

pub struct ServerHandle {
    shutdown: CancellationToken,
    tasks: Vec<JoinHandle<Result<(), std::io::Error>>>,
    worker: JoinHandle<Result<(), StoreError>>,
    store: JobStore,
    models: ModelManager,
    pub listeners: Vec<std::net::SocketAddr>,
}

impl ServerHandle {
    /// Starts the service on every validated explicit address.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe listeners, persistence failures, or bind failures.
    pub async fn start(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        validate_runtime_config(&config)?;
        if let Some(parent) = config
            .database_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::create_dir_all(&config.temp_directory).await?;
        let store = JobStore::open_path(&config.database_path).await?;
        store.recover_interrupted().await?;
        let retention_cutoff = (OffsetDateTime::now_utc()
            - TimeDuration::days(i64::from(config.retention_days)))
        .format(&Rfc3339)?;
        store.cleanup_terminal_before(&retention_cutoff).await?;
        discover_orphans(&store, &config.temp_directory).await?;
        let data_root = config
            .database_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let models = ModelManager::open(store.clone(), data_root)
            .await
            .map_err(|error| RuntimeError::Store(StoreError::InvalidData(error.to_string())))?;

        let mut bound = Vec::with_capacity(config.listener_addresses.len());
        for address in &config.listener_addresses {
            bound.push(TcpListener::bind((*address, config.port)).await?);
        }
        let listeners = bound
            .iter()
            .filter_map(|listener| listener.local_addr().ok())
            .collect::<Vec<_>>();
        let shutdown = CancellationToken::new();
        let worker = tokio::spawn(run_job_dispatcher(
            store.clone(),
            models.clone(),
            InferenceService::new(store.clone(), models.clone()),
            shutdown.child_token(),
        ));
        let app = router(AppState {
            store: store.clone(),
            network_mode: network_mode(&config.listener_addresses),
            models: models.clone(),
        });
        let tasks = bound
            .into_iter()
            .map(|listener| {
                let token = shutdown.child_token();
                let service = app.clone();
                tokio::spawn(async move {
                    axum::serve(listener, service)
                        .with_graceful_shutdown(token.cancelled_owned())
                        .await
                })
            })
            .collect();
        tracing::info!(
            ?listeners,
            protocol_version = PROTOCOL_VERSION,
            "Tome server listeners ready"
        );
        Ok(Self {
            shutdown,
            tasks,
            worker,
            store,
            models,
            listeners,
        })
    }

    /// Gracefully stops all listeners and persists final recovery state.
    ///
    /// # Errors
    ///
    /// Returns an error if a listener, worker, or final persistence update fails.
    pub async fn stop(self) -> Result<(), RuntimeError> {
        self.shutdown.cancel();
        for task in self.tasks {
            task.await??;
        }
        self.worker.await??;
        self.models
            .shutdown()
            .await
            .map_err(|error| RuntimeError::Store(StoreError::InvalidData(error.to_string())))?;
        self.store.recover_interrupted().await?;
        Ok(())
    }
}

/// Validates that a runtime uses only explicit trusted local addresses.
///
/// # Errors
///
/// Returns an error for an empty set, port zero, wildcard, public, or unusable addresses.
pub fn validate_runtime_config(config: &RuntimeConfig) -> Result<(), RuntimeError> {
    if config.port == 0 {
        return Err(RuntimeError::InvalidPort);
    }
    if config.listener_addresses.is_empty() {
        return Err(RuntimeError::NoListeners);
    }
    for address in &config.listener_addresses {
        if !matches!(
            classify_address(*address),
            AddressKind::Loopback | AddressKind::Lan | AddressKind::Tailscale
        ) {
            return Err(RuntimeError::UnsafeListener(*address));
        }
    }
    Ok(())
}

fn network_mode(addresses: &[IpAddr]) -> NetworkMode {
    let kinds = addresses
        .iter()
        .map(|address| classify_address(*address))
        .collect::<std::collections::BTreeSet<_>>();
    if kinds.len() > 1 {
        NetworkMode::Multi
    } else if kinds.contains(&AddressKind::Tailscale) {
        NetworkMode::Tailscale
    } else if kinds.contains(&AddressKind::Lan) {
        NetworkMode::Lan
    } else {
        NetworkMode::Loopback
    }
}

async fn run_job_dispatcher(
    store: JobStore,
    models: ModelManager,
    inference: InferenceService,
    shutdown: CancellationToken,
) -> Result<(), StoreError> {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                while let Some(job) = store.claim_next_queued().await? {
                    if job.job_type == JobType::ModelDownload {
                        match models.execute_download(&job).await {
                            Ok(output) => {
                                if store.get_job(&job.id).await?.state == JobState::Running {
                                    store.complete_job(&job.id, output).await?;
                                }
                            }
                            Err(error) => {
                                if store.get_job(&job.id).await?.state == JobState::Running {
                                    store.fail_job(&job.id, json!({
                                        "code": "model_download_failed",
                                        "message": error.to_string(),
                                    })).await?;
                                }
                            }
                        }
                    } else if job.job_type == JobType::Inference {
                        match inference.execute(&job).await {
                            Ok(output) => {
                                if store.get_job(&job.id).await?.state == JobState::Running {
                                    store.complete_job(&job.id, output).await?;
                                    store.inference_event(
                                        &job.id,
                                        crate::model::EventType::InferenceCompleted,
                                        json!({ "schema_version": 1 }),
                                    ).await?;
                                }
                            }
                            Err(error) => {
                                if store.get_job(&job.id).await?.state == JobState::Running {
                                    let code = match &error {
                                        crate::inference::InferenceError::ContextOverflow(_) => "context_overflow",
                                        crate::inference::InferenceError::Invalid(_) => "invalid_inference_request",
                                        _ => "inference_failed",
                                    };
                                    store.mark_inference_failed(&job.id).await?;
                                    store.fail_job(&job.id, json!({
                                        "code": code,
                                        "message": error.to_string(),
                                    })).await?;
                                    store.inference_event(
                                        &job.id,
                                        crate::model::EventType::InferenceFailed,
                                        json!({ "schema_version": 1, "code": code }),
                                    ).await?;
                                }
                            }
                        }
                    } else {
                        store.fail_job(&job.id, json!({
                            "code": "job_type_unimplemented",
                            "message": format!("{} jobs are not implemented in this phase", job.job_type),
                        })).await?;
                    }
                }
            }
        }
    }
}

async fn discover_orphans(
    store: &JobStore,
    directory: &std::path::Path,
) -> Result<(), RuntimeError> {
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut count = 0_u64;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_string_lossy().starts_with("tome-job-") {
            count += 1;
        }
    }
    if count > 0 {
        store
            .warning(
                "orphan_temporary_artifacts",
                &format!(
                    "discovered {count} possible orphan temporary artifact(s); no files were removed"
                ),
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(addresses: &[&str]) -> RuntimeConfig {
        RuntimeConfig {
            listener_addresses: addresses
                .iter()
                .map(|value| value.parse().unwrap())
                .collect(),
            port: 7331,
            database_path: "jobs.sqlite3".into(),
            temp_directory: "temp".into(),
            retention_days: 30,
        }
    }

    #[test]
    fn permits_explicit_simultaneous_private_listeners() {
        validate_runtime_config(&config(&["127.0.0.1", "192.168.1.10", "100.100.20.30"])).unwrap();
    }

    #[test]
    fn rejects_wildcard_and_public_listeners() {
        assert!(matches!(
            validate_runtime_config(&config(&["0.0.0.0"])),
            Err(RuntimeError::UnsafeListener(_))
        ));
        assert!(matches!(
            validate_runtime_config(&config(&["8.8.8.8"])),
            Err(RuntimeError::UnsafeListener(_))
        ));
    }

    #[tokio::test]
    async fn starts_multiple_listeners_then_stops_and_restarts_cleanly() {
        let secondary = crate::network::enumerate_addresses()
            .unwrap()
            .into_iter()
            .find(|item| matches!(item.kind, AddressKind::Lan | AddressKind::Tailscale))
            .map_or(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), |item| {
                item.address
            });
        let (port, first, second) = (0..10)
            .find_map(|_| {
                let first = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
                let port = first.local_addr().ok()?.port();
                let second = std::net::TcpListener::bind((secondary, port)).ok()?;
                Some((port, first, second))
            })
            .expect("a port should be available on both test interfaces");
        drop((first, second));
        let temporary = tempfile::tempdir().unwrap();
        let make_config = |addresses: &[&str]| RuntimeConfig {
            listener_addresses: addresses
                .iter()
                .map(|value| value.parse().unwrap())
                .collect(),
            port,
            database_path: temporary.path().join("jobs.sqlite3"),
            temp_directory: temporary.path().join("temp"),
            retention_days: 30,
        };

        let secondary_text = secondary.to_string();
        let server = ServerHandle::start(make_config(&["127.0.0.1", secondary_text.as_str()]))
            .await
            .unwrap();
        assert_eq!(server.listeners.len(), 2);
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        tokio::net::TcpStream::connect((secondary, port))
            .await
            .unwrap();
        server.stop().await.unwrap();

        let restarted = ServerHandle::start(make_config(&["127.0.0.1"]))
            .await
            .unwrap();
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        restarted.stop().await.unwrap();
    }
}
