//! Cell-tower resolution worker for PDA trips.
//!
//! Trips arrive with points that carry raw cell identity (MCC/MNC/TAC/CID)
//! instead of coordinates — the PDA records them passively without GPS.
//! This worker geocodes those cells:
//!   1. `cell_tower` cache table (towers don't move — resolve once, keep
//!      forever). Negative results are cached too: a cell OpenCelliD does not
//!      know gets a `resolve_failed: true` tombstone row, so an unknown cell
//!      costs ONE lookup mesh-wide, ever. (2026-08-10: the missing negative
//!      cache + three keyed cloud nodes re-resolving the same stuck open trip
//!      burned ~33k OpenCelliD requests in a week.)
//!   2. OpenCelliD HTTP API (needs `OPENCELLID_API_KEY`; free tier is rate-limited,
//!      so lookups are throttled to one per second and capped per cycle)
//!
//! SINGLE-WRITER RULE (2026-08-10): only the trip's HOME node
//! (`home_instance_id`, ownership rule of conflict.rs) mutates the trip row —
//! points, distance, seal, retention — and every such write bumps the author
//! `_vclock` so peers adopt it instead of LWW-fighting it. Non-home nodes that
//! hold an API key still perform lookups, but write ONLY into the mesh-synced
//! `cell_tower` cache; the home node (which may have no key at all, e.g. the
//! kiosk) then resolves its points from the cache. Before this, a non-home
//! node's point flags were reverted on every convergence with the authority's
//! copy, which re-armed the same lookups forever.
//!
//! Open trips whose recording died (device gone, app killed) are auto-closed
//! by the home node after `TRIP_OPEN_AUTOCLOSE_HOURS` (default 24, 0 =
//! disabled) without new activity — mirroring the movFast app's own
//! "auto-closed: recording was interrupted" behavior, so an abandoned open
//! trip cannot sit in every node's resolve queue indefinitely.
//!
//! When every point of an ended trip is resolved (or the trip is older than
//! 24 h — leftover cells are marked failed), the worker computes the trip
//! distance: accuracy-aware smoothed haversine × road factor (default 1.25,
//! env `TRIP_ROAD_FACTOR`). Cell positioning is approximate by nature; the
//! computed distance is an estimate and is flagged as such — odometer photos
//! remain the source of truth for the Fahrtenbuch.

use chrono::{DateTime, Utc};
use eck_core::db::SurrealDb;
use eck_core::sync::hedera::{self, HederaClient};
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tracing::{debug, info, warn};

const CYCLE_SECS: u64 = 30;
const TRIPS_PER_CYCLE: usize = 2;
const LOOKUPS_PER_CYCLE: usize = 20;
const MAX_PLAUSIBLE_SPEED_KMH: f64 = 160.0;
/// Odometer chain tolerance: prev trip end vs next trip start (km)
const ODOMETER_GAP_TOLERANCE_KM: f64 = 0.5;

