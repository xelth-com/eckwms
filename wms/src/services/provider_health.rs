//! Deterministic scraper/provider outage watchdog over `sync_history`.
//!
//! The 2026-09 incident: Zoho login was dead for 18 days and the only trace was
//! an hourly `warn!` line nobody read. This turns those silent failures into the
//! SAME dashboard alerts the AI Observer raises — deduplicated per provider,
//! auto-escalating warning→critical, and self-clearing the moment a run
//! succeeds — via the shared [`crate::services::alerts`] sink.
//!
//! The decision logic is a PURE function ([`evaluate`]) so it is unit-testable
//! without a DB: it takes a compact [`ProviderState`] for ONE provider (its
//! newest row, the time of its last success, and how many failures have piled up
//! since), the current time, and that provider's expected cadence, and returns a
//! [`Verdict`].
//!
//! Self-configuring: a provider is *watched* only once it has at least one
//! successful run in the last 30 days, so a node that never ran (say) Exact
//! Online never alerts about it.
//!
//! The DB-facing [`check`] builds each [`ProviderState`] from small, BOUNDED
//! per-provider queries (newest row / newest success / failures-since) rather
//! than one `LIMIT 400` window. A single window evicts the last success once an
//! outage runs longer than the window holds (~5 days at the kiosk's ~72 rows/day)
//! — the provider silently becomes "unwatched" and the alert vanishes, which is
//! exactly the 18-day outage class this watchdog exists for.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use eck_core::db::SurrealDb;
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::warn;

/// How far back a success must appear for a provider to be "watched".
const WATCH_WINDOW_DAYS: i64 = 30;

/// Bound on the failures-since-last-success count query (a number this large
/// already means a long outage; the exact count past it does not matter).
const MAX_CONSECUTIVE_SCAN: usize = 100;

/// Compact per-provider view the watchdog reasons over. Built from bounded
/// per-provider `sync_history` queries in [`check`] (or, in tests, from a row
/// slice) so the pure [`evaluate`] never has to see the raw rows.
#[derive(Debug, Clone, Default)]
pub struct ProviderState {
    /// Newest row overall, as `(status, error_detail)`. `None` if the provider
    /// has no `sync_history` rows at all.
    pub newest: Option<(String, String)>,
    /// `completed_at` of the newest successful run (`ok`/`partial`), parsed.
    /// `None` means the provider has never succeeded (→ never watched).
    pub last_success_at: Option<DateTime<Utc>>,
    /// Failure rows newer than the last success, counted up to a bound.
    pub consecutive_failures: u32,
}

impl ProviderState {
    /// True once this provider has succeeded within the watch window.
    pub fn is_watched(&self, now: DateTime<Utc>) -> bool {
        self.last_success_at
            .map(|t| t >= now - Duration::days(WATCH_WINDOW_DAYS))
            .unwrap_or(false)
    }

    /// True when the newest row is a success — i.e. the scraper/provider
    /// demonstrably answered on its most recent attempt.
    pub fn newest_is_success(&self) -> bool {
        self.newest.as_ref().map(|(s, _)| is_success(s)).unwrap_or(false)
    }
}

/// Outcome of evaluating one provider's recent history.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Not watched, or the newest run succeeded, or too recent to alert yet.
    Healthy,
    Failing {
        /// Time since the last successful run (None only in the degenerate
        /// never-succeeded-but-failing case, which watched-gating prevents).
        since_last_success: Option<Duration>,
        consecutive_failures: u32,
        last_error: String,
        /// No success row at all in the window. Watched-gating means an emitted
        /// `Failing` always has this `false`; kept for message symmetry.
        never_succeeded: bool,
    },
}

/// A `sync_history` status that means "the run reached the provider and
/// imported". `partial` counts as success: the connection worked, only some
/// per-record imports failed — treating it as an outage would be a false alarm.
fn is_success(status: &str) -> bool {
    status == "ok" || status == "partial"
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&Utc))
}

