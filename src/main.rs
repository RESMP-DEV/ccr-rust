use anyhow::Result;
use axum::{
    extract::State,
    routing::{get, post},
    Router,
};
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::signal::ctrl_c;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config {
    pub use ccr_rust::config::*;
}
mod dashboard {
    pub use ccr_rust::dashboard::*;
}
mod metrics {
    pub use ccr_rust::metrics::*;
}
mod ratelimit {
    pub use ccr_rust::ratelimit::*;
}
mod router {
    pub use ccr_rust::router::*;
}
mod routing {
    pub use ccr_rust::routing::*;
}
mod transformer {
    pub use ccr_rust::transformer::*;
}

use crate::config::Config;
use ratelimit::RateLimitTracker;
use router::AppState;
use routing::EwmaTracker;
use transformer::TransformerRegistry;

#[derive(Parser)]
#[command(name = "ccr-rust")]
#[command(about = "Claude Code Router in Rust")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file (global option)
    #[arg(short, long, env = "CCR_CONFIG", global = true)]
    config: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the CCR server
    Start {
        /// Server host
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Server port
        #[arg(short, long, default_value = "3456")]
        port: u16,

        /// Maximum concurrent streams (0 = unlimited)
        #[arg(long, env = "CCR_MAX_STREAMS", default_value = "512")]
        max_streams: usize,

        /// Graceful shutdown timeout in seconds
        #[arg(long, default_value = "30")]
        shutdown_timeout: u64,
    },
    /// Check if server is running
    Status {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(short, long, default_value = "3456")]
        port: u16,
    },
    /// Validate config file syntax and providers
    Validate,
    /// Launch interactive TUI dashboard
    Dashboard {
        /// Tracker host
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Tracker port
        #[arg(short, long, default_value = "3456")]
        port: u16,
    },
    /// Show version and build info
    Version,
}

fn show_version() {
    println!("ccr-rust {}", env!("CARGO_PKG_VERSION"));
    #[cfg(debug_assertions)]
    println!("Build: debug");
    #[cfg(not(debug_assertions))]
    println!("Build: release");
    println!("Features: streaming, ewma-routing, transformers, rate-limiting");
}

async fn run_server(
    config_path: &str,
    host: String,
    port: u16,
    max_streams: usize,
    shutdown_timeout: u64,
) -> anyhow::Result<()> {
    let config = Config::from_file(config_path)?;
    tracing::info!("Loaded config from {}", config_path);
    tracing::info!("Tier order: {:?}", config.backend_tiers());
    tracing::info!("Max concurrent streams: {}", max_streams);
    tracing::info!("Shutdown timeout: {}s", shutdown_timeout);

    let ewma_tracker = std::sync::Arc::new(EwmaTracker::new());
    let transformer_registry = std::sync::Arc::new(TransformerRegistry::new());
    let ratelimit_tracker = std::sync::Arc::new(RateLimitTracker::new());
    let state = AppState {
        config,
        ewma_tracker,
        transformer_registry,
        active_streams: Arc::new(AtomicUsize::new(0)),
        ratelimit_tracker,
        shutdown_timeout,
    };

    let app = Router::new()
        .route("/v1/messages", post(router::handle_messages))
        .route(
            "/v1/chat/completions",
            post(router::handle_chat_completions),
        )
        .route(
            "/preset/{name}/v1/messages",
            post(router::handle_preset_messages),
        )
        .route("/v1/presets", get(router::list_presets))
        .route("/v1/latencies", get(latencies_handler))
        .route("/v1/usage", get(metrics::usage_handler))
        .route("/v1/token-drift", get(metrics::token_drift_handler))
        .route("/health", get(health))
        .route("/metrics", get(metrics::metrics_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from((host.parse::<std::net::IpAddr>()?, port));
    tracing::info!("CCR-Rust listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_timeout))
        .await?;

    Ok(())
}

fn validate_config(config_path: &str) -> anyhow::Result<()> {
    println!("Validating: {}", config_path);

    let config = Config::from_file(config_path)?;

    let providers = config.providers();
    println!("✓ {} provider(s)", providers.len());
    for p in providers {
        println!("  - {}: {} model(s)", p.name, p.models.len());
    }

    let tiers = config.backend_tiers();
    println!("✓ {} tier(s)", tiers.len());
    for tier in &tiers {
        println!("  - {}", tier);
    }

    println!("\n✓ Configuration valid");
    Ok(())
}

async fn check_status(host: &str, port: u16) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://{}:{}/health", host, port);

    match client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            println!("✓ CCR-Rust running on {}:{}", host, port);

            // Fetch latencies
            let lat_url = format!("http://{}:{}/v1/latencies", host, port);
            if let Ok(lat_resp) = client.get(&lat_url).send().await {
                if let Ok(json) = lat_resp.json::<serde_json::Value>().await {
                    println!("Latencies: {}", serde_json::to_string_pretty(&json)?);
                }
            }
            Ok(())
        }
        Ok(resp) => {
            eprintln!("✗ Server returned: {}", resp.status());
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("✗ Not running: {}", e);
            std::process::exit(1);
        }
    }
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
    let config_path = cli
        .config
        .map(|p| shellexpand::tilde(&p).to_string())
        .unwrap_or_else(|| shellexpand::tilde("~/.claude-code-router/config.json").to_string());

    match cli.command {
        Some(Commands::Start {
            host,
            port,
            max_streams,
            shutdown_timeout,
        }) => {
            run_server(&config_path, host, port, max_streams, shutdown_timeout).await?;
        }
        None => {
            // Default: start server with defaults
            run_server(&config_path, "127.0.0.1".into(), 3456, 512, 30).await?;
        }
        Some(Commands::Status { host, port }) => {
            check_status(&host, port).await?;
        }
        Some(Commands::Validate) => {
            validate_config(&config_path)?;
        }
        Some(Commands::Dashboard { host, port }) => {
            dashboard::run_dashboard(host, port)?;
        }
        Some(Commands::Version) => {
            show_version();
        }
    }
    Ok(())
}

async fn shutdown_signal(timeout: u64) {
    let ctrl_c = async { ctrl_c().await.expect("failed to listen for ctrl+c") };
    #[cfg(unix)]
    let terminate = async {
        signal(SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received SIGINT"),
        _ = terminate => tracing::info!("Received SIGTERM"),
    }
    tracing::info!(
        "Received shutdown signal, draining connections (timeout {}s)...",
        timeout
    );
}

async fn latencies_handler(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    axum::Json(metrics::get_latency_entries(&state.ewma_tracker))
}

async fn health() -> &'static str {
    "ok"
}
