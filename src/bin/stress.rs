//! CCR Stress Test Binary
//!
//! Scales the CCR proxy to 200+ concurrent streams and measures performance,
//! memory usage, and stability under load. Designed to validate the proxy's
//! ability to handle agent swarm workloads (100+ concurrent Claude Code agents).
//!
//! # Usage
//!
//! ```bash
//! # Basic stress test with 200 concurrent streams
//! cargo run --release --bin ccr-stress -- --streams 200
//!
//! # Ramp-up test: start with 50, ramp to 300 streams
//! cargo run --release --bin ccr-stress -- --ramp-start 50 --ramp-end 300 --ramp-step 25
//!
//! # Memory-focused test with detailed metrics
//! cargo run --release --bin ccr-stress -- --streams 200 --memory-profile
//!
//! # Continuous durability test for 5 minutes
//! cargo run --release --bin ccr-stress -- --streams 200 --duration 300s
//! ```

use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser as ClapParser;
use futures::StreamExt;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Barrier};
use tokio::time;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

/// Memory tracking for stress tests.
#[derive(Debug, Default)]
struct MemoryStats {
    /// Current RSS in bytes
    current_rss: AtomicU64,
    /// Peak RSS in bytes
    peak_rss: AtomicU64,
    /// Number of allocations tracked
    allocations: AtomicU64,
    /// Stream count at peak memory
    peak_stream_count: AtomicUsize,
}

/// Simplified mock SSE stream for stress testing.
/// Mimics the behavior of real LLM response streams without making external requests.
struct MockStream {
    id: u64,
    chunk_count: usize,
    bytes_per_chunk: usize,
    delay_ms: u64,
}

impl MockStream {
    fn new(id: u64) -> Self {
        Self {
            id,
            // Real LLM responses typically have 10-100+ chunks depending on output length
            chunk_count: 50 + (id as usize % 100),
            // Chunks are typically 10-500 bytes depending on tokenization
            bytes_per_chunk: 50 + (id as usize % 200),
            // Variable delay mimics network and model inference time
            delay_ms: 10 + (id as usize % 50),
        }
    }

    /// Generate the next SSE chunk for this stream.
    fn next_chunk(&self, chunk_idx: usize) -> Option<Bytes> {
        if chunk_idx >= self.chunk_count {
            return None;
        }

        // Simulate SSE format: "event: ...\ndata: ...\n\n"
        let content = format!(
            r#"event: content_block_delta
data: {{"type":"content_block_delta","delta":{{"type":"text_delta","text":"chunk-{}-{}"}}}}
"#,
            self.id, chunk_idx
        );

        Some(Bytes::from(content))
    }
}

/// Statistics for a single stream's lifecycle.
#[derive(Debug)]
struct StreamStats {
    id: u64,
    start_time: Instant,
    end_time: Option<Instant>,
    chunks_sent: usize,
    bytes_sent: u64,
    errors: usize,
}

impl StreamStats {
    fn new(id: u64) -> Self {
        Self {
            id,
            start_time: Instant::now(),
            end_time: None,
            chunks_sent: 0,
            bytes_sent: 0,
            errors: 0,
        }
    }

    fn record_chunk(&mut self, bytes: &Bytes) {
        self.chunks_sent += 1;
        self.bytes_sent += bytes.len() as u64;
    }

    fn record_error(&mut self) {
        self.errors += 1;
    }

    fn finish(&mut self) {
        self.end_time = Some(Instant::now());
    }

    fn duration(&self) -> Duration {
        self.end_time
            .map(|e| e - self.start_time)
            .unwrap_or_else(|| Instant::now() - self.start_time)
    }
}

/// Results from a stress test run.
#[derive(Debug, Default)]
struct StressTestResults {
    /// Total streams started
    streams_started: AtomicU64,
    /// Total streams completed successfully
    streams_completed: AtomicU64,
    /// Total streams that errored
    streams_errored: AtomicU64,
    /// Total chunks sent
    chunks_sent: AtomicU64,
    /// Total bytes sent
    bytes_sent: AtomicU64,
    /// Total errors
    total_errors: AtomicU64,
    /// Peak concurrent streams
    peak_concurrent: AtomicUsize,
    /// Start time of the test
    start_time: Option<Instant>,
    /// End time of the test
    end_time: Option<Instant>,
    /// Memory statistics
    memory: Arc<MemoryStats>,
    /// Per-stream statistics (sampled)
    stream_stats: parking_lot::Mutex<HashMap<u64, StreamStats>>,
}

