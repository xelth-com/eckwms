use eck_core::db::SurrealDb;
use serde_json::Value;
use std::sync::atomic::{AtomicU8, Ordering};
use tracing::{info, warn};

// ── Budget tiers (tokens per rolling window) ───────────────────────────────
// Calibrated after the 2026-04-21 loop incident: peak legitimate summarization
// is ~3M tokens/hour during bulk scraper imports. Anything meaningfully above
// that is either a loop or a runaway batch — throttle fast, halt hard.
// Hourly limits — fast reaction to sudden spikes
const HOURLY_WARN: i64     =  3_000_000;   //  3M tokens/h → alert
const HOURLY_THROTTLE: i64 =  6_000_000;   //  6M tokens/h → rate-limit
const HOURLY_HALT: i64     = 15_000_000;   // 15M tokens/h → full stop

// Daily limits — overall budget protection
const DAILY_WARN: i64      =  20_000_000;  //  20M tokens/day → alert
const DAILY_THROTTLE: i64  =  40_000_000;  //  40M tokens/day → rate-limit
const DAILY_HALT: i64      =  80_000_000;  //  80M tokens/day → full stop

/// Throttle delay: workers sleep this long between calls when in THROTTLE mode
pub const THROTTLE_DELAY_SECS: u64 = 60;

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum BudgetLevel {
    /// Normal operation
    Normal = 0,
    /// Approaching budget — alert sent, work continues at normal speed
    Warn = 1,
    /// Over budget — work continues but rate-limited (1 call/min)
    Throttle = 2,
    /// Hard budget exceeded — all AI work stops
    Halt = 3,
}

impl BudgetLevel {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Normal,
            1 => Self::Warn,
            2 => Self::Throttle,
            3 => Self::Halt,
            _ => Self::Halt,
        }
    }
}

impl std::fmt::Display for BudgetLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "normal"),
            Self::Warn => write!(f, "warn"),
            Self::Throttle => write!(f, "THROTTLE"),
            Self::Halt => write!(f, "HALT"),
        }
    }
}

/// Global atomic budget level — updated by the observer, read by workers.
/// Using atomic so workers can check it without DB queries on every call.
static BUDGET_LEVEL: AtomicU8 = AtomicU8::new(0);

/// Read the current budget level (lock-free, called by workers before each API call).
pub fn current_budget_level() -> BudgetLevel {
    BudgetLevel::from_u8(BUDGET_LEVEL.load(Ordering::Relaxed))
}

/// Evaluate token spending against budget tiers and update the global level.
/// Returns (level, hourly_tokens, daily_tokens) for logging/alerting.
pub async fn evaluate_budget(db: &SurrealDb) -> anyhow::Result<(BudgetLevel, i64, i64)> {
    // Query hourly and daily totals in one roundtrip
    let mut response = db
        .query(
            "SELECT math::sum(total_tokens) AS tokens FROM ai_telemetry WHERE timestamp > time::now() - 1h GROUP ALL; \
             SELECT math::sum(total_tokens) AS tokens FROM ai_telemetry WHERE timestamp > time::now() - 24h GROUP ALL;"
        )
        .await?;

    let hourly: Vec<Value> = response.take(0)?;
    let daily: Vec<Value> = response.take(1)?;

    let hourly_tokens = hourly.first()
        .and_then(|r| r.get("tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let daily_tokens = daily.first()
        .and_then(|r| r.get("tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Determine level — highest triggered tier wins
    let level = if hourly_tokens >= HOURLY_HALT || daily_tokens >= DAILY_HALT {
        BudgetLevel::Halt
    } else if hourly_tokens >= HOURLY_THROTTLE || daily_tokens >= DAILY_THROTTLE {
        BudgetLevel::Throttle
    } else if hourly_tokens >= HOURLY_WARN || daily_tokens >= DAILY_WARN {
        BudgetLevel::Warn
    } else {
        BudgetLevel::Normal
    };

    let prev = BudgetLevel::from_u8(BUDGET_LEVEL.swap(level as u8, Ordering::Relaxed));

    // Log transitions
    if level != prev {
        match level {
            BudgetLevel::Normal => info!(
                "[Budget] Back to normal (hourly: {}K, daily: {}K)",
                hourly_tokens / 1000, daily_tokens / 1000
            ),
            BudgetLevel::Warn => warn!(
                "[Budget] WARNING — approaching limit (hourly: {}K, daily: {}K)",
                hourly_tokens / 1000, daily_tokens / 1000
            ),
            BudgetLevel::Throttle => warn!(
                "[Budget] THROTTLE — rate-limiting AI workers (hourly: {}K, daily: {}K)",
                hourly_tokens / 1000, daily_tokens / 1000
            ),
            BudgetLevel::Halt => warn!(
                "[Budget] HALT — stopping all AI workers (hourly: {}K, daily: {}K)",
                hourly_tokens / 1000, daily_tokens / 1000
            ),
        }
    }

    Ok((level, hourly_tokens, daily_tokens))
}

/// Log AI API usage to the `ai_telemetry` table for cost tracking.
pub async fn log_telemetry(
    db: &SurrealDb,
    module: &str,
    model: &str,
    entity_id: &str,
    usage: &Value,
) {
    let prompt_tokens = usage.get("promptTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let candidates_tokens = usage.get("candidatesTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let total_tokens = usage.get("totalTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);

    let result = db
        .query(
            "CREATE ai_telemetry CONTENT {
                timestamp: time::now(),
                module: $module,
                model: $model,
                entity_id: $entity_id,
                prompt_tokens: $prompt_tokens,
                candidates_tokens: $candidates_tokens,
                total_tokens: $total_tokens
            }",
        )
        .bind(("module", module.to_string()))
        .bind(("model", model.to_string()))
        .bind(("entity_id", entity_id.to_string()))
        .bind(("prompt_tokens", prompt_tokens))
        .bind(("candidates_tokens", candidates_tokens))
        .bind(("total_tokens", total_tokens))
        .await;

    if let Err(e) = result {
        warn!("[Telemetry] Failed to log {module}/{entity_id}: {e}");
    }
}
