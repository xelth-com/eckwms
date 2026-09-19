//! Lazy-load + idle-unload holder for the heavy on-device models (feature
//! `local-embed`).
//!
//! Both the German NER (`local_ner`, ~709 MB mBERT) and the Jina embedder
//! (`local_embed`) are loaded by Candle's `VarBuilder::from_mmaped_safetensors`,
//! which `convert()`s the mmap into an anonymous heap copy that lives forever
//! behind a process-wide `static OnceCell`. On the 3.3 GiB kiosk that resident
//! copy was the memory hog behind the 2026-09-03 swap-thrash stall (2.7 G peak,
//! 955 M swap, 23 min unresponsive).
//!
//! [`IdleModel`] replaces the permanent `OnceCell` with a `RwLock<Option<Arc<T>>>`
//! plus a last-used timestamp. The model is loaded lazily on first use (on the
//! blocking pool — never a runtime worker), shared as an `Arc<T>` so an in-flight
//! call keeps its own model alive, and dropped by a cheap background reaper after
//! `idle_secs` of no use (`0` = never unload). Concurrent first-callers serialize
//! on a load gate so the weights are read exactly once.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::sync::{Mutex, RwLock};
use tracing::info;

/// Pure idle-unload decision, factored out so it is unit-testable without a
/// model or a clock. Returns `true` iff a model last touched at `last_used_ms`
/// should be dropped at `now_ms` given the `idle_secs` budget.
///
/// * `idle_secs == 0` → never unload (the disable knob).
/// * `last_used_ms == 0` → sentinel for "never loaded / already unloaded" →
///   never unload.
/// * otherwise unload once the idle gap reaches `idle_secs` whole seconds.
pub fn should_unload(last_used_ms: u64, now_ms: u64, idle_secs: u64) -> bool {
    if idle_secs == 0 || last_used_ms == 0 {
        return false;
    }
    now_ms.saturating_sub(last_used_ms) >= idle_secs.saturating_mul(1000)
}

/// Wall-clock millis since the UNIX epoch. Used only for coarse idle timing, so
/// a clock step of a few seconds is harmless.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A lazily-loaded, idle-unloadable holder for one heavy model `T`.
///
/// Lives in a `static` (via `OnceLock`), so `&self` is effectively `'static`;
/// the reaper task and the returned `Arc<T>` both need that.
pub struct IdleModel<T: Send + Sync + 'static> {
    slot: RwLock<Option<Arc<T>>>,
    /// Serializes the slow (load) path so the weights are read exactly once even
    /// under a thundering herd of first-callers.
    load_gate: Mutex<()>,
    /// Millis-since-epoch of the last successful `get()`; `0` before first load.
    last_used_ms: AtomicU64,
    /// One-shot guard so the reaper task is spawned exactly once.
    reaper_started: AtomicBool,
    label: &'static str,
    idle_secs: u64,
    /// Blocking loader (hf-hub download + safetensors mmap + candle convert).
    load: fn() -> Result<T>,
}

impl<T: Send + Sync + 'static> IdleModel<T> {
    /// Build a holder. `label` is the short tag used in the load/unload logs
    /// (e.g. `"NER"` → `"[NER] model loaded (…ms)"`). `idle_secs` is the
    /// no-use budget before unload (`0` = never).
    pub fn new(label: &'static str, idle_secs: u64, load: fn() -> Result<T>) -> Self {
        Self {
            slot: RwLock::new(None),
            load_gate: Mutex::new(()),
            last_used_ms: AtomicU64::new(0),
            reaper_started: AtomicBool::new(false),
            label,
            idle_secs,
            load,
        }
    }

    fn touch(&self) {
        self.last_used_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Return the model, loading it if the slot is empty. The load runs on the
    /// blocking pool and is serialized by `load_gate` so it happens once; the
    /// returned `Arc` keeps the model alive for the duration of the caller's use
    /// even if the reaper unloads the slot meanwhile.
    pub async fn get(&'static self) -> Result<Arc<T>> {
        // Fast path: already loaded.
        if let Some(m) = self.slot.read().await.as_ref() {
            self.touch();
            return Ok(m.clone());
        }
        // Slow path: single-flight the load behind the gate.
        let _gate = self.load_gate.lock().await;
        if let Some(m) = self.slot.read().await.as_ref() {
            // Another caller loaded it while we waited on the gate.
            self.touch();
            return Ok(m.clone());
        }
        let load = self.load;
        let label = self.label;
        let started = Instant::now();
        let model = tokio::task::spawn_blocking(move || load())
            .await
            .with_context(|| format!("[{label}] model load task panicked"))??;
        let arc = Arc::new(model);
        *self.slot.write().await = Some(arc.clone());
        self.touch();
        info!(
            "[{label}] model loaded ({}ms)",
            started.elapsed().as_millis()
        );
        self.ensure_reaper();
        Ok(arc)
    }

    /// Spawn the idle reaper once (no-op on later calls, and when unloading is
    /// disabled). Called from `get()` — i.e. always from within the runtime.
    fn ensure_reaper(&'static self) {
        if self.idle_secs == 0 {
            return;
        }
        if self
            .reaper_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return; // already running
        }
        // Check ~4×/budget, clamped to a sane [5s, 60s] cadence.
        let check = Duration::from_secs((self.idle_secs / 4).clamp(5, 60));
        let label = self.label;
        let idle_secs = self.idle_secs;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(check).await;
                if !should_unload(
                    self.last_used_ms.load(Ordering::Relaxed),
                    now_ms(),
                    idle_secs,
                ) {
                    continue;
                }
                // Take the write lock and re-check under it so we never drop a
                // model that was touched between the read above and here.
                let mut w = self.slot.write().await;
                if w.is_some()
                    && should_unload(
                        self.last_used_ms.load(Ordering::Relaxed),
                        now_ms(),
                        idle_secs,
                    )
                {
                    *w = None;
                    info!("[{label}] model unloaded after idle");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_unload_decision() {
        // Not yet elapsed → keep; exactly/over the budget → unload.
        assert!(!should_unload(10_000, 10_000 + 599_000, 600));
        assert!(should_unload(10_000, 10_000 + 600_000, 600));
        assert!(should_unload(10_000, 10_000 + 900_000, 600));
    }

    #[test]
    fn timeout_zero_never_unloads() {
        // idle_secs == 0 is the "never unload" knob — true for any gap.
        assert!(!should_unload(1, 1 + 10_000_000_000, 0));
        assert!(!should_unload(1, u64::MAX, 0));
    }

    #[test]
    fn never_used_sentinel_never_unloads() {
        // last_used_ms == 0 means "not loaded" — the reaper must not fire.
        assert!(!should_unload(0, u64::MAX, 600));
    }

    #[test]
    fn saturates_instead_of_overflowing() {
        // A backwards clock step (now < last) must not panic or wrap.
        assert!(!should_unload(1_000, 500, 600));
    }
}
