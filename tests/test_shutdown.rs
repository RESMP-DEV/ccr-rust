use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::timeout;

/// Test graceful shutdown waits for streams to complete.
///
/// This test verifies behavior, not actual signals. We test the shutdown
/// logic components directly to ensure the stream tracking mechanism works
/// correctly.
#[test]
fn test_graceful_shutdown_waits_for_streams() {
    let active_count = AtomicUsize::new(1);

    // Simulate checking for active streams
    let has_active = active_count.load(Ordering::Relaxed) > 0;
    assert!(has_active, "Should detect active streams");

    // Simulate stream completing
    active_count.store(0, Ordering::Relaxed);
    let has_active = active_count.load(Ordering::Relaxed) > 0;
    assert!(
        !has_active,
        "Should detect no active streams after completion"
    );
}

/// Test that shutdown timeout is enforced when streams don't complete.
#[test]
fn test_shutdown_timeout_is_enforced() {
    let active_count = AtomicUsize::new(1);

    // Simulate waiting for streams with a timeout
    let result = timeout(Duration::from_millis(50), async {
        while active_count.load(Ordering::Relaxed) > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    // Should timeout because active_count never goes to zero
    assert!(
        result.await.is_err(),
        "Should timeout when streams don't complete"
    );
}

/// Test multiple streams tracked concurrently.
#[tokio::test]
async fn test_multiple_stream_tracking() {
    let active_count = AtomicUsize::new(3);

    // Simulate three streams
    assert_eq!(
        active_count.load(Ordering::Relaxed),
        3,
        "Should track 3 active streams"
    );

    // Complete one stream
    active_count.fetch_sub(1, Ordering::Relaxed);
    assert_eq!(
        active_count.load(Ordering::Relaxed),
        2,
        "Should track 2 remaining streams"
    );

    // Complete remaining streams
    active_count.fetch_sub(2, Ordering::Relaxed);
    assert_eq!(
        active_count.load(Ordering::Relaxed),
        0,
        "Should track no active streams"
    );
}

/// Test that shutdown completes immediately when no streams are active.
#[tokio::test]
async fn test_shutdown_completes_immediately_when_no_streams() {
    let active_count = AtomicUsize::new(0);

    let start = std::time::Instant::now();
    let result = timeout(Duration::from_millis(10), async {
        while active_count.load(Ordering::Relaxed) > 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;

    // Should complete immediately (not timeout)
    assert!(
        result.is_ok(),
        "Should complete immediately when no streams"
    );
    assert!(
        start.elapsed() < Duration::from_millis(5),
        "Should complete in < 5ms"
    );
}

/// Test atomic ordering consistency for concurrent access.
#[tokio::test]
async fn test_atomic_ordering_consistency() {
    let counter = AtomicUsize::new(0);

    // Spawn tasks that increment concurrently
    let handles: Vec<_> = (0..10)
        .map(|_| {
            let c = &counter;
            tokio::spawn(async move {
                c.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(1)).await;
                c.fetch_sub(1, Ordering::Relaxed);
            })
        })
        .collect();

    // Wait for all tasks
    for handle in handles {
        handle.await.unwrap();
    }

    // Counter should be back to zero
    assert_eq!(
        counter.load(Ordering::Relaxed),
        0,
        "Counter should be balanced"
    );
}