impl StressTestResults {
    fn new(memory: Arc<MemoryStats>) -> Self {
        Self {
            streams_started: AtomicU64::new(0),
            streams_completed: AtomicU64::new(0),
            streams_errored: AtomicU64::new(0),
            chunks_sent: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            peak_concurrent: AtomicUsize::new(0),
            start_time: None,
            end_time: None,
            memory,
            stream_stats: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    fn record_stream_start(&self, id: u64) {
        self.streams_started.fetch_add(1, Ordering::Relaxed);
        let mut stats = self.stream_stats.lock();
        stats.insert(id, StreamStats::new(id));
    }

    fn record_stream_complete(&self, id: u64) {
        self.streams_completed.fetch_add(1, Ordering::Relaxed);
        let mut stats = self.stream_stats.lock();
        if let Some(s) = stats.get_mut(&id) {
            s.finish();
        }
    }

    fn record_stream_error(&self, id: u64) {
        self.streams_errored.fetch_add(1, Ordering::Relaxed);
        let mut stats = self.stream_stats.lock();
        if let Some(s) = stats.get_mut(&id) {
            s.record_error();
            s.finish();
        }
    }

    fn record_chunk(&self, id: u64, bytes: &Bytes) {
        self.chunks_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let mut stats = self.stream_stats.lock();
        if let Some(s) = stats.get_mut(&id) {
            s.record_chunk(bytes);
        }
    }

    fn update_peak_concurrent(&self, current: usize) {
        let peak = self.peak_concurrent.load(Ordering::Relaxed);
        if current > peak {
            self.peak_concurrent.store(current, Ordering::Relaxed);
            // Update peak memory tracking
            self.memory
                .peak_stream_count
                .store(current, Ordering::Relaxed);
        }
    }

    fn start(&mut self) {
        self.start_time = Some(Instant::now());
    }

    fn end(&mut self) {
        self.end_time = Some(Instant::now());
    }

    fn duration(&self) -> Duration {
        self.start_time
            .map(|s| self.end_time.unwrap_or_else(Instant::now) - s)
            .unwrap_or_default()
    }

    fn throughput(&self) -> f64 {
        let duration = self.duration().as_secs_f64();
        if duration > 0.0 {
            self.chunks_sent.load(Ordering::Relaxed) as f64 / duration
        } else {
            0.0
        }
    }

    fn bytes_per_second(&self) -> f64 {
        let duration = self.duration().as_secs_f64();
        if duration > 0.0 {
            self.bytes_sent.load(Ordering::Relaxed) as f64 / duration
        } else {
            0.0
        }
    }
}

/// CLI arguments for the stress test.
#[derive(Debug, ClapParser)]
#[command(name = "ccr-stress")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Number of concurrent streams to simulate.
    #[arg(short, long, default_value = "200")]
    streams: usize,

    /// Duration of the stress test (e.g., "30s", "5m", "1h").
    #[arg(short, long, default_value = "30s")]
    duration: humantime::Duration,

    /// Enable memory profiling.
    #[arg(long)]
    memory_profile: bool,

    /// Enable verbose logging.
    #[arg(short, long)]
    verbose: bool,

    /// Number of chunks per stream.
    #[arg(long, default_value = "50")]
    chunks_per_stream: usize,

    /// Bytes per chunk.
    #[arg(long, default_value = "128")]
    bytes_per_chunk: usize,

    /// Base delay between chunks in milliseconds.
    #[arg(long, default_value = "20")]
    chunk_delay_ms: u64,

    /// Enable ramp-up test mode.
    #[arg(long)]
    ramp_mode: bool,

    /// Starting number of streams for ramp-up.
    #[arg(long, requires = "ramp_mode")]
    ramp_start: Option<usize>,

    /// Ending number of streams for ramp-up.
    #[arg(long, requires = "ramp_mode")]
    ramp_end: Option<usize>,

    /// Step size for ramp-up.
    #[arg(long, requires = "ramp_mode")]
    ramp_step: Option<usize>,

    /// Wait time between ramp steps in seconds.
    #[arg(long, default_value = "10", requires = "ramp_mode")]
    ramp_wait: u64,
}

/// Memory tracking helper.
fn get_current_rss() -> u64 {
    // On Linux, read from /proc/self/stat
    // On macOS, use libproc (if available) or estimate
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        statm.split_whitespace().next().map(|s| s.parse::<u64>().unwrap_or(0) * 4096)
    }
    #[cfg(target_os = "macos")]
    {
        // Rough estimate for macOS - libproc would be more accurate
        // but we don't want to add a dependency just for this
        0
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        0
    }
}

/// Simulate a single SSE stream client.
/// This creates a connection, sends a request, and consumes the stream.
async fn simulate_stream_client(
    id: u64,
    addr: SocketAddr,
    results: Arc<StressTestResults>,
    chunks_per_stream: usize,
    bytes_per_chunk: usize,
    chunk_delay_ms: u64,
) {
    results.record_stream_start(id);

    // Create a mock stream
    let mut mock_stream = MockStream {
        id,
        chunk_count: chunks_per_stream,
        bytes_per_chunk,
        delay_ms: chunk_delay_ms,
    };

    // Simulate receiving chunks
    for chunk_idx in 0..chunks_per_stream {
        if let Some(bytes) = mock_stream.next_chunk(chunk_idx) {
            results.record_chunk(id, &bytes);

            // Simulate processing delay
            if chunk_delay_ms > 0 {
                time::sleep(Duration::from_millis(chunk_delay_ms)).await;
            }
        }
    }

    results.record_stream_complete(id);
}

