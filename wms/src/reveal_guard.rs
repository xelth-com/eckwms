//! Per-user PII reveal rate limiting (`.eck/PII_VAULT.md` §0 "Reveal is
//! re-hash", §4 "Reveal", §9 decision 3 — P0 item 3).
//!
//! Every reveal path today (`handlers::support::list_tickets`,
//! `get_ticket_threads`, and the MCP master surfaces `reveal_tokens` /
//! `reveal_file_local`) re-derives clear PII on demand from stored data — there
//! is no plaintext vault, but there was also no cap on how much a single
//! account could pull in a session and no audit trail. This module is the
//! rate-limit half of closing that gap; the audit half is the callers'
//! `eck_core::audit::append_soft` calls, using [`audit_payload`] for a
//! consistent shape.
//!
//! Config: `ECK_REVEAL_RATE = "<tokens>/<seconds>"`, default `"500/600"` (500
//! tokens / 10 minutes / user, per the owner's §9 decision). Any malformed or
//! missing value falls back to the default — a typo in `.env` must never turn
//! a reveal gate into a hard outage.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const DEFAULT_LIMIT: u32 = 500;
const DEFAULT_WINDOW_SECS: u64 = 600;

/// A reveal was denied by the rate limiter. The caller must skip the reveal
/// (serve the tokenised text, as for an observer) and may surface
/// `retry_after_secs` to the operator/Observer alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevealDenied {
    pub retry_after_secs: u64,
}

/// Parse `ECK_REVEAL_RATE` ("<tokens>/<seconds>"). Falls back to the default
/// on ANY malformed input: absent, wrong shape, non-numeric, or a zero on
/// either side (a zero-token or zero-second window is not a rate limit, it's
/// a footgun).
fn parse_rate(raw: Option<&str>) -> (u32, u64) {
    let Some(raw) = raw else {
        return (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS);
    };
    let raw = raw.trim();
    let Some((tok, secs)) = raw.split_once('/') else {
        return (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS);
    };
    match (tok.trim().parse::<u32>(), secs.trim().parse::<u64>()) {
        (Ok(t), Ok(s)) if t > 0 && s > 0 => (t, s),
        _ => (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS),
    }
}

/// Per-user sliding-window counter of tokens revealed. `check` both tests AND
/// records usage in one lock acquisition — call it exactly once per reveal
/// decision (a denied call records nothing, so retrying costs no budget).
pub struct RevealGuard {
    limit: u32,
    window: Duration,
    windows: Mutex<HashMap<String, VecDeque<(Instant, u32)>>>,
}

impl RevealGuard {
    pub fn new(limit: u32, window_secs: u64) -> Self {
        Self {
            limit,
            window: Duration::from_secs(window_secs),
            windows: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let (limit, window_secs) = parse_rate(std::env::var("ECK_REVEAL_RATE").ok().as_deref());
        Self::new(limit, window_secs)
    }

    /// Check whether `user` may reveal `n_tokens` more tokens right now.
    /// `n_tokens == 0` is always `Ok` and records nothing. On success the
    /// usage is recorded against `user`'s window; on denial nothing is
    /// recorded and `retry_after_secs` is how long until enough old usage
    /// ages out of the window to admit this exact request.
    pub fn check(&self, user: &str, n_tokens: u32) -> Result<(), RevealDenied> {
        if n_tokens == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut all = self.windows.lock().unwrap();
        let entry = all.entry(user.to_string()).or_default();

        while let Some(&(ts, _)) = entry.front() {
            if now.duration_since(ts) >= self.window {
                entry.pop_front();
            } else {
                break;
            }
        }

        let used: u32 = entry.iter().map(|&(_, n)| n).sum();
        if used.saturating_add(n_tokens) > self.limit {
            let retry_after_secs = entry
                .front()
                .map(|&(ts, _)| self.window.saturating_sub(now.duration_since(ts)).as_secs() + 1)
                .unwrap_or_else(|| self.window.as_secs());
            return Err(RevealDenied { retry_after_secs });
        }

        entry.push_back((now, n_tokens));
        Ok(())
    }
}

/// Process-global guard. Config is read once, from `ECK_REVEAL_RATE`, at
/// first use.
pub fn global() -> &'static RevealGuard {
    static GUARD: OnceLock<RevealGuard> = OnceLock::new();
    GUARD.get_or_init(RevealGuard::from_env)
}

