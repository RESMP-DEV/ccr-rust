use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
    Terminal,
};
use std::env;
use std::io;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::metrics::{FrontendMetrics, TierLatency, TierTokenDrift, TierUsage, UsageSummary};

/// Aggregated dashboard data fetched from the CCR-Rust API.
#[derive(Debug, Clone)]
pub struct DashboardData {
    /// Aggregated usage summary
    pub usage: UsageSummary,
    /// Per-tier latency metrics
    pub latencies: Vec<TierLatency>,
    /// Per-tier token drift data
    pub token_drifts: Vec<TierTokenDrift>,
    /// Per-frontend metrics
    pub frontend_metrics: Vec<FrontendMetrics>,
    /// When the data was last updated
    pub last_updated: Instant,
}

/// Shared state for dashboard data, updated by background thread.
pub type SharedDashboardState = Arc<RwLock<Option<DashboardData>>>;

/// Start a background thread that fetches dashboard data from the CCR-Rust API
/// every 500ms and updates the shared state.
///
/// Returns a `SharedDashboardState` that holds the latest data fetched from
/// `http://{host}:{port}/v1/usage`, `/v1/latencies`, and `/v1/token-drift`.
pub fn spawn_dashboard_fetcher(host: String, port: u16) -> SharedDashboardState {
    let state: SharedDashboardState = Arc::new(RwLock::new(None));
    let state_clone = Arc::clone(&state);

    thread::spawn(move || {
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to create HTTP client: {}", e);
                return;
            }
        };

        loop {
            let start = Instant::now();
            let base_url = format!("http://{}:{}", host, port);

            // Fetch usage data
            let usage_url = format!("{}/v1/usage", base_url);
            let usage_result: Option<UsageSummary> = client
                .get(&usage_url)
                .send()
                .ok()
                .and_then(|r| r.json().ok());

            // Fetch latency data
            let latencies_url = format!("{}/v1/latencies", base_url);
            let latencies_result: Option<Vec<TierLatency>> = client
                .get(&latencies_url)
                .send()
                .ok()
                .and_then(|r| r.json().ok());

            // Fetch token drift data
            let drift_url = format!("{}/v1/token-drift", base_url);
            let drift_result: Option<Vec<TierTokenDrift>> = client
                .get(&drift_url)
                .send()
                .ok()
                .and_then(|r| r.json().ok());

            // Fetch frontend metrics data
            let frontend_url = format!("{}/v1/frontend-metrics", base_url);
            let frontend_result: Option<Vec<FrontendMetrics>> = client
                .get(&frontend_url)
                .send()
                .ok()
                .and_then(|r| r.json().ok());

            // Update shared state if we got usage data (the core metric)
            if let Some(usage) = usage_result {
                let dashboard_data = DashboardData {
                    usage,
                    latencies: latencies_result.unwrap_or_default(),
                    token_drifts: drift_result.unwrap_or_default(),
                    frontend_metrics: frontend_result.unwrap_or_default(),
                    last_updated: Instant::now(),
                };

                if let Ok(mut guard) = state_clone.write() {
                    *guard = Some(dashboard_data);
                }
            }

            // Sleep to maintain 500ms interval
            let elapsed = start.elapsed();
            if elapsed < Duration::from_millis(500) {
                thread::sleep(Duration::from_millis(500) - elapsed);
            }
        }
    });

    state
}

/// Local UI state for rendering (derived from shared DashboardData)
#[derive(Debug, Default, Clone)]
struct UiState {
    token_drifts: Vec<TierTokenDrift>,
    tier_usages: Vec<TierUsage>,
    tier_latencies: Vec<TierLatency>,
    frontend_metrics: Vec<FrontendMetrics>,
    last_update: Option<Instant>,
    error: Option<String>,
    /// Global stats from the server (not local atomics)
    global_stats: GlobalStats,
}

/// Session information containing environment stats
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Current working directory
    pub cwd: String,
    /// Git branch name
    pub git_branch: String,
    /// Project version from Cargo.toml
    pub version: String,
}

impl SessionInfo {
    /// Collect session information at startup
    pub fn collect() -> Self {
        let cwd = env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let git_branch = option_env!("GIT_BRANCH").unwrap_or("unknown").to_string();
        let version = option_env!("CARGO_PKG_VERSION").unwrap_or("unknown").to_string();

        Self {
            cwd,
            git_branch,
            version,
        }
    }

