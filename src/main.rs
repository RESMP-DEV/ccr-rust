use anyhow::{anyhow, Result};
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
mod frontend {
    // frontend module re-exports are used by downstream consumers
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
use ccr_rust::debug_capture::DebugCapture;
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
    /// Clear persisted CCR stats in Redis for a prefix.
    ClearStats {
        /// Redis URL override (defaults to Persistence.redis_url or CCR_REDIS_URL)
        #[arg(long, env = "CCR_REDIS_URL")]
        redis_url: Option<String>,

        /// Redis key prefix override (defaults to Persistence.redis_prefix)
        #[arg(long)]
        redis_prefix: Option<String>,
    },
    /// List and analyze debug captures.
    Captures {
        /// Filter by provider name (e.g., "minimax")
        #[arg(short, long)]
        provider: Option<String>,

        /// Maximum number of captures to list
        #[arg(short, long, default_value = "20")]
        limit: usize,

        /// Show statistics instead of individual captures
        #[arg(long)]
        stats: bool,

        /// Output capture directory (override config)
        #[arg(long)]
        output_dir: Option<String>,

        /// Show full response body (by default truncated)
        #[arg(long)]
        full: bool,
    },
}

fn show_version() {
    println!("ccr-rust {}", env!("CARGO_PKG_VERSION"));
    #[cfg(debug_assertions)]
    println!("Build: debug");
    #[cfg(not(debug_assertions))]
    println!("Build: release");
    println!("Features: streaming, ewma-routing, transformers, rate-limiting");
}

fn resolve_redis_target(
    config_path: &str,
    redis_url_override: Option<String>,
    redis_prefix_override: Option<String>,
) -> anyhow::Result<(String, String)> {
    let config = Config::from_file(config_path)?;
    let persistence = config.persistence();

    let redis_url = redis_url_override
        .or_else(|| persistence.redis_url.clone())
        .or_else(|| std::env::var("CCR_REDIS_URL").ok())
        .ok_or_else(|| {
            anyhow!(
                "No Redis URL configured. Set Persistence.redis_url, CCR_REDIS_URL, or pass --redis-url."
            )
        })?;

    let redis_prefix = redis_prefix_override.unwrap_or_else(|| persistence.redis_prefix.clone());

    Ok((redis_url, redis_prefix))
}

fn clear_stats(
    config_path: &str,
    redis_url_override: Option<String>,
    redis_prefix_override: Option<String>,
) -> anyhow::Result<()> {
    let (redis_url, redis_prefix) =
        resolve_redis_target(config_path, redis_url_override, redis_prefix_override)?;
    let deleted = metrics::clear_redis_persistence(&redis_url, &redis_prefix)?;
    println!(
        "Cleared {} Redis key(s) for prefix '{}'",
        deleted, redis_prefix
    );
    Ok(())
}