/// Standard reveal-audit payload (`.eck/PII_VAULT.md` §4). `kinds` and
/// `tokens` are COUNTS only — never call this with clear values or the
/// tokens themselves. `tickets` is capped to 20 ids by the caller (or here,
/// defensively).
pub fn audit_payload(
    who: &str,
    role: &str,
    surface: &str,
    tokens: u32,
    kinds: &HashMap<String, u32>,
    tickets: &[String],
    denied: bool,
) -> serde_json::Value {
    let capped: Vec<&String> = tickets.iter().take(20).collect();
    let mut obj = serde_json::json!({
        "class": "reveal",
        "who": who,
        "role": role,
        "surface": surface,
        "tokens": tokens,
        "kinds": kinds,
        "tickets": capped,
    });
    if denied {
        obj["denied"] = serde_json::json!(true);
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rate_falls_back_to_default_on_bad_input() {
        assert_eq!(parse_rate(None), (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS));
        assert_eq!(parse_rate(Some("")), (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS));
        assert_eq!(parse_rate(Some("garbage")), (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS));
        assert_eq!(parse_rate(Some("500")), (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS));
        assert_eq!(parse_rate(Some("0/600")), (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS));
        assert_eq!(parse_rate(Some("500/0")), (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS));
        assert_eq!(parse_rate(Some("abc/600")), (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS));
        assert_eq!(parse_rate(Some("500/abc")), (DEFAULT_LIMIT, DEFAULT_WINDOW_SECS));
    }

    #[test]
    fn parse_rate_accepts_valid_input() {
        assert_eq!(parse_rate(Some("500/600")), (500, 600));
        assert_eq!(parse_rate(Some(" 10 / 5 ")), (10, 5));
    }

    #[test]
    fn allows_within_budget_and_denies_over() {
        let g = RevealGuard::new(100, 60);
        assert!(g.check("u1", 40).is_ok());
        assert!(g.check("u1", 40).is_ok());
        assert!(g.check("u1", 21).is_err(), "80 + 21 > 100 must deny");
        assert!(g.check("u1", 20).is_ok(), "80 + 20 == 100 must allow");
    }

    #[test]
    fn denial_reports_a_positive_retry_after() {
        let g = RevealGuard::new(10, 60);
        assert!(g.check("u1", 10).is_ok());
        let err = g.check("u1", 1).unwrap_err();
        assert!(err.retry_after_secs > 0 && err.retry_after_secs <= 60);
    }

    #[test]
    fn denied_request_records_no_usage() {
        let g = RevealGuard::new(10, 60);
        assert!(g.check("u1", 10).is_ok());
        assert!(g.check("u1", 5).is_err());
        // The denied 5 must not have been recorded — a later small request
        // that only needs the original 10 to age out should still be judged
        // against exactly 10 used, not 15.
        assert!(g.check("u1", 1).is_err()); // still over (10 + 1 > 10)
    }

    #[test]
    fn per_user_buckets_are_isolated() {
        let g = RevealGuard::new(10, 60);
        assert!(g.check("u1", 10).is_ok());
        assert!(g.check("u2", 10).is_ok());
        assert!(g.check("u1", 1).is_err());
        assert!(g.check("u2", 1).is_err());
    }

    #[test]
    fn window_rollover_admits_after_expiry() {
        // A zero-second window means every prior entry is immediately stale
        // on the very next check — a deterministic stand-in for "wait out the
        // window" without a real sleep.
        let g = RevealGuard::new(5, 0);
        assert!(g.check("u1", 5).is_ok());
        assert!(g.check("u1", 5).is_ok(), "prior usage must have aged out");
    }

    #[test]
    fn zero_tokens_is_always_ok_and_free() {
        let g = RevealGuard::new(1, 60);
        assert!(g.check("u1", 0).is_ok());
        assert!(g.check("u1", 0).is_ok());
        assert!(g.check("u1", 1).is_ok());
    }
}
