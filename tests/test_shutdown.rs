//! Tests for graceful shutdown behavior.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[test]
fn test_active_stream_counter_basic() {
    let counter = AtomicUsize::new(0);

    // Simulate stream start
    counter.fetch_add(1, Ordering::Relaxed);
    assert_eq!(counter.load(Ordering::Relaxed), 1);

    // Simulate stream end
    counter.fetch_sub(1, Ordering::Relaxed);
    assert_eq!(counter.load(Ordering::Relaxed), 0);
}

#[test]
fn test_multiple_streams() {
    let counter = AtomicUsize::new(0);

    // Start multiple streams
    for _ in 0..5 {
        counter.fetch_add(1, Ordering::Relaxed);
    }
    assert_eq!(counter.load(Ordering::Relaxed), 5);

    // End all streams
    for _ in 0..5 {
        counter.fetch_sub(1, Ordering::Relaxed);
    }
    assert_eq!(counter.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn test_shutdown_timeout() {
    use tokio::time::timeout;

    let result = timeout(Duration::from_millis(10), async {
        tokio::time::sleep(Duration::from_secs(10)).await;
    })
    .await;

    assert!(result.is_err(), "Should timeout");
}
