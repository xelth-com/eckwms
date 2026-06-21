use eck_core::utils::filestore::FileStore;
use image::codecs::avif::AvifEncoder;
#[allow(unused_imports)]
use image::ImageEncoder;
use serde_json::Value;
use std::io::Cursor;
use std::sync::Arc;
use tokio::fs;
use tracing::{debug, error, info, warn};

use crate::AppState;

const BATCH_SIZE: usize = 5;
const INTERVAL_SECS: u64 = 300;

// Aging thresholds in days
const LEVEL2_DAYS: i64 = 2 * 365 + 100;  // 830 days
const LEVEL3_DAYS: i64 = 6 * 365 + 100;  // 2290 days
const LEVEL4_DAYS: i64 = 11 * 365 + 100; // 4115 days

struct OptimizationTarget {
    level: i32,
    max_long_side: Option<u32>, // None = full-res
    quality: u8,
    include_webp: bool,
}

const TARGETS: &[OptimizationTarget] = &[
    // Level 1: immediate, JPEG/PNG only, full-res AVIF 60%
    OptimizationTarget { level: 1, max_long_side: None, quality: 60, include_webp: false },
    // Level 2: 2y+100d, all images incl. WebP, 1920px 60%
    OptimizationTarget { level: 2, max_long_side: Some(1920), quality: 60, include_webp: true },
    // Level 3: 6y+100d, 1280px 40%
    OptimizationTarget { level: 3, max_long_side: Some(1280), quality: 40, include_webp: true },
    // Level 4: 11y+100d, 800px 10% — "смутное воспоминание"
    OptimizationTarget { level: 4, max_long_side: Some(800), quality: 10, include_webp: true },
];

pub async fn start_optimizer_worker(state: Arc<AppState>) {
    // Cache nodes hold no local blobs — they pull `is_cache` rows on demand and
    // never own the filestore. Running the AVIF optimizer here is pure churn:
    // every candidate's blob is "missing on disk", so it would only ever mark
    // rows -2. Skip entirely (like the sync engine's cache-role short-circuits).
    if state.node_role == "cache" {
        info!("[ImageOptimizer] skipped (node_role=cache — no local blobs to optimize)");
        return;
    }
    tokio::time::sleep(std::time::Duration::from_secs(90)).await;
    info!("[ImageOptimizer] Background worker started (4-level aging: L1 immediate, L2 {}d, L3 {}d, L4 {}d)",
        LEVEL2_DAYS, LEVEL3_DAYS, LEVEL4_DAYS);

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(INTERVAL_SECS));

    loop {
        interval.tick().await;
        for target in TARGETS {
            if let Err(e) = optimize_batch(&state, target).await {
                warn!("[ImageOptimizer] Batch error (level {}): {}", target.level, e);
            }
        }
    }
}