/// Byte-safe truncation (a multi-byte error body must never panic). Mirrors
/// `scheduler::safe_trunc`; duplicated here to keep the watchdog self-contained.
fn trunc(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Pure decision core. Reasons over one provider's compact [`ProviderState`].
/// `cadence` is the provider's expected run interval. See module docs.
pub fn evaluate(state: &ProviderState, now: DateTime<Utc>, cadence: Duration) -> Verdict {
    // Unwatched (never succeeded, or last success older than the watch window)
    // → nothing to alert about.
    if !state.is_watched(now) {
        return Verdict::Healthy;
    }

    // The newest run decides the current state.
    let (status, error) = match &state.newest {
        Some(n) => n,
        None => return Verdict::Healthy, // watched implies a success row exists, but be safe
    };
    if is_success(status) {
        return Verdict::Healthy;
    }

    let since_last_success = state.last_success_at.map(|t| now - t);

    // Only alert once the outage has outlasted two expected cadences —
    // a single missed hourly run is not yet an outage.
    match since_last_success {
        Some(d) if d > cadence * 2 => {}
        _ => return Verdict::Healthy,
    }

    Verdict::Failing {
        since_last_success,
        consecutive_failures: state.consecutive_failures,
        last_error: error.clone(),
        never_succeeded: false,
    }
}

// ── Presentation helpers ──────────────────────────────────────────────────

fn display_name(provider: &str) -> &str {
    match provider {
        "zoho_desk" => "Zoho Desk",
        "opal" => "OPAL",
        "dhl" => "DHL",
        "exact_online" => "Exact Online",
        "excel" => "Excel",
        other => other,
    }
}

/// Human duration for titles, e.g. "18 h" or "3 d".
fn humanize(d: Duration) -> String {
    let hours = d.num_hours();
    if hours >= 48 {
        format!("{} d", d.num_days())
    } else {
        format!("{} h", hours.max(0))
    }
}

/// Severity from how long the outage has run relative to cadence:
/// warning above 2×, critical above 6×.
fn severity_for(since: Option<Duration>, cadence: Duration) -> &'static str {
    match since {
        Some(d) if d > cadence * 6 => "critical",
        _ => "warning",
    }
}

/// Provider-specific remediation hint appended to the alert message.
fn hint_for(provider: &str, last_error: &str) -> Option<String> {
    let err = last_error.to_lowercase();
    match provider {
        "zoho_desk" if err.contains("login") => Some(
            "Zoho login failed: the 60-day password policy or a rotated password — \
             check config.env ZOHO_PASSWORD"
                .to_string(),
        ),
        _ => None,
    }
}

fn last_success_str(since: Option<Duration>, now: DateTime<Utc>) -> String {
    match since {
        Some(d) => (now - d).to_rfc3339(),
        None => "never".to_string(),
    }
}

// ── DB-facing watchdog tick ────────────────────────────────────────────────

/// Expected run interval per watched provider.
fn cadence(provider: &str) -> Duration {
    match provider {
        "exact_online" | "excel" => Duration::hours(24),
        _ => Duration::hours(1), // zoho_desk, opal, dhl
    }
}

const HOURLY: [&str; 3] = ["zoho_desk", "opal", "dhl"];
const DAILY: [&str; 2] = ["exact_online", "excel"];

