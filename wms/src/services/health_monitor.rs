//! Self-diagnosis monitor: samples the runtime loop counters
//! ([`eck_core::metrics`]) together with process CPU / RSS at a fixed interval
//! and keeps a rolling in-memory history. Surfaced at `GET /api/health/deep`.
//!
//! The point is to make runaway loops and data-driven blow-ups *visible*:
//! - `now` shows live CPU% / RSS / thread count;
//! - `recent_rates` shows how fast each instrumented loop fired in the last
//!   interval — a hot loop stands out as one counter with a huge rate;
//! - `suspect` names the loop with the highest recent rate when CPU is high;
//! - `history` lets you see *when* a spike began (which deploy / data caused it).
//!
//! CPU/RSS/threads are read from `/proc/self/*` on Linux (the prod nodes and the
//! kiosk); on other platforms those fields are `null` but the counters — the
//! part that actually localizes a loop — work everywhere.

use axum::Json;
use eck_core::metrics::{self, NAMES, N};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SAMPLE_SECS: u64 = 15;
const HISTORY: usize = 240; // 240 * 15s ≈ 1 hour
/// Heuristic: flag a `suspect` only when the process is meaningfully busy.
const BUSY_PCT_ONE_CORE: f64 = 40.0;

#[derive(Clone)]
struct Sample {
    ts: u64,
    cpu_pct_one_core: Option<f64>,
    rss_mb: Option<u64>,
    threads: Option<u64>,
    /// Per-counter delta over the sample interval, index-aligned with NAMES.
    rates: Vec<u64>,
}

static HIST: OnceLock<Mutex<VecDeque<Sample>>> = OnceLock::new();
static STARTED_AT: OnceLock<u64> = OnceLock::new();

fn hist() -> &'static Mutex<VecDeque<Sample>> {
    HIST.get_or_init(|| Mutex::new(VecDeque::with_capacity(HISTORY)))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// CLK_TCK — 100 on every mainstream Linux build; used to turn jiffies into %.
const CLK_TCK: f64 = 100.0;

/// Returns `(utime+stime jiffies, num_threads)` from `/proc/self/stat`.
#[cfg(target_os = "linux")]
fn read_cpu_threads() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    // `comm` (field 2) is wrapped in parens and may contain spaces; split after
    // the last ')' so positional indexing of the numeric tail is reliable.
    let after = &s[s.rfind(')')? + 1..];
    let f: Vec<&str> = after.split_whitespace().collect();
    // After ')' the next token is field 3 (state). utime=14, stime=15,
    // num_threads=20 → offsets 11, 12, 17 into `f`.
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    let threads: u64 = f.get(17)?.parse().ok()?;
    Some((utime + stime, threads))
}

#[cfg(target_os = "linux")]
fn read_rss_mb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident_pages.saturating_mul(4096) / (1024 * 1024))
}

#[cfg(not(target_os = "linux"))]
fn read_cpu_threads() -> Option<(u64, u64)> {
    None
}
#[cfg(not(target_os = "linux"))]
fn read_rss_mb() -> Option<u64> {
    None
}

/// Background sampler. Spawn once at startup.
pub async fn start_health_monitor() {
    let _ = *STARTED_AT.get_or_init(now_secs);
    let mut prev_counters = metrics::snapshot();
    let mut prev_jiffies = read_cpu_threads().map(|(j, _)| j);
    let mut last = Instant::now();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(SAMPLE_SECS));
    interval.tick().await; // consume the immediate first tick
    tracing::info!("[HealthMonitor] started (sampling every {SAMPLE_SECS}s, history ~1h)");

    loop {
        interval.tick().await;

        let now_counters = metrics::snapshot();
        let mut rates = vec![0u64; N];
        for i in 0..N {
            rates[i] = now_counters[i].saturating_sub(prev_counters[i]);
        }
        prev_counters = now_counters;

        let elapsed = last.elapsed().as_secs_f64().max(0.001);
        last = Instant::now();

        let (cpu_pct, threads) = match read_cpu_threads() {
            Some((jiffies, th)) => {
                let pct = prev_jiffies.map(|p| {
                    let d = jiffies.saturating_sub(p) as f64;
                    ((d / CLK_TCK / elapsed) * 100.0 * 10.0).round() / 10.0
                });
                prev_jiffies = Some(jiffies);
                (pct, Some(th))
            }
            None => (None, None),
        };

        let sample = Sample {
            ts: now_secs(),
            cpu_pct_one_core: cpu_pct,
            rss_mb: read_rss_mb(),
            threads,
            rates,
        };

        if let Ok(mut h) = hist().lock() {
            if h.len() >= HISTORY {
                h.pop_front();
            }
            h.push_back(sample);
        }
    }
}

fn rates_to_obj(rates: &[u64]) -> Value {
    let mut m = Map::new();
    for (i, name) in NAMES.iter().enumerate() {
        m.insert((*name).to_string(), json!(rates.get(i).copied().unwrap_or(0)));
    }
    Value::Object(m)
}

/// Build the `/api/health/deep` payload from the rolling history + counter totals.
pub fn report() -> Value {
    let totals = metrics::snapshot();
    let mut totals_obj = Map::new();
    for (i, name) in NAMES.iter().enumerate() {
        totals_obj.insert((*name).to_string(), json!(totals[i]));
    }

    let guard = hist().lock().ok();
    let samples: Vec<Sample> = guard
        .as_ref()
        .map(|h| h.iter().cloned().collect())
        .unwrap_or_default();
    let latest = samples.last().cloned();

    // suspect = highest recent rate, but only worth surfacing when busy.
    let mut suspect = Value::Null;
    if let Some(s) = &latest {
        let busy = s.cpu_pct_one_core.map(|c| c >= BUSY_PCT_ONE_CORE).unwrap_or(false);
        if let Some((idx, &rate)) = s.rates.iter().enumerate().max_by_key(|(_, r)| **r) {
            if rate > 0 && (busy || s.cpu_pct_one_core.is_none()) {
                suspect = json!({
                    "loop": NAMES.get(idx).copied().unwrap_or("?"),
                    "rate_per_interval": rate,
                    "per_sec": ((rate as f64 / SAMPLE_SECS as f64) * 10.0).round() / 10.0,
                });
            }
        }
    }

    let history: Vec<Value> = samples
        .iter()
        .map(|s| {
            json!({
                "ts": s.ts,
                "cpu_pct_one_core": s.cpu_pct_one_core,
                "rss_mb": s.rss_mb,
                "threads": s.threads,
                "rates": rates_to_obj(&s.rates),
            })
        })
        .collect();

    json!({
        "uptime_s": now_secs().saturating_sub(*STARTED_AT.get_or_init(now_secs)),
        "sample_interval_s": SAMPLE_SECS,
        "now": latest.as_ref().map(|s| json!({
            "ts": s.ts,
            "cpu_pct_one_core": s.cpu_pct_one_core,
            "rss_mb": s.rss_mb,
            "threads": s.threads,
            "rates": rates_to_obj(&s.rates),
        })).unwrap_or(Value::Null),
        "suspect": suspect,
        "counters_total": Value::Object(totals_obj),
        "history": history,
    })
}

/// Compact view (no history) for embedding into the existing ops `system_health`
/// response, so the regular health verb flags a runaway loop at a glance.
pub fn report_compact() -> Value {
    let mut v = report();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("history");
    }
    v
}

/// `GET /X/ops/loop_metrics` handler (service-token gated).
pub async fn deep_health_handler() -> Json<Value> {
    Json(report())
}