pub async fn start_cell_resolver_worker(
    db: SurrealDb,
    hedera_client: Option<HederaClient>,
    instance_id: String,
) {
    tokio::time::sleep(Duration::from_secs(45)).await;
    info!("[CellResolver] Background worker started");

    let client = Client::builder()
        .user_agent("eckWMS-Server/1.0 (cell-resolver)")
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let api_key = std::env::var("OPENCELLID_API_KEY").ok().filter(|k| !k.is_empty());
    if api_key.is_none() {
        warn!("[CellResolver] OPENCELLID_API_KEY not set — resolving from cell_tower cache only");
    }

    let mut interval = tokio::time::interval(Duration::from_secs(CYCLE_SECS));
    let mut cycles: u64 = 0;

    loop {
        interval.tick().await;
        cycles += 1;

        // DSGVO retention: raw track points are personal data — strip them
        // once the trip is resolved and older than TRIP_RAW_RETENTION_DAYS
        // (default 14, legal guidance is 7–30). Aggregates (distance,
        // odometer, timestamps) and the tower cache (no PII) are kept.
        // Runs roughly twice a day. Home-gated single-writer: pruning is a
        // per-row authored write (vclock bump) so peers ADOPT the pruned row —
        // three nodes pruning concurrently at three timestamps was permanent
        // content divergence. Legacy rows without a home keep old behavior.
        if cycles % 1440 == 1 {
            let days: i64 = std::env::var("TRIP_RAW_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(14);
            let cutoff = (Utc::now() - chrono::Duration::days(days)).to_rfc3339();
            let prune_ids: Vec<Value> = db
                .query(
                    "SELECT VALUE record::id(id) FROM trip \
                     WHERE ended_at < $cutoff AND needs_resolution = false \
                     AND points_pruned_at IS NONE AND array::len(points ?? []) > 0 \
                     AND (home_instance_id = $iid OR home_instance_id IS NONE)",
                )
                .bind(("cutoff", cutoff))
                .bind(("iid", instance_id.clone()))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default();
            for idv in &prune_ids {
                let Some(id) = idv.as_str() else { continue };
                let vc = next_trip_vclock(&db, id, &instance_id).await;
                let res = db
                    .query(
                        "UPDATE type::record('trip', $id) SET points = [], \
                         points_pruned_at = $now, updated_at = $now, _vclock = $vc",
                    )
                    .bind(("id", id.to_string()))
                    .bind(("now", Utc::now().to_rfc3339()))
                    .bind(("vc", vc))
                    .await;
                if let Err(e) = res {
                    warn!("[CellResolver] retention prune of trip {} failed: {}", id, e);
                }
            }
            if !prune_ids.is_empty() {
                debug!(
                    "[CellResolver] retention pass pruned {} trips ({}d window)",
                    prune_ids.len(),
                    days
                );
            }
        }

        let trips: Vec<Value> = match db
            .query(
                "SELECT record::id(id) AS id, points, ended_at, status, started_at, \
                 device_id, driver_user_id, purpose, purpose_ref, purpose_declared_at, \
                 purpose_source, vehicle_plate, note, home_instance_id, \
                 start_odometer_km, start_odometer_source, end_odometer_km, end_odometer_source \
                 FROM trip WHERE needs_resolution = true LIMIT $lim",
            )
            .bind(("lim", TRIPS_PER_CYCLE as i64))
            .await
        {
            Ok(mut r) => r.take(0).unwrap_or_default(),
            Err(e) => {
                warn!("[CellResolver] trip query failed: {}", e);
                continue;
            }
        };

        for trip in trips {
            process_trip(
                &db,
                &client,
                api_key.as_deref(),
                hedera_client.as_ref(),
                &instance_id,
                trip,
            )
            .await;
        }
    }
}

async fn process_trip(
    db: &SurrealDb,
    client: &Client,
    api_key: Option<&str>,
    hedera_client: Option<&HederaClient>,
    instance_id: &str,
    mut trip: Value,
) {
    let trip_id = match trip.get("id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return,
    };
    // Single-writer gate: only the trip's HOME node mutates the trip row.
    // A legacy row without home_instance_id keeps the old any-node behavior
    // (better a churn-prone resolve than a trip nobody may finish).
    let is_home = trip
        .get("home_instance_id")
        .and_then(|v| v.as_str())
        .map_or(true, |h| h == instance_id);
    let mut points: Vec<Value> = trip
        .get("points")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Auto-close an abandoned open trip: recording died (device off, app
    // killed) and never came back, so the app-side "auto-closed: recording was
    // interrupted" close never fired. Without this the trip stays `open`
    // forever, never becomes expired (that clock keys off ended_at), and sits
    // in every node's resolve queue indefinitely.
    let mut trip_ended = trip.get("status").and_then(|v| v.as_str()) == Some("ended");
    if is_home && !trip_ended {
        if let Some(close_at) = stale_open_close_ts(&trip, &points, Utc::now(), autoclose_hours())
        {
            let prev_note = trip.get("note").and_then(|v| v.as_str()).unwrap_or("");
            let close_note = format!(
                "auto-closed: recording was interrupted; ended at last recorded activity {}",
                close_at
            );
            let note = if prev_note.is_empty() {
                close_note
            } else {
                format!("{} | {}", prev_note, close_note)
            };
            let vc = next_trip_vclock(db, &trip_id, instance_id).await;
            let update = json!({
                "status": "ended",
                "ended_at": close_at,
                "note": note,
                "updated_at": Utc::now().to_rfc3339(),
                "_vclock": vc,
            });
            match db
                .query("UPDATE type::record('trip', $id) MERGE $update")
                .bind(("id", trip_id.clone()))
                .bind(("update", update.clone()))
                .await
            {
                Ok(_) => {
                    info!("[CellResolver] trip {} auto-closed (stale open, last activity {})", trip_id, close_at);
                    // Keep the in-memory copy coherent — trip_expired below and
                    // the seal canonical both read ended_at/status from it.
                    if let Some(obj) = trip.as_object_mut() {
                        obj.insert("status".into(), json!("ended"));
                        obj.insert("ended_at".into(), json!(close_at));
                        obj.insert("note".into(), update["note"].clone());
                    }
                    trip_ended = true;
                }
                Err(e) => warn!("[CellResolver] trip {} auto-close failed: {}", trip_id, e),
            }
        }
    }

    let ended_at = trip.get("ended_at").and_then(|v| v.as_str()).map(String::from);
    let trip_expired = ended_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| Utc::now().signed_duration_since(t.with_timezone(&Utc)).num_hours() >= 24)
        .unwrap_or(false);

    let mut lookups_done = 0usize;
    let mut changed = false;
    let mut still_pending = 0usize;
    // A transient API failure (HTTP 5xx/429, network) stops further lookups
    // this cycle — hammering a rate-limited endpoint helps nobody.
    let mut api_degraded = false;

    for p in points.iter_mut() {
        let has_coords = p.get("lat").and_then(|v| v.as_f64()).is_some();
        let failed = p.get("resolve_failed").and_then(|v| v.as_bool()).unwrap_or(false);
        if has_coords || failed {
            continue;
        }

        let (mcc, mnc, tac, cid) = (
            p.get("mcc").and_then(|v| v.as_i64()),
            p.get("mnc").and_then(|v| v.as_i64()),
            p.get("tac").and_then(|v| v.as_i64()),
            p.get("cid").and_then(|v| v.as_i64()),
        );
        let (Some(mcc), Some(mnc), Some(tac), Some(cid)) = (mcc, mnc, tac, cid) else {
            if is_home {
                set_point_failed(p);
                changed = true;
            }
            continue;
        };

        let cache_key = format!("{}-{}-{}-{}", mcc, mnc, tac, cid);

        // 1. Cache hit? (positive rows carry lat/lng; negative rows carry
        //    resolve_failed: true — a cell OpenCelliD does not know)
        let cached: Option<Value> = db
            .select(("cell_tower", cache_key.as_str()))
            .await
            .ok()
            .flatten();

        if let Some(tower) = cached {
            if tower.get("resolve_failed").and_then(|v| v.as_bool()).unwrap_or(false) {
                if is_home {
                    set_point_failed(p);
                    changed = true;
                }
            } else if is_home {
                adopt_tower(p, &tower);
                changed = true;
            }
            continue;
        }

        // 2. Cache miss on an expired trip: give up on the cell (home marks the
        //    point failed; a non-home node has nothing to write).
        if trip_expired {
            if is_home {
                set_point_failed(p);
                changed = true;
            } else {
                still_pending += 1;
            }
            continue;
        }

        // 3. API lookup — any keyed node may perform it, but the RESULT goes
        //    into the mesh-synced cell_tower cache only; the home node adopts
        //    coordinates into the trip row (possibly next cycle, from cache).
        let Some(key) = api_key else {
            still_pending += 1;
            continue;
        };
        if api_degraded || lookups_done >= LOOKUPS_PER_CYCLE {
            still_pending += 1;
            continue;
        }
        lookups_done += 1;
        // Free-tier politeness: one lookup per second
        tokio::time::sleep(Duration::from_secs(1)).await;
        match opencellid_lookup(client, key, mcc, mnc, tac, cid).await {
            Lookup::Found(tower) => {
                let _: Result<Option<Value>, _> = db
                    .create(("cell_tower", cache_key.as_str()))
                    .content(tower.clone())
                    .await;
                if is_home {
                    adopt_tower(p, &tower);
                    changed = true;
                } else {
                    still_pending += 1;
                }
            }
            Lookup::UnknownCell => {
                // Negative cache: tombstone the cell mesh-wide so no node ever
                // asks OpenCelliD about it again.
                let tombstone = json!({
                    "mcc": mcc, "mnc": mnc, "tac": tac, "cid": cid,
                    "resolve_failed": true,
                    "source": "opencellid",
                    "samples": 0,
                    "resolved_at": Utc::now().to_rfc3339(),
                });
                let _: Result<Option<Value>, _> = db
                    .create(("cell_tower", cache_key.as_str()))
                    .content(tombstone)
                    .await;
                if is_home {
                    set_point_failed(p);
                    changed = true;
                }
            }
            Lookup::Transient => {
                // NEVER tombstone on quota/network errors — that would poison
                // the mesh-wide cache with cells that exist. Retry next cycle.
                api_degraded = true;
                still_pending += 1;
            }
        }
    }

    // Non-home nodes only feed the shared cache — the trip row is the home
    // node's to write (its convergence would revert our copy anyway).
    if !is_home {
        return;
    }

    let resolution_complete = still_pending == 0 && (trip_ended || trip_expired);

    let mut update = json!({ "points": points });
    let mut final_distance: Option<f64> = None;
    if resolution_complete {
        let distance = compute_distance_km(update["points"].as_array().unwrap());
        final_distance = distance;
        update["computed_distance_km"] = json!(distance);
        update["distance_is_estimate"] = json!(true);
        update["needs_resolution"] = json!(false);
        update["resolved_at"] = json!(Utc::now().to_rfc3339());
        info!(
            "[CellResolver] trip {} resolved: {:.1} km estimated",
            trip_id,
            distance.unwrap_or(0.0)
        );
    } else if !changed {
        // Nothing we can do this cycle (no key / lookup budget spent)
        return;
    }

    // Author the write properly: bump this node's vclock component so peers
    // ADOPT the row (the "synced-table writers must stamp _vclock" tech-debt —
    // the unstamped resolver write was exactly what peers kept reverting).
    update["_vclock"] = next_trip_vclock(db, &trip_id, instance_id).await;
    update["updated_at"] = json!(Utc::now().to_rfc3339());

    let res = db
        .query("UPDATE type::record('trip', $id) MERGE $update")
        .bind(("id", trip_id.clone()))
        .bind(("update", update))
        .await;
    if let Err(e) = res {
        warn!("[CellResolver] trip {} update failed: {}", trip_id, e);
        return;
    }

    if resolution_complete {
        seal_trip(db, hedera_client, instance_id, &trip, &trip_id, final_distance).await;
    }
}

/// Copy a resolved tower's coordinates onto a track point.
fn adopt_tower(p: &mut Value, tower: &Value) {
    if let Some(obj) = p.as_object_mut() {
        obj.insert("lat".into(), tower.get("lat").cloned().unwrap_or(Value::Null));
        obj.insert("lng".into(), tower.get("lng").cloned().unwrap_or(Value::Null));
        obj.insert(
            "accuracy_m".into(),
            tower.get("range_m").cloned().unwrap_or(json!(1500)),
        );
        obj.insert("resolved_by".into(), json!("cell_tower"));
    }
}

/// Hours of inactivity after which the HOME node auto-closes an open trip.
/// 0 disables the auto-close entirely.
fn autoclose_hours() -> i64 {
    std::env::var("TRIP_OPEN_AUTOCLOSE_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24)
}

/// For an OPEN trip, decide whether recording is dead: returns the timestamp
/// to close the trip at (last point ts, else started_at) when the last
/// activity is older than `hours`. Pure so it's testable.
fn stale_open_close_ts(
    trip: &Value,
    points: &[Value],
    now: DateTime<Utc>,
    hours: i64,
) -> Option<String> {
    if hours <= 0 {
        return None;
    }
    let last_activity = points
        .iter()
        .filter_map(|p| p.get("ts").and_then(|v| v.as_str()))
        .max()
        .or_else(|| trip.get("started_at").and_then(|v| v.as_str()))?
        .to_string();
    let t = DateTime::parse_from_rfc3339(&last_activity).ok()?;
    if now.signed_duration_since(t.with_timezone(&Utc)).num_hours() >= hours {
        Some(last_activity)
    } else {
        None
    }
}

/// Read-increment this node's `_vclock` component on a trip row (preserving
/// peer components) — the house way to author a write on a synced table.
async fn next_trip_vclock(db: &SurrealDb, trip_id: &str, instance_id: &str) -> Value {
    let cur: Vec<Value> = db
        .query("SELECT VALUE _vclock FROM type::record('trip', $id) LIMIT 1")
        .bind(("id", trip_id.to_string()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    eck_core::sync::conflict::next_local_vclock(cur.first(), instance_id)
}

/// GoBD seal (Fahrtenbuch "unveränderbar"): canonical hash over the financially
/// relevant aggregate — NOT the raw points, which fall to the DSGVO retention
/// pass; the seal stays verifiable after pruning. Every re-resolution appends a
/// new seal version (a changed trip is a new sealed version, never a silent edit).
/// Also validates the odometer chain against the previous trip of the device.
async fn seal_trip(
    db: &SurrealDb,
    hedera_client: Option<&HederaClient>,
    instance_id: &str,
    trip: &Value,
    trip_id: &str,
    distance: Option<f64>,
) {
    let s = |k: &str| trip.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let f = |k: &str| trip.get(k).and_then(|v| v.as_f64());

    // Odometer chain: compare against the previous ended trip of this device
    let mut odometer_gap_km: Option<f64> = None;
    if let (Some(start_odo), false) = (f("start_odometer_km"), s("device_id").is_empty()) {
        let prev: Vec<Value> = db
            .query(
                "SELECT end_odometer_km, started_at FROM trip \
                 WHERE device_id = $dev AND status = 'ended' AND started_at < $start \
                 AND end_odometer_km IS NOT NONE \
                 ORDER BY started_at DESC LIMIT 1",
            )
            .bind(("dev", s("device_id")))
            .bind(("start", s("started_at")))
            .await
            .ok()
            .and_then(|mut r| r.take(0).ok())
            .unwrap_or_default();
        if let Some(prev_end) = prev.first().and_then(|p| p.get("end_odometer_km")).and_then(|v| v.as_f64()) {
            let gap = start_odo - prev_end;
            if gap.abs() > ODOMETER_GAP_TOLERANCE_KM {
                odometer_gap_km = Some((gap * 10.0).round() / 10.0);
                warn!(
                    "[CellResolver] trip {} odometer gap: {:.1} km unaccounted since previous trip",
                    trip_id, gap
                );
            }
        }
    }

    let canonical =
        fahrtenbuch_canonical(FAHRTENBUCH_CANONICAL_VERSION, trip, trip_id, distance, odometer_gap_km);
    let hash = hex::encode(Sha256::digest(canonical.as_bytes()));

    // IDEMPOTENT SEAL — the crux of the trip mesh non-convergence.
    // Every node runs this resolver, and `hedera_seals` (which carries a per-node
    // wall-clock `sealed_at`) is part of the merkle content hash. Appending a fresh
    // seal on every resolution cycle made each node grow its OWN seal history at its
    // OWN timestamps → two nodes NEVER produce the same content hash → "roots differ"
    // forever (the churn we've chased ~"100 times"). A GoBD "new sealed version" is
    // only warranted when the canonical hash actually changes; a re-resolution that
    // reproduces the identical hash must be a no-op (no append, no re-write, no
    // hedera submit) so the row stays byte-stable and the mesh can settle.
    let existing_seal_hash: Vec<String> = db
        .query("SELECT VALUE seal_hash FROM type::record('trip', $id) LIMIT 1")
        .bind(("id", trip_id.to_string()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    if existing_seal_hash.first().map(String::as_str) == Some(hash.as_str()) {
        debug!(
            "[CellResolver] trip {} already sealed (hash {}… unchanged) — skip append",
            trip_id,
            &hash[..8.min(hash.len())]
        );
        return;
    }

    let receipt = hedera::submit_hash_if_configured(hedera_client, &hash).await;

    let seal = json!({
        "hash": hash,
        "canonical_version": FAHRTENBUCH_CANONICAL_VERSION,
        "sealed_at": Utc::now().to_rfc3339(),
        "hedera_sequence": receipt.as_ref().map(|r| r.sequence_number),
        "hedera_timestamp": receipt.as_ref().map(|r| r.consensus_timestamp.clone()),
    });

    // Authored write: bump the home vclock so the sealed row dominates peers'
    // copies (the seal is only ever written by the home node — see the
    // single-writer gate in process_trip).
    let vc = next_trip_vclock(db, trip_id, instance_id).await;
    let res = db
        .query(
            "UPDATE type::record('trip', $id) SET \
             seal_hash = $hash, odometer_gap_km = $gap, \
             seal_canonical_version = $ver, \
             hedera_seals = array::concat(hedera_seals ?? [], [$seal]), \
             _vclock = $vc, updated_at = $now",
        )
        .bind(("id", trip_id.to_string()))
        .bind(("hash", hash.clone()))
        .bind(("gap", odometer_gap_km))
        .bind(("ver", FAHRTENBUCH_CANONICAL_VERSION))
        .bind(("seal", seal))
        .bind(("vc", vc))
        .bind(("now", Utc::now().to_rfc3339()))
        .await;
    match res {
        Ok(_) => info!(
            "[CellResolver] trip {} sealed: {}… (hedera={})",
            trip_id,
            &hash[..16],
            receipt.is_some()
        ),
        Err(e) => warn!("[CellResolver] trip {} seal store failed: {}", trip_id, e),
    }
}

/// Current Fahrtenbuch seal canonical version. Bumped v2→v3 to fold the vehicle
/// plate (amtliches Kennzeichen — a GoBD-required Fahrtenbuch field) into the
/// sealed aggregate. Legacy v2 seals stay verifiable: `fahrtenbuch_canonical`
/// reproduces the EXACT v2 byte string when given "fahrtenbuch:v2".
pub(crate) const FAHRTENBUCH_CANONICAL_VERSION: &str = "fahrtenbuch:v3";

/// Canonical GoBD aggregate string for a Fahrtenbuch trip seal. **Single source
/// of truth** shared by `seal_trip()` (write side) and the `/verify` + Z3 export
/// (read side) so they can never drift apart. `version` selects the format: v2
/// is the original 15-field aggregate; v3 appends the vehicle plate. Pass the
/// version the trip was SEALED under (stored as `seal_canonical_version`,
/// defaulting to v2 for legacy trips) so a later recompute reproduces the same
/// hash. `distance` / `odometer_gap_km` are the values stored on the trip.
pub(crate) fn fahrtenbuch_canonical(
    version: &str,
    trip: &Value,
    trip_id: &str,
    distance: Option<f64>,
    odometer_gap_km: Option<f64>,
) -> String {
    let s = |k: &str| trip.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let f = |k: &str| trip.get(k).and_then(|v| v.as_f64());
    let mut out = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{:?}|{}|{:?}|{}|{:?}|{:?}",
        version,
        trip_id,
        s("device_id"),
        s("driver_user_id"),
        s("started_at"),
        s("ended_at"),
        s("purpose"),
        s("purpose_ref"),
        s("purpose_declared_at"),
        s("purpose_source"),
        f("start_odometer_km"),
        s("start_odometer_source"),
        f("end_odometer_km"),
        s("end_odometer_source"),
        distance,
        odometer_gap_km,
    );
    // v3 folds the vehicle plate into the sealed aggregate. v2 (which had no
    // vehicle field) stays byte-identical, so existing v2 seals still verify.
    if version == FAHRTENBUCH_CANONICAL_VERSION {
        out.push_str(&format!("|{}", s("vehicle_plate")));
    }
    out
}

fn set_point_failed(p: &mut Value) {
    if let Some(obj) = p.as_object_mut() {
        obj.insert("resolve_failed".into(), json!(true));
    }
}

/// Outcome of an OpenCelliD lookup. The distinction matters for caching:
/// `UnknownCell` is a DEFINITIVE answer and gets a mesh-wide tombstone;
/// `Transient` (quota, network, 5xx) must never be cached or the mesh would be
/// poisoned with tombstones for cells that exist.
enum Lookup {
    Found(Value),
    UnknownCell,
    Transient,
}

/// OpenCelliD single-cell lookup → {lat, lng, range_m, resolved_at}.
async fn opencellid_lookup(
    client: &Client,
    key: &str,
    mcc: i64,
    mnc: i64,
    tac: i64,
    cid: i64,
) -> Lookup {
    let url = format!(
        "https://opencellid.org/cell/get?key={}&mcc={}&mnc={}&lac={}&cellid={}&format=json",
        key, mcc, mnc, tac, cid
    );
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            debug!("[CellResolver] OpenCelliD request error for {}-{}-{}-{}: {}", mcc, mnc, tac, cid, e);
            return Lookup::Transient;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        debug!("[CellResolver] OpenCelliD HTTP {} for {}-{}-{}-{}", status, mcc, mnc, tac, cid);
        // 404 = the API answered: this cell is not in the database. Anything
        // else (401/403 key trouble, 429 quota, 5xx) is not a statement about
        // the cell — retry later.
        return if status.as_u16() == 404 {
            Lookup::UnknownCell
        } else {
            Lookup::Transient
        };
    }
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Lookup::Transient,
    };
    let (Some(lat), Some(lon)) = (
        body.get("lat").and_then(|v| v.as_f64()),
        body.get("lon").and_then(|v| v.as_f64()),
    ) else {
        // 2xx with no coordinates ({"error":"cell not found"} style body) —
        // a definitive miss, not an outage.
        return Lookup::UnknownCell;
    };
    let mut range = body.get("range").and_then(|v| v.as_f64()).unwrap_or(1500.0);
    let samples = body.get("samples").and_then(|v| v.as_i64()).unwrap_or(0);
    // Single-observation towers are the 100-km-jump factory (2026-07-06: all
    // four wild outliers, worst resolved to Kassel 163 km off the A5, had
    // samples=1). Keep the position but declare the honest uncertainty — the
    // distance filters and the device-side smoother σ both key off range_m.
    if samples <= 1 {
        range = range.max(20_000.0);
    }
    Lookup::Found(json!({
        "lat": lat,
        "lng": lon,
        "range_m": range,
        "samples": samples,
        "mcc": mcc, "mnc": mnc, "tac": tac, "cid": cid,
        "source": "opencellid",
        "resolved_at": Utc::now().to_rfc3339(),
    }))
}

/// Accuracy-aware track distance.
///
/// **Source preference:** fused/GPS points (±10-150 m) are accurate; cell
/// points (±500 m-1.5 km) are coarse. Mixing them lets the cell uncertainty
/// swamp the jitter filter and zero out real movement (a genuine 2 km drive
/// with good GPS came out 0.0). So when the trip has ≥2 fused/GPS points we
/// compute the track from THOSE; cells are only the fallback for cell-only
/// trips. × road factor (straight hops underestimate road length).
fn compute_distance_km(points: &[Value]) -> Option<f64> {
    let road_factor: f64 = std::env::var("TRIP_ROAD_FACTOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.25);

    let collect = |fine_only: bool| -> Vec<(f64, f64, f64, Option<DateTime<Utc>>)> {
        points
            .iter()
            .filter_map(|p| {
                let src = p.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let is_fine = src == "fused" || src == "gps";
                if fine_only && !is_fine {
                    return None;
                }
                let lat = p.get("lat")?.as_f64()?;
                let lng = p.get("lng")?.as_f64()?;
                let acc = p.get("accuracy_m").and_then(|v| v.as_f64()).unwrap_or(1000.0);
                let ts = p
                    .get("ts")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.with_timezone(&Utc));
                let seq = p.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
                Some((seq, (lat, lng, acc, ts)))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_values()
            .collect()
    };

    // Road factor compensates for straight-line hops between COARSE cell
    // positions undercutting the curvy road. Fused/GPS already follows the
    // road, so it gets NO inflation (×1.0) — otherwise a long drive would come
    // out ~25% too long, and under-counting is the legally safe direction for a
    // Fahrtenbuch anyway.
    let fine = collect(true);
    let (track, factor) = if fine.len() >= 2 {
        (fine, 1.0)
    } else {
        (collect(false), road_factor)
    };

    track_distance_m(&track).map(|m| (m / 1000.0 * factor * 10.0).round() / 10.0)
}

/// Sum the seq-ordered track, skipping jitter (hop < combined uncertainty) and
/// physically impossible cell bounces (> MAX_PLAUSIBLE_SPEED_KMH).
fn track_distance_m(resolved: &[(f64, f64, f64, Option<DateTime<Utc>>)]) -> Option<f64> {
    if resolved.len() < 2 {
        return None;
    }
    let mut total_m = 0.0;
    let mut anchor = resolved[0];
    for &next in &resolved[1..] {
        let d = haversine_m(anchor.0, anchor.1, next.0, next.1);
        let uncertainty = anchor.2.max(next.2);
        if d < uncertainty {
            continue; // jitter below positioning noise — keep the anchor
        }
        if let (Some(t1), Some(t2)) = (anchor.3, next.3) {
            let dt_h = (t2 - t1).num_seconds().abs() as f64 / 3600.0;
            if dt_h > 0.0 && (d / 1000.0) / dt_h > MAX_PLAUSIBLE_SPEED_KMH {
                continue; // cell bounce — impossible speed
            }
        }
        total_m += d;
        anchor = next;
    }
    Some(total_m)
}

fn haversine_m(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let r = 6_371_000.0_f64;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lng2 - lng1).to_radians());
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_frankfurt_eschborn() {
        // Frankfurt Hbf → Eschborn ≈ 9.5 km straight line
        let d = haversine_m(50.1071, 8.6638, 50.1407, 8.5721) / 1000.0;
        assert!((d - 7.5).abs() < 1.5, "got {d}");
    }

    #[test]
    fn distance_skips_jitter_and_bounce() {
        let mk = |seq: i64, lat: f64, lng: f64, acc: f64, ts: &str| {
            json!({"seq": seq, "lat": lat, "lng": lng, "accuracy_m": acc, "ts": ts})
        };
        let points = vec![
            mk(1, 50.1000, 8.5000, 300.0, "2026-06-12T10:00:00Z"),
            // jitter: 100 m hop with 300 m accuracy — ignored
            mk(2, 50.1009, 8.5000, 300.0, "2026-06-12T10:00:30Z"),
            // real movement: ~11 km in 10 min (66 km/h) — counted
            mk(3, 50.2000, 8.5000, 300.0, "2026-06-12T10:10:00Z"),
            // bounce: ~110 km hop in 30 s — impossible, ignored
            mk(4, 51.2000, 8.5000, 300.0, "2026-06-12T10:10:30Z"),
        ];
        let d = compute_distance_km(&points).unwrap();
        // ~11.1 km × 1.25 ≈ 13.9
        assert!((d - 13.9).abs() < 0.5, "got {d}");
    }

    #[test]
    fn fused_points_preferred_over_coarse_cells() {
        // Real bug: a genuine drive with good GPS came out 0.0 because coarse
        // cell points (±600 m) interleaved with fine fused points (±30 m) and
        // the cell uncertainty swamped the jitter filter. With ≥2 fused points
        // the track must be computed from THEM.
        let cell = |seq: i64, lat: f64, lng: f64| {
            json!({"seq": seq, "lat": lat, "lng": lng, "accuracy_m": 600.0, "source": "cell",
                   "ts": format!("2026-06-12T10:{:02}:00Z", seq)})
        };
        let fused = |seq: i64, lat: f64, lng: f64| {
            json!({"seq": seq, "lat": lat, "lng": lng, "accuracy_m": 30.0, "source": "fused",
                   "ts": format!("2026-06-12T10:{:02}:00Z", seq)})
        };
        // Cells clustered (all within ~600 m → cell-only distance = 0), but the
        // fused points trace ~1.1 km of real movement.
        let points = vec![
            cell(1, 50.1400, 8.5700),
            fused(2, 50.1400, 8.5700),
            cell(3, 50.1405, 8.5702),
            fused(4, 50.1450, 8.5700), // ~555 m north
            cell(5, 50.1402, 8.5701),
            fused(6, 50.1500, 8.5700), // another ~555 m north
        ];
        let d = compute_distance_km(&points).unwrap();
        // fused track ~1.11 km × 1.0 (GPS gets no road factor) ≈ 1.1 km — NOT 0
        assert!(d > 1.0, "expected fused-based distance >1 km, got {d}");
    }

    #[test]
    fn stale_open_trip_detection() {
        let now = DateTime::parse_from_rfc3339("2026-08-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let trip = json!({ "started_at": "2026-07-30T13:09:07.735Z" });
        let points = vec![
            json!({"ts": "2026-07-30T13:10:00Z"}),
            json!({"ts": "2026-07-30T18:04:38.942Z"}),
        ];
        // 11 days silent → close at the LAST point ts, not started_at
        assert_eq!(
            stale_open_close_ts(&trip, &points, now, 24).as_deref(),
            Some("2026-07-30T18:04:38.942Z")
        );
        // no points → falls back to started_at
        assert_eq!(
            stale_open_close_ts(&trip, &[], now, 24).as_deref(),
            Some("2026-07-30T13:09:07.735Z")
        );
        // fresh activity → stays open
        let fresh = vec![json!({"ts": "2026-08-10T11:30:00Z"})];
        assert_eq!(stale_open_close_ts(&trip, &fresh, now, 24), None);
        // 0 = feature disabled
        assert_eq!(stale_open_close_ts(&trip, &points, now, 0), None);
    }

    #[test]
    fn seal_canonical_roundtrips_through_json() {
        // The /verify endpoint recomputes the hash from values reloaded from
        // SurrealDB (JSON). This must reproduce EXACTLY what seal_trip wrote —
        // otherwise an honest record would fail verification. Guards the f64
        // formatting ({:?}) surviving a JSON store→load round-trip.
        let trip = json!({
            "device_id": "dev-1",
            "driver_user_id": "user:7",
            "started_at": "2026-06-13T08:00:00Z",
            "ended_at": "2026-06-13T08:42:00Z",
            "purpose": "business",
            "purpose_ref": "order:abc123",
            "purpose_declared_at": "2026-06-13T08:00:00Z",
            "purpose_source": "planned",
            "start_odometer_km": 454039.0,
            "start_odometer_source": "photo",
            "end_odometer_km": 454051.0,
            "end_odometer_source": "photo",
        });
        let trip_id = "trip-uuid-1";
        let (distance, gap) = (Some(12.3), Some(0.5));

        // write side (seal_trip)
        let hash_write = hex::encode(Sha256::digest(
            fahrtenbuch_canonical(FAHRTENBUCH_CANONICAL_VERSION, &trip, trip_id, distance, gap).as_bytes(),
        ));

        // persist the computed aggregate and round-trip through JSON text
        let mut stored = trip.clone();
        stored["computed_distance_km"] = json!(distance);
        stored["odometer_gap_km"] = json!(gap);
        let reloaded: Value =
            serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();

        // read side (verify_trip): pull distance/gap from the reloaded record
        let d = reloaded.get("computed_distance_km").and_then(|v| v.as_f64());
        let g = reloaded.get("odometer_gap_km").and_then(|v| v.as_f64());
        let hash_read = hex::encode(Sha256::digest(
            fahrtenbuch_canonical(FAHRTENBUCH_CANONICAL_VERSION, &reloaded, trip_id, d, g).as_bytes(),
        ));

        assert_eq!(hash_write, hash_read, "verify recompute must match seal hash");
    }
}
