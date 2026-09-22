use tome_server::{
    config::Config,
    runtime::{RuntimeConfig, ServerHandle},
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .init();
    let config = Config::from_env()?;
    let server = ServerHandle::start(RuntimeConfig {
        listener_addresses: vec![config.bind_address],
        port: config.port,
        database_path: config.database_path,
        temp_directory: config.temp_directory,
        retention_days: config.retention_days,
    })
    .await?;
    tracing::info!(listeners = ?server.listeners, "developer server ready");
    shutdown_signal().await;
    server.stop().await?;
    Ok(())
}

async fn shutdown_signal() {
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
        _ = tokio::signal::ctrl_c() => {},
        () = terminate => {},
    }
}
