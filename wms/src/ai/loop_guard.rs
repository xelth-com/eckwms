//! In-memory per-entity attempt counter that acts as a last-resort circuit
//! breaker against AI-worker infinite loops.
//!
//! The existing exponential backoff (summarization.rs / embeddings.rs) relies
//! on the DB field `summary_retries` / `embedding_retries` being incremented
//! after each attempt. On 2026-04-21 we learned that if the success-path
//! UPDATE silently matches 0 rows (e.g., ID-format regression), the counter
//! never advances and the worker loops on the same doc at full speed
//! (3–4 calls/minute/doc, burning ~6M Gemini tokens/hour before the hourly
//! Observer catches it).
//!
//! This guard is independent of DB state: it counts attempts in-process and
//! blocks re-entry for a cooldown window once a threshold is crossed.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Any entity hit more than this many times within WINDOW is considered looping.
const MAX_ATTEMPTS: u32 = 3;
/// Rolling window for counting attempts.
const WINDOW: Duration = Duration::from_secs(60);
/// How long to block re-entry once an entity is flagged.
const COOLDOWN: Duration = Duration::from_secs(300);
/// Evict entries older than this from the map (keeps memory bounded).
const EVICT_AFTER: Duration = Duration::from_secs(600);

struct Entry {
    attempts: Vec<Instant>,
    blocked_until: Option<Instant>,
}

impl Entry {
    fn new() -> Self {
        Self { attempts: Vec::new(), blocked_until: None }
    }
}

pub struct LoopGuard {
    inner: Mutex<HashMap<String, Entry>>,
}

impl LoopGuard {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    /// Record an attempt for `entity_id`. Returns `true` if the entity should
    /// be processed, `false` if it's in cooldown (caller must skip).
    pub fn check_and_record(&self, entity_id: &str) -> bool {
        let now = Instant::now();
        let mut map = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };

        // Periodic eviction to bound memory.
        map.retain(|_, e| {
            let fresh_attempt = e.attempts.last().map_or(false, |t| now.duration_since(*t) < EVICT_AFTER);
            let still_blocked = e.blocked_until.map_or(false, |t| t > now);
            fresh_attempt || still_blocked
        });

        let entry = map.entry(entity_id.to_string()).or_insert_with(Entry::new);

        if let Some(until) = entry.blocked_until {
            if until > now {
                return false;
            }
            // Cooldown expired — reset the counter and allow.
            entry.blocked_until = None;
            entry.attempts.clear();
        }

        // Drop attempts outside the rolling window.
        entry.attempts.retain(|t| now.duration_since(*t) < WINDOW);
        entry.attempts.push(now);

        if entry.attempts.len() as u32 > MAX_ATTEMPTS {
            entry.blocked_until = Some(now + COOLDOWN);
            return false;
        }
        true
    }

    /// Mark `entity_id` as successfully completed — clear any tracked state so
    /// it doesn't linger in the map.
    pub fn clear(&self, entity_id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(entity_id);
        }
    }
}

impl Default for LoopGuard {
    fn default() -> Self {
        Self::new()
    }
}
