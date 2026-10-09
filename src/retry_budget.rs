// SPDX-License-Identifier: AGPL-3.0-or-later
//! Process-wide retry-amplification budget.
//!
//! Per-request retry limits do not bound aggregate retry load when many client
//! requests fail together. This tracker allows retries only within a
//! configurable percentage of active client requests, with a small floor so a
//! quiet router can still recover from one transient failure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::metrics;

const RETRY_FLOOR: u64 = 3;
/// `state` packs the active-request count in its high half and admitted
/// retries in its low half. Reserving a retry therefore linearizes against
/// both concurrent request arrival and completion.
const ACTIVE_COUNT_SHIFT: u32 = 32;
const COUNT_MASK: u64 = (1_u64 << ACTIVE_COUNT_SHIFT) - 1;

#[derive(Debug, Default)]
pub struct RetryBudget {
    state: AtomicU64,
}

/// RAII active-client-request marker.
#[derive(Debug)]
pub struct ActiveRequest {
    budget: Arc<RetryBudget>,
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        let _ = self
            .budget
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let active = current >> ACTIVE_COUNT_SHIFT;
                Some(current & COUNT_MASK | (active.saturating_sub(1) << ACTIVE_COUNT_SHIFT))
            });
    }
}

/// RAII admitted retry slot.
#[derive(Debug)]
pub struct RetryPermit {
    budget: Arc<RetryBudget>,
}

impl Drop for RetryPermit {
    fn drop(&mut self) {
        let _ = self
            .budget
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let retries = current & COUNT_MASK;
                Some(current & !COUNT_MASK | retries.saturating_sub(1))
            });
    }
}

impl RetryBudget {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_started(self: &Arc<Self>) -> ActiveRequest {
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let active = current >> ACTIVE_COUNT_SHIFT;
                Some(current & COUNT_MASK | (active.saturating_add(1) << ACTIVE_COUNT_SHIFT))
            });
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
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            let active = (current >> ACTIVE_COUNT_SHIFT).max(1);
            let retries = current & COUNT_MASK;
            let allowance = RETRY_FLOOR.max(active * u64::from(percent) / 100);
            let Some(next) = retries
                .checked_add(1)
                .filter(|reserved| *reserved <= allowance)
            else {
                metrics::record_retry_budget_overflow();
                return None;
            };
            match self.state.compare_exchange_weak(
                current,
                current & !COUNT_MASK | next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(RetryPermit {
                        budget: self.clone(),
                    })
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub fn active_requests(&self) -> usize {
        usize::try_from(self.state.load(Ordering::Acquire) >> ACTIVE_COUNT_SHIFT)
            .unwrap_or(usize::MAX)
    }

    pub fn retries_in_flight(&self) -> usize {
        usize::try_from(self.state.load(Ordering::Acquire) & COUNT_MASK).unwrap_or(usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Barrier, Mutex};

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

    /// Reservation and allowance checks are one CAS. Contending threads can
    /// defer or reject each other, but they cannot all increment after reading
    /// the same allowance.
    #[test]
    fn concurrent_reservations_cannot_exceed_the_allowance() {
        const THREADS: usize = 16;
        let budget = Arc::new(RetryBudget::new());
        let _request = budget.request_started();
        let permits: Mutex<Vec<RetryPermit>> = Mutex::new(Vec::new());
        let barrier = Barrier::new(THREADS);

        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let budget = &budget;
                let permits = &permits;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    if let Some(permit) = budget.acquire_retry(1) {
                        permits.lock().expect("permit log lock").push(permit);
                    }
                });
            }
        });

        // One active request has a floor of three retries, even at 1%, and
        // the 16 racing threads admitted exactly that many.
        assert_eq!(
            permits.lock().expect("permit log lock").len(),
            RETRY_FLOOR as usize
        );
        assert_eq!(budget.active_requests(), 1);
        assert_eq!(budget.retries_in_flight(), RETRY_FLOOR as usize);
    }
}