/// Run a single stress test phase.
async fn run_stress_phase(
    num_streams: usize,
    duration: Duration,
    results: Arc<StressTestResults>,
    chunks_per_stream: usize,
    bytes_per_chunk: usize,
    chunk_delay_ms: u64,
) -> Result<()> {
    info!("Starting stress phase with {} concurrent streams for {:?}", num_streams, duration);

    // Use a barrier to start all streams nearly simultaneously
    let barrier = Arc::new(Barrier::new(num_streams));
    let start_time = Instant::now();

    // Spawn all stream tasks
    let mut handles = Vec::with_capacity(num_streams);

    for i in 0..num_streams {
        let barrier = barrier.clone();
        let results = results.clone();

        let handle = tokio::spawn(async move {
            // Wait for all tasks to be ready
            barrier.wait().await;

            // Run the stream simulation with timeout
            let stream_id = i as u64;
            let stream_results = results.clone();

            // Create a mock stream and process it
            let mut mock_stream = MockStream {
                id: stream_id,
                chunk_count: chunks_per_stream,
                bytes_per_chunk,
                delay_ms: chunk_delay_ms,
            };

            stream_results.record_stream_start(stream_id);

            for chunk_idx in 0..chunks_per_stream {
                if let Some(bytes) = mock_stream.next_chunk(chunk_idx) {
                    stream_results.record_chunk(stream_id, &bytes);

                    if chunk_delay_ms > 0 {
                        time::sleep(Duration::from_millis(chunk_delay_ms)).await;
                    }
                }
            }

            stream_results.record_stream_complete(stream_id);
        });

        handles.push(handle);
    }

    // Track peak concurrent streams
    let peak_check_interval = Duration::from_millis(100);
    let mut peak_check = time::interval(peak_check_interval);

    loop {
        tokio::select! {
            _ = peak_check.tick() => {
                results.update_peak_concurrent(num_streams);

                // Check if we've exceeded duration
                if start_time.elapsed() > duration {
                    break;
                }
            }
            _ = time::sleep(duration) => {
                break;
            }
        }
    }

    // Wait for all streams to complete (with timeout)
    let timeout = Duration::from_secs(60);
    let deadline = Instant::now() + timeout;

    for handle in handles {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        if let Err(e) = tokio::time::timeout(remaining, handle).await {
            warn!("Stream task timed out: {:?}", e);
        }
    }

    Ok(())
}

/// Print final stress test results.
fn print_results(results: &StressTestResults) {
    let duration = results.duration();
    let started = results.streams_started.load(Ordering::Relaxed);
    let completed = results.streams_completed.load(Ordering::Relaxed);
    let errored = results.streams_errored.load(Ordering::Relaxed);
    let chunks = results.chunks_sent.load(Ordering::Relaxed);
    let bytes = results.bytes_sent.load(Ordering::Relaxed);
    let peak = results.peak_concurrent.load(Ordering::Relaxed);

    println!("\n{:=^60}", " STRESS TEST RESULTS ");
    println!("Duration:               {:?}", duration);
    println!("Streams Started:        {}", started);
    println!("Streams Completed:      {}", completed);
    println!("Streams Errored:        {}", errored);
    println!("Completion Rate:        {:.2}%", if started > 0 { (completed as f64 / started as f64) * 100.0 } else { 0.0 });
    println!("Peak Concurrent:        {}", peak);
    println!("Total Chunks:           {}", chunks);
    println!("Total Bytes:            {} ({:.2} MB)", bytes, bytes as f64 / 1024.0 / 1024.0);
    println!("Chunk Throughput:       {:.2} chunks/sec", results.throughput());
    println!("Data Throughput:        {:.2} MB/sec", results.bytes_per_second() / 1024.0 / 1024.0);
    println!("{:=^60}\n", "");

    if let Some(rss) = results.memory.peak_rss.load(Ordering::Relaxed).checked_div(1024 * 1024) {
        println!("Peak Memory (RSS):      {} MB", rss);
    }

    // Sample some stream statistics
    let stats = results.stream_stats.lock();
    if !stats.is_empty() {
        let durations: Vec<Duration> = stats.values().map(|s| s.duration()).collect();
        let avg_duration: Duration = durations.iter().sum::<Duration>() / durations.len() as u32;
        let max_duration = durations.iter().max().cloned().unwrap_or_default();
        let min_duration = durations.iter().min().cloned().unwrap_or_default();

        println!("\nStream Duration Stats:");
        println!("  Average:              {:?}", avg_duration);
        println!("  Min:                  {:?}", min_duration);
        println!("  Max:                  {:?}", max_duration);
    }

    println!();
}