fn list_captures(
    config_path: &str,
    provider: Option<String>,
    limit: usize,
    stats: bool,
    output_dir_override: Option<String>,
    full: bool,
) -> anyhow::Result<()> {
    let config = Config::from_file(config_path)?;
    let mut debug_config = config.debug_capture().clone();

    // Override output directory if specified
    if let Some(dir) = output_dir_override {
        debug_config.output_dir = dir;
    }

    // Create a capture manager to read existing captures
    debug_config.enabled = true; // Enable to read directory
    let capture = DebugCapture::new(debug_config)?;

    if stats {
        // Show statistics
        let stats = capture.get_stats()?;
        println!("Debug Capture Statistics");
        println!("========================");
        println!("Total captures: {}", stats.total_captures);
        println!("Successful: {}", stats.success_count);
        println!("Errors: {}", stats.error_count);
        println!("Avg latency: {}ms", stats.avg_latency_ms);
        println!("\nBy provider:");
        for (prov, count) in &stats.by_provider {
            println!("  {}: {}", prov, count);
        }
    } else {
        // List individual captures
        let captures = capture.list_captures(provider.as_deref(), limit)?;

        if captures.is_empty() {
            println!(
                "No captures found{}",
                provider
                    .as_ref()
                    .map(|p| format!(" for provider '{}'", p))
                    .unwrap_or_default()
            );
            return Ok(());
        }

        println!(
            "Recent Captures{} ({} found)",
            provider
                .as_ref()
                .map(|p| format!(" for '{}'", p))
                .unwrap_or_default(),
            captures.len()
        );
        println!("{}", "=".repeat(60));

        for cap in captures {
            println!(
                "\n[{}] {} @ {}",
                cap.request_id, cap.provider, cap.timestamp
            );
            println!("  Tier: {}, Model: {}", cap.tier_name, cap.model);
            println!(
                "  Status: {} ({}) - {}ms",
                cap.response_status,
                if cap.success { "OK" } else { "FAILED" },
                cap.latency_ms
            );
            println!("  URL: {}", cap.url);

            if let Some(ref error) = cap.error {
                println!("  Error: {}", error);
            }

            if full {
                println!("  Request:");
                if let Ok(pretty) = serde_json::to_string_pretty(&cap.request_body) {
                    for line in pretty.lines() {
                        println!("    {}", line);
                    }
                }
                println!(
                    "  Response ({} bytes{}):",
                    cap.response_body.len(),
                    if cap.response_truncated {
                        ", truncated"
                    } else {
                        ""
                    }
                );
                // Try to pretty-print if it's JSON
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&cap.response_body) {
                    if let Ok(pretty) = serde_json::to_string_pretty(&json) {
                        for line in pretty.lines() {
                            println!("    {}", line);
                        }
                    }
                } else {
                    // Print as-is
                    for line in cap.response_body.lines().take(20) {
                        println!("    {}", line);
                    }
                }
            } else {
                // Show truncated response preview
                let preview: String = cap.response_body.chars().take(200).collect();
                if !preview.is_empty() {
                    println!(
                        "  Response preview: {}{}",
                        preview.replace('\n', " "),
                        if cap.response_body.len() > 200 {
                            "..."
                        } else {
                            ""
                        }
                    );
                }
            }
        }
    }

    Ok(())
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
    metrics::init_persistence(config.persistence(), &ewma_tracker)?;
    let transformer_registry = std::sync::Arc::new(TransformerRegistry::new());
    let ratelimit_tracker = std::sync::Arc::new(RateLimitTracker::new());

    // Initialize debug capture if enabled
    let debug_capture = if config.debug_capture().enabled {
        match DebugCapture::new(config.debug_capture().clone()) {
            Ok(capture) => Some(Arc::new(capture)),
            Err(e) => {
                tracing::warn!("Failed to initialize debug capture: {}", e);
                None
            }
        }
    } else {
        None
    };

    let state = AppState {
        config,
        ewma_tracker,
        transformer_registry,
        active_streams: Arc::new(AtomicUsize::new(0)),
        ratelimit_tracker,
        shutdown_timeout,
        debug_capture,
    };

    let app = Router::new()
        .route("/v1/messages", post(router::handle_messages))
        .route(
            "/v1/chat/completions",
            post(router::handle_chat_completions),
        )
        .route("/v1/responses", post(router::handle_responses))
        .route("/v1/models", get(router::list_models))
        .route(
            "/preset/{name}/v1/messages",
            post(router::handle_preset_messages),
        )
        .route("/v1/presets", get(router::list_presets))
        .route("/v1/latencies", get(latencies_handler))
        .route("/v1/usage", get(metrics::usage_handler))
        .route("/v1/token-drift", get(metrics::token_drift_handler))
        .route(
            "/v1/frontend-metrics",
            get(metrics::frontend_metrics_handler),
        )
        .route("/health", get(health))
        .route("/metrics", get(metrics::metrics_handler))
        // Debug capture API
        .route("/debug/capture/status", get(debug_capture_status))
        .route("/debug/capture/list", get(debug_capture_list))
        .route("/debug/capture/stats", get(debug_capture_stats))
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
        Some(Commands::ClearStats {
            redis_url,
            redis_prefix,
        }) => {
            clear_stats(&config_path, redis_url, redis_prefix)?;
        }
        Some(Commands::Captures {
            provider,
            limit,
            stats,
            output_dir,
            full,
        }) => {
            list_captures(&config_path, provider, limit, stats, output_dir, full)?;
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

// Debug capture API handlers
async fn debug_capture_status(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    match &state.debug_capture {
        Some(capture) => {
            let stats = capture.get_stats().unwrap_or_default();
            axum::Json(serde_json::json!({
                "enabled": true,
                "output_dir": state.config.debug_capture().output_dir,
                "providers": state.config.debug_capture().providers,
                "stats": stats
            }))
        }
        None => axum::Json(serde_json::json!({
            "enabled": false,
            "message": "Debug capture not configured. Add DebugCapture to config.json"
        })),
    }
}

async fn debug_capture_list(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    let provider = params.get("provider").map(|s| s.as_str());
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    match &state.debug_capture {
        Some(capture) => match capture.list_captures(provider, limit) {
            Ok(captures) => axum::Json(serde_json::json!({
                "captures": captures,
                "count": captures.len()
            })),
            Err(e) => axum::Json(serde_json::json!({
                "error": format!("Failed to list captures: {}", e)
            })),
        },
        None => axum::Json(serde_json::json!({
            "error": "Debug capture not enabled"
        })),
    }
}

async fn debug_capture_stats(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    match &state.debug_capture {
        Some(capture) => match capture.get_stats() {
            Ok(stats) => axum::Json(serde_json::json!(stats)),
            Err(e) => axum::Json(serde_json::json!({
                "error": format!("Failed to get stats: {}", e)
            })),
        },
        None => axum::Json(serde_json::json!({
            "error": "Debug capture not enabled"
        })),
    }
}
