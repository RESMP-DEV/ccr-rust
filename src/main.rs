use anyhow::Result;
use axum::{
    routing::{get, post},
    Router,
};
use clap::Parser;
use std::net::SocketAddr;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod metrics;
mod router;
mod sse;

use config::Config;

#[derive(Parser)]
#[command(name = "ccr-rust")]
#[command(about = "Claude Code Router in Rust", long_about = None)]
struct Cli {
    /// Path to CCR config file
    #[arg(short, long, env = "CCR_CONFIG", default_value = "~/.claude-code-router/config.json")]
    config: String,

    /// Server host
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Server port
    #[arg(short, long, default_value = "3456")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ccr_rust=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    // Load config
    let config_path = shellexpand::tilde(&cli.config).to_string();
    let config = Config::from_file(&config_path)?;
    tracing::info!("Loaded config from {}", config_path);
    tracing::info!("Tier order: {:?}", config.backend_tiers());

    // Build app
    let app = Router::new()
        .route("/v1/messages", post(router::handle_messages))
        .route("/v1/latencies", get(metrics::latencies_handler))
        .route("/v1/usage", get(metrics::usage_handler))
        .route("/health", get(health))
        .route("/metrics", get(metrics::metrics_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(config);

    // Start server
    let addr = SocketAddr::from((
        cli.host.parse::<std::net::IpAddr>()?,
        cli.port,
    ));
    tracing::info!("CCR-Rust listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}
