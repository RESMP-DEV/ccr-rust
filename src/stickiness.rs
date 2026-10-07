// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation-provider stickiness for fallback routing.
//!
//! A conversation that starts on one protocol/provider family should keep
//! using that family while it remains eligible. This reduces mid-conversation
//! protocol translation and makes fallback less visible. The stable partition
//! preserves configured order inside each family and never moves a directly
//! routed or web-search pinned prefix.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::router::AnthropicRequest;

#[derive(Debug, Clone)]
struct Entry {
    provider: String,
    expires_at: Instant,
}

/// TTL cache mapping a stable conversation key to its preferred provider.
#[derive(Debug)]
pub struct StickySessionTracker {
    entries: Mutex<HashMap<String, Entry>>,
    ttl: Duration,
}

impl Default for StickySessionTracker {
    fn default() -> Self {
        Self::new(Duration::from_secs(3600))
    }
}

impl StickySessionTracker {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Extract a stable conversation key. No key means keep today's routing.
    pub fn conversation_key(request: &AnthropicRequest) -> Option<String> {
        // The normalized Anthropic request currently has no metadata field.
        // Use the leading system prompt as the stable fallback; hash it so
        // private prompt text never becomes a cache key in logs or metrics.
        let system = request.system.as_ref()?;
        let serialized = serde_json::to_string(system).ok()?;
        let digest = Sha256::digest(serialized.as_bytes());
        Some(format!("system:{digest:x}"))
    }

    pub fn remember(&self, key: &str, provider: &str) {
        if key.is_empty() || provider.is_empty() {
            return;
        }
        let mut entries = self.entries.lock();
        // Bound the local map defensively; conversations are not unbounded
        // state and stale entries are harmless to evict.
        if entries.len() >= 10_000 {
            let now = Instant::now();
            entries.retain(|_, entry| entry.expires_at > now);
        }
        if entries.len() >= 10_000 {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            key.to_string(),
            Entry {
                provider: provider.to_string(),
                expires_at: Instant::now() + self.ttl,
            },
        );
    }

    pub fn preferred_provider(&self, key: &str) -> Option<String> {
        let mut entries = self.entries.lock();
        let now = Instant::now();
        let entry = entries.get(key)?;
        if entry.expires_at <= now {
            entries.remove(key);
            return None;
        }
        Some(entry.provider.clone())
    }

    /// Stable-partition the fallback portion by provider family. The prefix
    /// remains untouched so direct routing and web-search invariants hold.
    pub fn stable_partition(
        &self,
        conversation_key: &str,
        ordered: &mut [(String, String)],
        pinned_prefix_len: usize,
    ) {
        if conversation_key.is_empty() {
            return;
        }
        let Some(preferred) = self.preferred_provider(conversation_key) else {
            return;
        };
        let split = pinned_prefix_len.min(ordered.len());
        let (_, fallback) = ordered.split_at_mut(split);
        fallback.sort_by_key(|(tier, _)| {
            let provider = tier.split(',').next().unwrap_or(tier.as_str());
            provider != preferred
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_system(system: &str) -> AnthropicRequest {
        serde_json::from_value(serde_json::json!({
            "model": "p,m",
            "messages": [{"role":"user","content":"hi"}],
            "system": system
        }))
        .unwrap()
    }

    #[test]
    fn system_prompt_keys_are_stable_and_hashed() {
        let one = StickySessionTracker::conversation_key(&request_with_system("stable")).unwrap();
        let two = StickySessionTracker::conversation_key(&request_with_system("stable")).unwrap();
        let other =
            StickySessionTracker::conversation_key(&request_with_system("different")).unwrap();
        assert_eq!(one, two);
        assert_ne!(one, other);
        assert!(!one.contains("stable"));
    }

    #[test]
    fn partition_preserves_prefix_and_relative_order() {
        let tracker = StickySessionTracker::new(Duration::from_secs(60));
        let key = "conversation";
        tracker.remember(key, "preferred");
        let mut ordered = vec![
            ("pinned,m".to_string(), "pinned".to_string()),
            ("other,a".to_string(), "other-a".to_string()),
            ("preferred,b".to_string(), "preferred-b".to_string()),
            ("preferred,a".to_string(), "preferred-a".to_string()),
            ("other,b".to_string(), "other-b".to_string()),
        ];
        // Put the remembered key at the front as handle_messages would.
        tracker.stable_partition(key, &mut ordered, 1);
        let providers: Vec<_> = ordered
            .iter()
            .map(|(tier, _)| tier.split(',').next().unwrap().to_string())
            .collect();
        assert_eq!(
            providers,
            vec![
                "pinned".to_string(),
                "preferred".to_string(),
                "preferred".to_string(),
                "other".to_string(),
                "other".to_string()
            ]
        );
        assert_eq!(ordered[2].0, "preferred,a");
        assert_eq!(ordered[3].0, "other,a");
    }

    #[test]
    fn ttl_expiry_forgets_preference() {
        let tracker = StickySessionTracker::new(Duration::from_secs(0));
        tracker.remember("conversation", "preferred");
        assert!(tracker.preferred_provider("conversation").is_none());
    }
}