    /// Format as display lines for the UI
    pub fn format_lines(&self) -> Vec<Line<'_>> {
        vec![
            Line::from(vec![
                Span::styled("CWD: ", Style::default().fg(Color::Cyan)),
                Span::raw(&self.cwd),
            ]),
            Line::from(vec![
                Span::styled("Git Branch: ", Style::default().fg(Color::Cyan)),
                Span::raw(&self.git_branch),
            ]),
            Line::from(vec![
                Span::styled("Version: ", Style::default().fg(Color::Cyan)),
                Span::raw(&self.version),
            ]),
        ]
    }
}

/// Global stats snapshot for the dashboard header.
#[derive(Debug, Clone, Default)]
pub struct GlobalStats {
    pub active_streams: f64,
    pub total_requests: u64,
    pub total_failures: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// Sync UI state from shared dashboard data
fn sync_ui_state(shared: &SharedDashboardState, ui_state: &mut UiState) {
    match shared.read() {
        Ok(guard) => {
            if let Some(ref data) = *guard {
                ui_state.token_drifts = data.token_drifts.clone();
                ui_state.tier_latencies = data.latencies.clone();
                ui_state.tier_usages = data.usage.tiers.clone();
                ui_state.frontend_metrics = data.frontend_metrics.clone();
                ui_state.last_update = Some(data.last_updated);
                ui_state.error = None;
                // Extract global stats from the fetched UsageSummary
                ui_state.global_stats = GlobalStats {
                    active_streams: data.usage.active_streams,
                    total_requests: data.usage.total_requests,
                    total_failures: data.usage.total_failures,
                    total_input_tokens: data.usage.total_input_tokens,
                    total_output_tokens: data.usage.total_output_tokens,
                };
            } else {
                ui_state.error = Some("Waiting for data...".to_string());
            }
        }
        Err(_) => {
            ui_state.error = Some("Lock poisoned".to_string());
        }
    }
}

pub fn run_dashboard(host: String, port: u16) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Start background data fetcher thread
    let shared_state = spawn_dashboard_fetcher(host.clone(), port);

    // Run the UI loop
    let tick_rate = Duration::from_millis(250);
    let res = run_loop(&mut terminal, tick_rate, host, port, shared_state);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

fn run_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    tick_rate: Duration,
    host: String,
    port: u16,
    shared_state: SharedDashboardState,
) -> Result<()>
where
    <B as Backend>::Error: std::error::Error + Send + Sync + 'static,
{
    let mut last_tick = Instant::now();
    let mut ui_state = UiState::default();
    let session_info = SessionInfo::collect();

    loop {
        // Sync UI state from shared data on each tick
        if last_tick.elapsed() >= tick_rate {
            sync_ui_state(&shared_state, &mut ui_state);
            last_tick = Instant::now();
        }

        terminal.draw(|f| ui(f, &host, port, &ui_state, &session_info))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

fn ui(f: &mut ratatui::Frame, host: &str, port: u16, state: &UiState, session_info: &SessionInfo) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Min(0),
        ])
        .split(f.area());

    // Render the header widget with global stats from fetched data
    render_header(f, main_chunks[0], host, port, &state.global_stats);

    // Token Drift Monitor section
    let drift_widget = create_token_drift_table(state);
    f.render_widget(drift_widget, main_chunks[1]);

    // Frontend Metrics section
    let frontend_widget = create_frontend_metrics_table(state);
    f.render_widget(frontend_widget, main_chunks[2]);

    // Split the bottom section into left (session info) and right (tier stats) panels
    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
        .split(main_chunks[3]);

    // Session Info panel
    let session_lines = session_info.format_lines();
    let session_info_widget = Paragraph::new(session_lines)
        .block(Block::default().borders(Borders::ALL).title("Session Info"))
        .wrap(Wrap { trim: true });
    f.render_widget(session_info_widget, body_chunks[0]);

    // Tier Statistics panel
    let tier_table = create_tier_stats_table(state);
    f.render_widget(tier_table, body_chunks[1]);
}