/// Build one provider's [`ProviderState`] from bounded queries. Any
/// unparsable `completed_at` string encountered is pushed onto `bad_ts` for a
/// single per-pass `warn!` by the caller (A3).
async fn load_state(db: &SurrealDb, provider: &str, bad_ts: &mut Vec<String>) -> ProviderState {
    // (1) Newest row overall — decides the current state.
    let newest_row: Option<Value> = db
        .query("SELECT status, completed_at, error_detail FROM sync_history WHERE provider = $p ORDER BY completed_at DESC LIMIT 1")
        .bind(("p", provider.to_string()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();

    let newest = newest_row.as_ref().map(|v| {
        (
            v.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            v.get("error_detail").and_then(|e| e.as_str()).unwrap_or("").to_string(),
        )
    });
    if let Some(v) = &newest_row {
        if let Some(ca) = v.get("completed_at").and_then(|c| c.as_str()) {
            if !ca.is_empty() && parse_ts(ca).is_none() {
                bad_ts.push(ca.to_string());
            }
        }
    }

    // (2) Newest success — the watched gate and the "since" clock.
    let success_row: Option<Value> = db
        .query("SELECT completed_at FROM sync_history WHERE provider = $p AND status IN ['ok','partial'] ORDER BY completed_at DESC LIMIT 1")
        .bind(("p", provider.to_string()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();
    let last_success_str = success_row
        .as_ref()
        .and_then(|v| v.get("completed_at").and_then(|c| c.as_str()))
        .map(|s| s.to_string());
    let last_success_at = last_success_str.as_deref().and_then(parse_ts);
    if let Some(s) = &last_success_str {
        if !s.is_empty() && last_success_at.is_none() {
            bad_ts.push(s.clone());
        }
    }

    // (3) Failures since that success — every row newer than the last success
    // is by construction a non-success, so counting them (bounded) is the
    // consecutive-failure streak. String compare is sound: every row is written
    // with `Utc::now().to_rfc3339()`, so lexical order == chronological order.
    let consecutive_failures = match &last_success_str {
        Some(since) => {
            let rows: Vec<Value> = db
                .query("SELECT completed_at FROM sync_history WHERE provider = $p AND completed_at > $since ORDER BY completed_at DESC LIMIT $lim")
                .bind(("p", provider.to_string()))
                .bind(("since", since.clone()))
                .bind(("lim", MAX_CONSECUTIVE_SCAN as i64))
                .await
                .ok()
                .and_then(|mut r| r.take(0).ok())
                .unwrap_or_default();
            rows.len() as u32
        }
        None => 0,
    };

    ProviderState { newest, last_success_at, consecutive_failures }
}

/// Run one watchdog pass: for each watched provider read its bounded recent
/// history, evaluate it, and raise/resolve the matching dashboard alerts.
pub async fn check(db: &SurrealDb, ws: Option<&broadcast::Sender<String>>) {
    let now = Utc::now();
    let mut bad_ts: Vec<String> = Vec::new();

    let providers: Vec<&str> = HOURLY.iter().chain(DAILY.iter()).copied().collect();
    let mut states: HashMap<&str, ProviderState> = HashMap::new();
    for p in &providers {
        states.insert(p, load_state(db, p, &mut bad_ts).await);
    }

    // A3: one loud line per pass for unparsable timestamps instead of silently
    // dropping them (they would otherwise skew the "since last success" clock).
    if !bad_ts.is_empty() {
        warn!(
            "[provider_health] {} unparsable completed_at value(s) this pass; first = {:?}",
            bad_ts.len(),
            bad_ts.first()
        );
    }

    let empty = ProviderState::default();
    let verdicts: HashMap<&str, Verdict> = providers
        .iter()
        .map(|p| (*p, evaluate(states.get(p).unwrap_or(&empty), now, cadence(p))))
        .collect();

    // ── Scraper-process consolidation ──
    // If EVERY watched hourly provider is failing AND they all blame an
    // unreachable scraper, the Node/Bun process is down — one alert, not three.
    let watched_hourly: Vec<&str> = HOURLY
        .iter()
        .copied()
        .filter(|p| states.get(*p).unwrap_or(&empty).is_watched(now))
        .collect();

    let all_hourly_failing = !watched_hourly.is_empty()
        && watched_hourly.iter().all(|p| matches!(verdicts.get(p), Some(Verdict::Failing { .. })));
    let all_blame_scraper = watched_hourly.iter().all(|p| match verdicts.get(p) {
        Some(Verdict::Failing { last_error, .. }) => last_error.to_lowercase().contains("scraper unreachable"),
        _ => false,
    });
    let scraper_down = all_hourly_failing && all_blame_scraper;

    if scraper_down {
        // Worst severity across the failing hourly providers.
        let severity = if watched_hourly.iter().any(|p| matches!(
            verdicts.get(p),
            Some(Verdict::Failing { since_last_success, .. }) if since_last_success.map(|d| d > cadence(p) * 6).unwrap_or(false)
        )) {
            "critical"
        } else {
            "warning"
        };
        let providers_disp: Vec<&str> = watched_hourly.iter().map(|p| display_name(p)).collect();
        let message = format!(
            "No hourly provider ({}) has reached its scraper. The Node/Bun scraper on this node \
             is down; the scheduler respawns it hourly — check `POST /api/scraper/start` and \
             scraper/scraper.err.log.",
            providers_disp.join(", ")
        );
        super::alerts::raise(db, ws, "scraper:process", severity, "Scraper process unreachable", &message).await;
        // A consolidated alert owns these — clear any stale per-provider rows.
        for p in &HOURLY {
            super::alerts::resolve(db, ws, &format!("provider:{}", p), "consolidated into scraper:process").await;
        }
    } else {
        // A2: only clear `scraper:process` when at least one hourly provider's
        // NEWEST row actually succeeded (the scraper demonstrably answered).
        // Otherwise leave it as-is: the scheduler raises this alert the moment
        // `ensure_scraper_running()` fails, and the providers may simply not be
        // 2× cadence stale yet — clearing it here would flap the banner off.
        let scraper_answered = HOURLY
            .iter()
            .any(|p| states.get(*p).unwrap_or(&empty).newest_is_success());
        if scraper_answered {
            super::alerts::resolve(db, ws, "scraper:process", "recovered: a scraper-backed provider reached its endpoint").await;
        }
    }

    // ── Per-provider alerts ──
    // Hourly providers only emit individually when NOT consolidated above.
    let per_provider: Vec<&str> = if scraper_down {
        DAILY.to_vec()
    } else {
        HOURLY.iter().chain(DAILY.iter()).copied().collect()
    };

    for p in per_provider {
        let key = format!("provider:{}", p);
        match verdicts.get(p) {
            Some(Verdict::Failing { since_last_success, consecutive_failures, last_error, never_succeeded }) => {
                let sev = severity_for(*since_last_success, cadence(p));
                let title = if *never_succeeded {
                    format!("{} import has never succeeded", display_name(p))
                } else {
                    match since_last_success {
                        Some(d) => format!("{} import failing for {}", display_name(p), humanize(*d)),
                        None => format!("{} import failing", display_name(p)),
                    }
                };
                let mut message = format!(
                    "Consecutive failures: {}. Last success: {}. Last error: {}",
                    consecutive_failures,
                    last_success_str(*since_last_success, now),
                    trunc(last_error, 300),
                );
                if let Some(hint) = hint_for(p, last_error) {
                    message.push_str("\n\n");
                    message.push_str(&hint);
                }
                super::alerts::raise(db, ws, &key, sev, &title, &message).await;
            }
            _ => {
                // Healthy — only clear if this provider is actually watched (an
                // unwatched provider never had an alert to clear).
                let state = states.get(p).unwrap_or(&empty);
                if state.is_watched(now) {
                    let last_ok = state
                        .last_success_at
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_else(|| now.to_rfc3339());
                    super::alerts::resolve(db, ws, &key, &format!("recovered: last run succeeded at {}", last_ok)).await;
                }
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// One `sync_history` row, reduced to the fields the watchdog reasons over.
    #[derive(Debug, Clone)]
    struct HistoryRow {
        status: String,
        completed_at: String,
        error_detail: String,
    }

    fn row(status: &str, completed_at: &str, err: &str) -> HistoryRow {
        HistoryRow {
            status: status.to_string(),
            completed_at: completed_at.to_string(),
            error_detail: err.to_string(),
        }
    }

    /// Derive a [`ProviderState`] from a row slice exactly as the bounded DB
    /// queries in `check` would: newest by lexical `completed_at` (== the
    /// `ORDER BY completed_at DESC LIMIT 1`), newest success, and the count of
    /// rows newer than that success.
    fn state_from_rows(rows: &[HistoryRow]) -> ProviderState {
        let mut sorted: Vec<&HistoryRow> = rows.iter().collect();
        sorted.sort_by(|a, b| b.completed_at.cmp(&a.completed_at));
        let newest = sorted.first().map(|r| (r.status.clone(), r.error_detail.clone()));
        let last_success_str = sorted
            .iter()
            .filter(|r| is_success(&r.status))
            .map(|r| r.completed_at.clone())
            .max();
        let last_success_at = last_success_str.as_deref().and_then(parse_ts);
        let consecutive_failures = match &last_success_str {
            Some(since) => sorted.iter().filter(|r| r.completed_at.as_str() > since.as_str()).count() as u32,
            None => 0,
        };
        ProviderState { newest, last_success_at, consecutive_failures }
    }

    fn eval(rows: &[HistoryRow], now: DateTime<Utc>, cadence: Duration) -> Verdict {
        evaluate(&state_from_rows(rows), now, cadence)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-30T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    /// Newest run succeeded → healthy, even with older failures.
    #[test]
    fn healthy_when_latest_succeeds() {
        let rows = vec![
            row("ok", "2026-09-30T11:00:00Z", ""),
            row("error", "2026-09-30T10:00:00Z", "boom"),
        ];
        assert_eq!(eval(&rows, now(), Duration::hours(1)), Verdict::Healthy);
    }

    /// Failing > 2× but <= 6× cadence → warning severity.
    #[test]
    fn failing_warning() {
        // Last success 3 h ago (> 2×1h, <= 6×1h), then failures.
        let rows = vec![
            row("error", "2026-09-30T11:30:00Z", "scraper unreachable: connect"),
            row("error", "2026-09-30T10:30:00Z", "scraper unreachable: connect"),
            row("ok", "2026-09-30T09:00:00Z", ""),
        ];
        match eval(&rows, now(), Duration::hours(1)) {
            Verdict::Failing { since_last_success, consecutive_failures, .. } => {
                let d = since_last_success.unwrap();
                assert!(d > Duration::hours(2) && d <= Duration::hours(6), "since={:?}", d);
                assert_eq!(consecutive_failures, 2);
                assert_eq!(severity_for(since_last_success, Duration::hours(1)), "warning");
            }
            v => panic!("expected Failing, got {:?}", v),
        }
    }

    /// Failing > 6× cadence → critical severity.
    #[test]
    fn failing_critical() {
        // Last success 18 h ago.
        let rows = vec![
            row("error", "2026-09-30T11:00:00Z", "HTTP 401 invalid login"),
            row("ok", "2026-09-29T18:00:00Z", ""),
        ];
        match eval(&rows, now(), Duration::hours(1)) {
            Verdict::Failing { since_last_success, .. } => {
                assert_eq!(severity_for(since_last_success, Duration::hours(1)), "critical");
            }
            v => panic!("expected Failing, got {:?}", v),
        }
    }

    /// Never succeeded in the window → not watched → healthy (no alert).
    #[test]
    fn never_succeeded_not_watched() {
        let rows = vec![
            row("error", "2026-09-30T11:00:00Z", "boom"),
            row("error", "2026-09-30T10:00:00Z", "boom"),
        ];
        assert_eq!(eval(&rows, now(), Duration::hours(1)), Verdict::Healthy);
    }

    /// A single missed run (within 2× cadence) is not yet an outage.
    #[test]
    fn recent_failure_not_yet_failing() {
        let rows = vec![
            row("error", "2026-09-30T11:30:00Z", "boom"),
            row("ok", "2026-09-30T11:00:00Z", ""), // 1 h ago, <= 2×1h
        ];
        assert_eq!(eval(&rows, now(), Duration::hours(1)), Verdict::Healthy);
    }

    /// Unparsable timestamps are tolerated (no panic); a parsable success still
    /// makes the provider watched and the outage still reads as failing.
    #[test]
    fn tolerates_unparsable_timestamps() {
        let rows = vec![
            row("error", "not-a-date", "garbage ts"),
            row("error", "2026-09-30T11:00:00Z", "scraper unreachable: connect"),
            row("ok", "2026-09-29T09:00:00Z", ""),
        ];
        match eval(&rows, now(), Duration::hours(1)) {
            Verdict::Failing { .. } => {}
            v => panic!("expected Failing, got {:?}", v),
        }
    }

    /// The regression the LIMIT 400 window caused: an outage LONGER than the old
    /// window (18 days ≈ 430 hourly failure rows > 400) used to evict the last
    /// success row, un-watch the provider, and silently drop the alert. With
    /// bounded per-provider queries the success at t-18d is still visible, the
    /// provider stays watched, and it still alerts (critical).
    #[test]
    fn outage_longer_than_old_window_still_alerts() {
        let last_ok = now() - Duration::days(18);
        let state = ProviderState {
            newest: Some(("error".to_string(), "HTTP 401 invalid login".to_string())),
            last_success_at: Some(last_ok),
            consecutive_failures: 430,
        };
        match evaluate(&state, now(), Duration::hours(1)) {
            Verdict::Failing { since_last_success, consecutive_failures, last_error, .. } => {
                assert_eq!(consecutive_failures, 430);
                assert!(since_last_success.unwrap() > Duration::days(17));
                assert_eq!(severity_for(since_last_success, Duration::hours(1)), "critical");
                assert!(last_error.contains("401"));
            }
            v => panic!("expected Failing, got {:?}", v),
        }
    }
}
