use std::{path::Path, time::Duration};

use serde_json::json;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio_util::sync::CancellationToken;
use tome_server::{
    api::{AppState, router},
    config::Config,
    store::JobStore,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();
    let config = Config::from_env()?;
    if let Some(parent) = config
        .database_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::create_dir_all(&config.temp_directory).await?;

    let store = JobStore::open_path(&config.database_path).await?;
    let interrupted = store.recover_interrupted().await?;
    if interrupted > 0 {
        tracing::warn!(interrupted, "recovered jobs that were running at shutdown");
    }
    let retention_cutoff = (OffsetDateTime::now_utc()
        - TimeDuration::days(i64::from(config.retention_days)))
    .format(&Rfc3339)?;
    let removed = store.cleanup_terminal_before(&retention_cutoff).await?;
    if removed > 0 {
        tracing::info!(
            removed,
            retention_days = config.retention_days,
            "cleaned up expired jobs"
        );
    }
    discover_orphan_temporary_artifacts(&store, &config.temp_directory).await?;

    let listener = tokio::net::TcpListener::bind((config.bind_address, config.port)).await?;
    tracing::info!(
        bind_address = %config.bind_address,
        port = config.port,
        network_mode = ?config.network_mode,
        protocol_version = tome_server::model::PROTOCOL_VERSION,
        authentication = "none",
        "Tome server ready"
    );

    let shutdown = CancellationToken::new();
    let worker = tokio::spawn(run_job_dispatcher(store.clone(), shutdown.child_token()));
    let app = router(AppState {
        store: store.clone(),
        network_mode: config.network_mode,
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown.clone()))
        .await?;
    shutdown.cancel();
    worker.await??;
    let interrupted = store.recover_interrupted().await?;
    tracing::info!(interrupted, "Tome server stopped cleanly");
    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .init();
}

async fn shutdown_signal(shutdown: CancellationToken) {
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler can be installed")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(error) = result {
                tracing::error!(%error, "failed to listen for Ctrl+C");
            }
        }
        () = terminate => {}
    }
    tracing::info!("shutdown requested");
    shutdown.cancel();
}

async fn run_job_dispatcher(
    store: JobStore,
    shutdown: CancellationToken,
) -> Result<(), tome_server::store::StoreError> {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                while let Some(job) = store.claim_next_queued().await? {
                    store.fail_job(
                        &job.id,
                        json!({
                            "code": "job_type_unimplemented",
                            "message": format!("{} jobs are registered but not implemented in Phase 1", job.job_type),
                        }),
                    ).await?;
                }
            }
        }
    }
}

async fn discover_orphan_temporary_artifacts(
    store: &JobStore,
    directory: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut count = 0_u64;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_string_lossy().starts_with("tome-job-") {
            count += 1;
        }
    }
    if count > 0 {
        let message = format!(
            "discovered {count} possible orphan temporary artifact(s); Phase 1 never deletes them automatically"
        );
        store
            .warning("orphan_temporary_artifacts", &message)
            .await?;
        tracing::warn!(count, %message);
    }
    Ok(())
}
