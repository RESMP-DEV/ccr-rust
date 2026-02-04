use anyhow::Result;
use axum::{
    extract::State,
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
mod routing;
mod sse;
mod transformer;

use config::Config;
use router::AppState;
use routing::EwmaTracker;
use transformer::TransformerRegistry;

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

    /// Maximum concurrent streams (0 = unlimited)
    #[arg(long, env = "CCR_MAX_STREAMS", default_value = "512")]
    max_streams: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ccr_rust=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    let config_path = shellexpand::tilde(&cli.config).to_string();
    let config = Config::from_file(&config_path)?;
    tracing::info!("Loaded config from {}", config_path);
    tracing::info!("Tier order: {:?}", config.backend_tiers());
    tracing::info!("Max concurrent streams: {}", cli.max_streams);

    let ewma_tracker = std::sync::Arc::new(EwmaTracker::new());
    let transformer_registry = std::sync::Arc::new(TransformerRegistry::new());
    let state = AppState {
        config,
        ewma_tracker,
        transformer_registry,
    };

    let app = Router::new()
        .route("/v1/messages", post(router::handle_messages))
        .route("/v1/latencies", get(latencies_handler))
        .route("/v1/usage", get(metrics::usage_handler))
        .route("/v1/token-drift", get(metrics::token_drift_handler))
        .route("/health", get(health))
        .route("/metrics", get(metrics::metrics_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from((
        cli.host.parse::<std::net::IpAddr>()?,
        cli.port,
    ));
    tracing::info!("CCR-Rust listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn latencies_handler(
    State(state): State<AppState>,
) -> impl axum::response::IntoResponse {
    axum::Json(metrics::get_latency_entries(&state.ewma_tracker))
}

async fn health() -> &'static str {
    "ok"
}