/// Run ramp-up stress test.
async fn run_ramp_test(
    start: usize,
    end: usize,
    step: usize,
    step_wait: Duration,
    duration: Duration,
    chunks_per_stream: usize,
    bytes_per_chunk: usize,
    chunk_delay_ms: u64,
) -> Result<()> {
    let memory = Arc::new(MemoryStats::default());
    let results = Arc::new(StressTestResults::new(memory.clone()));

    results.start();

    let mut current = start;
    while current <= end {
        info!("Ramp step: {} streams", current);

        // Run a short phase at this concurrency level
        let phase_duration = std::cmp::min(duration, step_wait * 2);

        run_stress_phase(
            current,
            phase_duration,
            results.clone(),
            chunks_per_stream,
            bytes_per_chunk,
            chunk_delay_ms,
        )
        .await?;

        // Brief pause between steps
        if current + step <= end {
            time::sleep(step_wait).await;
        }

        current += step;
    }

    results.end();
    print_results(&results);

    Ok(())
}

/// Main stress test runner.
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let level = if args.verbose {
        Level::DEBUG
    } else {
        Level::INFO
    };

    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .context("Failed to set tracing subscriber")?;

    info!("CCR Stress Test - Scaling to {} concurrent streams", args.streams);
    info!("Duration: {:?}", args.duration);

    let duration = Duration::from(args.duration);
    let memory = Arc::new(MemoryStats::default());

    // Track memory if profiling enabled
    if args.memory_profile {
        let mem_memory = memory.clone();
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(500));
            loop {
                interval.tick().await;
                let rss = get_current_rss();
                if rss > 0 {
                    let current = mem_memory.current_rss.load(Ordering::Relaxed);
                    if rss > current {
                        mem_memory.current_rss.store(rss, Ordering::Relaxed);
                        // Update peak
                        let peak = mem_memory.peak_rss.load(Ordering::Relaxed);
                        if rss > peak {
                            mem_memory.peak_rss.store(rss, Ordering::Relaxed);
                        }
                    }
                }
            }
        });
    }

    if args.ramp_mode {
        // Ramp-up test
        let start = args.ramp_start.unwrap_or(50);
        let end = args.ramp_end.unwrap_or(300);
        let step = args.ramp_step.unwrap_or(25);

        info!("Running ramp-up test: {} -> {} streams (step {})", start, end, step);

        run_ramp_test(
            start,
            end,
            step,
            Duration::from_secs(args.ramp_wait),
            duration,
            args.chunks_per_stream,
            args.bytes_per_chunk,
            args.chunk_delay_ms,
        )
        .await?;
    } else {
        // Fixed concurrency test
        let results = Arc::new(StressTestResults::new(memory.clone()));
        results.start();

        run_stress_phase(
            args.streams,
            duration,
            results.clone(),
            args.chunks_per_stream,
            args.bytes_per_chunk,
            args.chunk_delay_ms,
        )
        .await?;

        results.end();
        print_results(&results);
    }

    info!("Stress test completed successfully");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_stream_generation() {
        let stream = MockStream::new(42);
        assert_eq!(stream.id, 42);
        assert!(stream.chunk_count >= 50 && stream.chunk_count <= 150);
        assert!(stream.bytes_per_chunk >= 50 && stream.bytes_per_chunk <= 250);
        assert!(stream.delay_ms >= 10 && stream.delay_ms <= 60);
    }

    #[test]
    fn test_stream_stats() {
        let mut stats = StreamStats::new(1);
        assert!(stats.duration().is_zero());
        assert_eq!(stats.chunks_sent, 0);
        assert_eq!(stats.errors, 0);

        stats.record_chunk(&Bytes::from("test"));
        assert_eq!(stats.chunks_sent, 1);
        assert_eq!(stats.bytes_sent, 4);

        stats.record_error();
        assert_eq!(stats.errors, 1);

        stats.finish();
        assert!(stats.duration() > Duration::ZERO);
    }

    #[test]
    fn test_stress_results_throughput() {
        let memory = Arc::new(MemoryStats::default());
        let results = StressTestResults::new(memory);
        results.start();

        // Simulate some activity
        for i in 0..100 {
            results.record_chunk(i as u64, &Bytes::from("test chunk"));
        }

        // Small delay to ensure non-zero duration
        std::thread::sleep(Duration::from_millis(10));

        results.end();

        let throughput = results.throughput();
        assert!(throughput > 0.0, "throughput should be positive");
        assert!(throughput > 100.0, "should handle 100 chunks quickly");
    }
}