use opentelemetry_sdk::trace::SdkTracerProvider;
use std::time::Duration;

pub async fn initialize_provider(
    factory: impl FnOnce() -> Option<SdkTracerProvider> + Send + 'static,
) -> Option<SdkTracerProvider> {
    match tokio::task::spawn_blocking(factory).await {
        Ok(provider) => provider,
        Err(_) => {
            eprintln!("CCR telemetry disabled: provider initialization task failed");
            None
        }
    }
}

pub async fn finish_command<T>(
    provider: Option<SdkTracerProvider>,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    if let Some(provider) = provider {
        let shutdown = tokio::task::spawn_blocking(move || {
            provider.shutdown_with_timeout(Duration::from_secs(3))
        })
        .await;
        if !matches!(shutdown, Ok(Ok(()))) {
            eprintln!("CCR telemetry shutdown timed out or failed; continuing shutdown");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
    use opentelemetry_sdk::trace::{Span, SpanData, SpanProcessor};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, ThreadId};

    #[derive(Debug)]
    struct RecordingProcessor {
        shutdowns: Arc<Mutex<Vec<(Duration, ThreadId)>>>,
        fail: bool,
    }

    impl SpanProcessor for RecordingProcessor {
        fn on_start(&self, _span: &mut Span, _context: &opentelemetry::Context) {}
        fn on_end(&self, _span: SpanData) {}
        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }
        fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
            self.shutdowns
                .lock()
                .unwrap()
                .push((timeout, thread::current().id()));
            if self.fail {
                Err(OTelSdkError::InternalFailure(
                    "test shutdown failure".into(),
                ))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn every_dispatch_outcome_gets_bounded_off_runtime_cleanup() {
        let caller = thread::current().id();
        for dispatch_succeeds in [true, false] {
            for shutdown_fails in [true, false] {
                let shutdowns = Arc::new(Mutex::new(Vec::new()));
                let provider = SdkTracerProvider::builder()
                    .with_span_processor(RecordingProcessor {
                        shutdowns: shutdowns.clone(),
                        fail: shutdown_fails,
                    })
                    .build();
                let result = if dispatch_succeeds {
                    Ok(42)
                } else {
                    Err(anyhow::anyhow!("dispatch marker"))
                };
                let result = finish_command(Some(provider), result).await;
                if dispatch_succeeds {
                    assert_eq!(result.unwrap(), 42);
                } else {
                    assert_eq!(result.unwrap_err().to_string(), "dispatch marker");
                }
                let records = shutdowns.lock().unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].0, Duration::from_secs(3));
                assert_ne!(records[0].1, caller);
            }
        }
        assert_eq!(finish_command(None, Ok(17)).await.unwrap(), 17);
    }

    #[tokio::test]
    async fn provider_initialization_handles_disabled_ready_and_panicking_factories() {
        let caller = thread::current().id();
        assert!(initialize_provider(move || {
            assert_ne!(thread::current().id(), caller);
            None
        })
        .await
        .is_none());
        let provider = initialize_provider(|| Some(SdkTracerProvider::builder().build())).await;
        assert!(provider.is_some());
        finish_command(provider, Ok(())).await.unwrap();
        assert!(initialize_provider(|| panic!("test factory panic"))
            .await
            .is_none());
    }
}
