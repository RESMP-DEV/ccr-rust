// SPDX-License-Identifier: AGPL-3.0-or-later
//! Process-wide retry-amplification budget.
//!
//! Per-request retry limits do not bound aggregate retry load when many client
//! requests fail together. This tracker allows retries only within a
//! configurable percentage of active client requests, with a small floor so a
//! quiet router can still recover from one transient failure.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::metrics;

const RETRY_FLOOR: usize = 3;

#[derive(Debug, Default)]
pub struct RetryBudget {
    active_requests: AtomicUsize,
    retries_in_flight: AtomicUsize,
}

/// RAII active-client-request marker.
#[derive(Debug)]
pub struct ActiveRequest {
    budget: Arc<RetryBudget>,
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.budget.active_requests.fetch_sub(1, Ordering::AcqRel);
    }
}

/// RAII admitted retry slot.
#[derive(Debug)]
pub struct RetryPermit {
    budget: Arc<RetryBudget>,
}

impl Drop for RetryPermit {
    fn drop(&mut self) {
        self.budget.retries_in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl RetryBudget {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_started(self: &Arc<Self>) -> ActiveRequest {
        self.active_requests.fetch_add(1, Ordering::AcqRel);
        ActiveRequest {
            budget: self.clone(),
        }
    }

    /// Reserve one retry when `percent` permits it. `0` disables retries.
    pub fn acquire_retry(self: &Arc<Self>, percent: u8) -> Option<RetryPermit> {
        if percent == 0 {
            metrics::record_retry_budget_overflow();
            return None;
        }
        let active = self.active_requests.load(Ordering::Acquire).max(1);
        let allowance = RETRY_FLOOR
            .max(((active as u64 * u64::from(percent)) / 100).min(usize::MAX as u64) as usize);
        let current = self.retries_in_flight.fetch_add(1, Ordering::AcqRel);
        if current >= allowance {
            self.retries_in_flight.fetch_sub(1, Ordering::AcqRel);
            metrics::record_retry_budget_overflow();
            return None;
        }
        Some(RetryPermit {
            budget: self.clone(),
        })
    }

    pub fn active_requests(&self) -> usize {
        self.active_requests.load(Ordering::Acquire)
    }

    pub fn retries_in_flight(&self) -> usize {
        self.retries_in_flight.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_percent_disables_retries() {
        let budget = Arc::new(RetryBudget::new());
        let _request = budget.request_started();
        assert!(budget.acquire_retry(0).is_none());
    }

    #[test]
    fn floor_and_release_work() {
        let budget = Arc::new(RetryBudget::new());
        let _request = budget.request_started();
        let permits: Vec<_> = (0..3).map(|_| budget.acquire_retry(20).unwrap()).collect();
        assert_eq!(budget.retries_in_flight(), 3);
        // One active request has a floor of three retries, not 20% = 0.
        assert!(budget.acquire_retry(20).is_none());
        drop(permits);
        assert_eq!(budget.retries_in_flight(), 0);
    }
}