/// Render the header widget with global stats.
/// Layout: [Active Streams | Requests/Failures | Token Throughput]
fn render_header(f: &mut ratatui::Frame, area: Rect, host: &str, port: u16, stats: &GlobalStats) {
    // Title bar showing host:port
    let title_text = format!("CCR-Rust Dashboard | {}:{}", host, port);

    // Create three columns for stats
    let stats_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(area);

    // Column 1: Active Streams (high-visibility)
    let active_streams_style = if stats.active_streams > 0.0 {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };

    let active_streams_text = format!("{:.0}", stats.active_streams);
    let streams_paragraph = Paragraph::new(Line::from(vec![
        Span::styled("Active Streams: ", Style::default().fg(Color::White)),
        Span::styled(&active_streams_text, active_streams_style),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title_text.clone()),
    )
    .alignment(Alignment::Left);
    f.render_widget(streams_paragraph, stats_chunks[0]);

    // Column 2: Total Requests / Failures with Success Rate
    let total_requests = stats.total_requests;
    let total_failures = stats.total_failures;
    let success_rate = if total_requests > 0 {
        ((total_requests - total_failures) as f64 / total_requests as f64) * 100.0
    } else {
        0.0
    };

    let success_rate_color = if success_rate >= 90.0 {
        Color::Green
    } else if success_rate >= 80.0 {
        Color::Yellow
    } else {
        Color::Red
    };

    let requests_text = format!(
        "Requests: {} / Failures: {} (",
        format_number(total_requests),
        format_number(total_failures)
    );
    let success_text = format!("{:.1}%", success_rate);

    let requests_paragraph = Paragraph::new(Line::from(vec![
        Span::raw(requests_text),
        Span::styled(&success_text, Style::default().fg(success_rate_color)),
        Span::raw(")"),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Traffic"))
    .alignment(Alignment::Center);
    f.render_widget(requests_paragraph, stats_chunks[1]);

    // Column 3: Token Throughput (Input/Output in 'k' units)
    let input_k = stats.total_input_tokens as f64 / 1000.0;
    let output_k = stats.total_output_tokens as f64 / 1000.0;

    let tokens_paragraph = Paragraph::new(Line::from(vec![
        Span::styled(
            "In: ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:.1}k", input_k),
            Style::default().fg(Color::White),
        ),
        Span::raw(" / "),
        Span::styled(
            "Out: ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:.1}k", output_k),
            Style::default().fg(Color::White),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Token Throughput"),
    )
    .alignment(Alignment::Right);
    f.render_widget(tokens_paragraph, stats_chunks[2]);
}

fn create_token_drift_table(state: &UiState) -> Table<'_> {
    let header_cells = vec![
        Cell::from("Tier").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Samples").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Cumulative Drift %").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Last Sample Drift %").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let header = Row::new(header_cells)
        .style(Style::default().bg(Color::Blue))
        .height(1);

    let rows: Vec<Row> = if state.token_drifts.is_empty() {
        vec![Row::new(vec![
            Cell::from("No data available"),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ])
        .height(1)]
    } else {
        state
            .token_drifts
            .iter()
            .map(|drift| {
                let tier = Cell::from(drift.tier.clone());
                let samples = Cell::from(drift.samples.to_string());

                // Determine color for cumulative drift (>25% red, >10% yellow)
                let cum_drift_style = get_drift_style(drift.cumulative_drift_pct);
                let cum_drift = Cell::from(format!("{:.1}%", drift.cumulative_drift_pct))
                    .style(cum_drift_style);

                // Determine color for last sample drift (>25% red, >10% yellow)
                let last_drift_style = get_drift_style(drift.last_drift_pct);
                let last_drift =
                    Cell::from(format!("{:.1}%", drift.last_drift_pct)).style(last_drift_style);

                Row::new(vec![tier, samples, cum_drift, last_drift]).height(1)
            })
            .collect()
    };

    let block_title = if let Some(ref err) = state.error {
        format!("Token Drift Monitor | Error: {}", err)
    } else {
        "Token Drift Monitor".to_string()
    };

    let block = Block::default().borders(Borders::ALL).title(block_title);

    Table::new(
        rows,
        [
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(1)
}

/// Get the style for a drift percentage value
/// - >25%: Red (critical)
/// - >10%: Yellow (warning)
/// - Otherwise: White (normal)
fn get_drift_style(drift_pct: f64) -> Style {
    let abs_pct = drift_pct.abs();
    if abs_pct > 25.0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if abs_pct > 10.0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    }
}

/// Create a table widget showing per-tier statistics with latency, requests, tokens, and duration.
fn create_tier_stats_table(state: &UiState) -> Table<'_> {
    let header_cells = vec![
        Cell::from("Tier").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("EWMA Latency (ms)").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Requests (Success/Fail)").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Tokens (In/Out)").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Avg Duration (s)").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let header = Row::new(header_cells)
        .style(Style::default().bg(Color::Blue))
        .height(1);

    // Build a latency lookup map from tier name to EWMA latency
    let latency_map: std::collections::HashMap<String, f64> = state
        .tier_latencies
        .iter()
        .map(|l| (l.tier.clone(), l.ewma_seconds))
        .collect();

    let rows: Vec<Row> = if state.tier_usages.is_empty() {
        vec![Row::new(vec![
            Cell::from("No tier data available"),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ])
        .height(1)]
    } else {
        state
            .tier_usages
            .iter()
            .map(|usage| {
                // Tier name
                let tier_cell = Cell::from(usage.tier.clone());

                // EWMA Latency in milliseconds
                let ewma_ms = latency_map
                    .get(&usage.tier)
                    .map(|s| s * 1000.0)
                    .unwrap_or(0.0);
                let latency_style = if ewma_ms > 7000.0 {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else if ewma_ms > 5000.0 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Green)
                };
                let latency_cell = Cell::from(format!("{:.1}", ewma_ms)).style(latency_style);

                // Requests (Success/Fail)
                let success = usage.requests.saturating_sub(usage.failures);
                let failure_rate = if usage.requests > 0 {
                    (usage.failures as f64 / usage.requests as f64) * 100.0
                } else {
                    0.0
                };
                let requests_style = if failure_rate > 10.0 {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else if failure_rate > 1.0 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Green)
                };
                let requests_cell = Cell::from(format!(
                    "{}/{}",
                    format_number(success),
                    format_number(usage.failures)
                ))
                .style(requests_style);

                // Tokens (In/Out)
                let tokens_cell = Cell::from(format!(
                    "{}/{}",
                    format_number(usage.input_tokens),
                    format_number(usage.output_tokens)
                ));

                // Avg Duration
                let duration_cell = Cell::from(format!("{:.2}", usage.avg_duration_seconds));

                Row::new(vec![
                    tier_cell,
                    latency_cell,
                    requests_cell,
                    tokens_cell,
                    duration_cell,
                ])
                .height(1)
            })
            .collect()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Tier Statistics");

    Table::new(
        rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(20),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(15),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(1)
}

/// Create a table widget showing per-frontend request counts and latency metrics.
fn create_frontend_metrics_table(state: &UiState) -> Table<'_> {
    let header_cells = vec![
        Cell::from("Frontend").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Requests").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Avg Latency (ms)").style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let header = Row::new(header_cells)
        .style(Style::default().bg(Color::Blue))
        .height(1);

    let rows: Vec<Row> = if state.frontend_metrics.is_empty() {
        vec![Row::new(vec![
            Cell::from("No frontend data available"),
            Cell::from(""),
            Cell::from(""),
        ])
        .height(1)]
    } else {
        state
            .frontend_metrics
            .iter()
            .map(|metrics| {
                // Frontend name with styling
                let frontend_cell = Cell::from(metrics.frontend.clone());

                // Request count
                let requests_cell = Cell::from(format_number(metrics.requests))
                    .style(Style::default().fg(Color::Cyan));

                // Average latency with color coding
                let latency_style = if metrics.avg_latency_ms > 5000.0 {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else if metrics.avg_latency_ms > 1000.0 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Green)
                };
                let latency_cell =
                    Cell::from(format!("{:.1}", metrics.avg_latency_ms)).style(latency_style);

                Row::new(vec![frontend_cell, requests_cell, latency_cell]).height(1)
            })
            .collect()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Frontend Breakdown");

    Table::new(
        rows,
        [
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(1)
}

/// Format a large number with commas as thousands separators.
fn format_number(n: u64) -> String {
    let s = n.to_string();
    s.as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect::<Vec<_>>()
        .join(",")
}