async fn optimize_batch(state: &Arc<AppState>, target: &OptimizationTarget) -> anyhow::Result<()> {
    let db = &state.db;

    let query = if target.level == 1 {
        // Level 1: fresh JPEG/PNG that haven't been optimized yet
        "SELECT * FROM file_resource \
         WHERE mime_type IN ['image/jpeg', 'image/png'] \
         AND replaced_by IS NONE \
         AND (optimization_level IS NONE OR optimization_level = 0) \
         LIMIT $limit".to_string()
    } else {
        // Level 2+: files at previous level, old enough by created_at
        let age_days = match target.level {
            2 => LEVEL2_DAYS,
            3 => LEVEL3_DAYS,
            4 => LEVEL4_DAYS,
            _ => unreachable!(),
        };
        // L2+ picks up files below this level that we can decode (not AVIF — no native decoder).
        // This means: WebP (skipped by L1), plus any JPEG/PNG that somehow weren't processed.
        // AVIF files from L1 stay at L1 until avif-native decoder is available.
        let mime_filter = if target.include_webp {
            "mime_type IN ['image/jpeg', 'image/png', 'image/webp']"
        } else {
            "mime_type IN ['image/jpeg', 'image/png']"
        };
        let current_level = target.level;
        format!(
            "SELECT * FROM file_resource \
             WHERE {mime_filter} \
             AND replaced_by IS NONE \
             AND (optimization_level IS NONE OR (optimization_level >= 0 AND optimization_level < {current_level})) \
             AND created_at < time::now() - {age_days}d \
             LIMIT $limit"
        )
    };

    let files: Vec<Value> = db.query(&query).bind(("limit", BATCH_SIZE)).await?.take(0)?;

    info!("[ImageOptimizer] L{}: found {} candidates", target.level, files.len());

    if files.is_empty() {
        return Ok(());
    }

    let filestore = FileStore::new(".");

    for file in files {
        let old_id = file.get("cas_uuid").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let old_path = file.get("storage_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let old_name = file.get("original_name").and_then(|v| v.as_str()).unwrap_or("image.jpg").to_string();

        if old_id.is_empty() || old_path.is_empty() {
            continue;
        }

        debug!("[ImageOptimizer] Processing {} (level {})", old_id, target.level);

        let raw_bytes = match fs::read(&old_path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                warn!("[ImageOptimizer] File missing on disk, marking as missing: {} ({})", old_id, old_path);
                // Address by the cas_uuid FIELD, not record id: these records'
                // real `id` != `cas_uuid`, so `UPDATE file_resource:`{cas_uuid}``
                // was a silent no-op — the mark never stuck and the file was
                // re-scanned every cycle (240 WARN/h, ~109% CPU). WHERE-by-field
                // works regardless of the id<->cas_uuid convention.
                let _ = db
                    .query("UPDATE file_resource MERGE { optimization_level: -2, updated_at: time::now() } WHERE cas_uuid = $cid")
                    .bind(("cid", old_id.clone()))
                    .await;
                continue;
            }
            Err(e) => {
                warn!("[ImageOptimizer] Failed to read file {}: {}", old_path, e);
                continue;
            }
        };

        let raw_len = raw_bytes.len();
        let max_side = target.max_long_side;
        let quality = target.quality;

        let optimized_bytes_res = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
            let img = image::load_from_memory(&raw_bytes)?;

            let img = if let Some(max) = max_side {
                let (w, h) = (img.width(), img.height());
                let long_side = w.max(h);
                if long_side > max {
                    let ratio = max as f64 / long_side as f64;
                    let new_w = (w as f64 * ratio).round() as u32;
                    let new_h = (h as f64 * ratio).round() as u32;
                    img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3)
                } else {
                    // Already smaller than target — skip resize
                    img
                }
            } else {
                img
            };

            let mut out = Cursor::new(Vec::new());
            let encoder = AvifEncoder::new_with_speed_quality(&mut out, 6, quality);
            img.write_with_encoder(encoder)?;
            Ok(out.into_inner())
        }).await?;

        let optimized_bytes = match optimized_bytes_res {
            Ok(b) => b,
            Err(e) => {
                warn!("[ImageOptimizer] Encoding failed for {}: {}", old_id, e);
                let _ = db
                    .query("UPDATE file_resource MERGE { optimization_level: -1 } WHERE cas_uuid = $cid")
                    .bind(("cid", old_id.clone()))
                    .await;
                continue;
            }
        };

        let new_name = if let Some(idx) = old_name.rfind('.') {
            format!("{}.avif", &old_name[..idx])
        } else {
            format!("{}.avif", old_name)
        };

        let saved = match filestore.save(&optimized_bytes, &new_name, None, None).await {
            Ok(s) => s,
            Err(e) => {
                error!("[ImageOptimizer] Failed to save AVIF to CAS: {}", e);
                continue;
            }
        };

        let new_cas_id = saved.cas_uuid.to_string();

        if new_cas_id == old_id {
            continue;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let level = target.level;

        // Resolve real record ids by the cas_uuid FIELD inside the tx. The
        // `file_resource` table mixes id conventions: migrated rows have
        // id == cas_uuid, but app-inserted rows (uploads, support, optimizer
        // output) get a random id (`INSERT INTO file_resource {{ cas_uuid }}`).
        // The old record-addressed `file_resource:`{cas}`` therefore silently
        // missed app-uploaded files — edges weren't relinked, the old row was
        // never tombstoned, yet the original was still deleted below → data loss.
        // Field-addressing works for both conventions (like files.rs/support.rs).
        let tx_query = "\
            BEGIN TRANSACTION;\
            LET $old = (SELECT VALUE id FROM file_resource WHERE cas_uuid = $old_id LIMIT 1)[0];\
            LET $created = (INSERT INTO file_resource {\
                cas_uuid: $new_id,\
                hash: $new_hash,\
                original_name: $new_name,\
                mime_type: 'image/avif',\
                size_bytes: $new_size,\
                storage_path: $new_path,\
                created_by_device: 'system_optimizer',\
                context: 'optimization',\
                optimization_level: $level,\
                original_cas_uuid: $old_id,\
                created_at: $now,\
                updated_at: $now\
            });\
            LET $new = $created[0].id;\
            LET $edges = SELECT * FROM has_attachment WHERE out = $old;\
            FOR $edge IN $edges {\
                RELATE ($edge.in) -> has_attachment -> $new \
                SET label = $edge.label, is_main = $edge.is_main, created_at = $edge.created_at;\
            };\
            DELETE has_attachment WHERE out = $old;\
            UPDATE $old MERGE {\
                storage_path: NONE,\
                avatar_b64: NONE,\
                replaced_by: $new_id,\
                optimization_level: $level,\
                optimized_at: $now,\
                updated_at: $now\
            };\
            COMMIT TRANSACTION;\
        ";

        match db.query(tx_query)
            .bind(("old_id", old_id.clone()))
            .bind(("new_id", new_cas_id.clone()))
            .bind(("new_hash", saved.sha256))
            .bind(("new_name", new_name.clone()))
            .bind(("new_size", saved.size_bytes))
            .bind(("new_path", saved.storage_path))
            .bind(("now", now))
            .bind(("level", level))
            .await
        {
            Ok(resp) => {
                if let Err(e) = resp.check() {
                    error!("[ImageOptimizer] DB Transaction failed: {}", e);
                    continue;
                }
            }
            Err(e) => {
                error!("[ImageOptimizer] DB Query failed: {}", e);
                continue;
            }
        }

        // Only delete old file AFTER successful DB transaction
        let _ = fs::remove_file(&old_path).await;

        info!("[ImageOptimizer] L{}: {} -> {} ({}B -> {}B)",
            target.level, old_name, new_name, raw_len, optimized_bytes.len());
    }

    Ok(())
}
