use eck_core::db::SurrealDb;
use eck_core::utils::anonymizer::{obfuscate_pii, scrub_pii_regex};
use reqwest::Client as HttpClient;
use serde_json::Value;
use std::sync::Arc;
use super::loop_guard::LoopGuard;
use super::telemetry::{log_telemetry, current_budget_level, BudgetLevel, THROTTLE_DELAY_SECS};
use tracing::{debug, info, warn};

const EMBEDDING_DIM: usize = 768;
const BATCH_LIMIT: usize = 50;
const LOOP_INTERVAL_SECS: u64 = 5;
const RATE_LIMIT_MS: u64 = 200;
const MAX_RETRIES: i64 = 5;
/// Idle-poll backoff cap. With the `embedding_status` index the per-cycle
/// "any pending?" probe is a cheap seek, but when nothing is pending for many
/// cycles we also stretch the interval (LOOP_INTERVAL_SECS → up to
/// LOOP_INTERVAL_SECS * (1 + IDLE_BACKOFF_CAP)) so a fully-embedded node idles.
const IDLE_BACKOFF_CAP: u64 = 11; // 5s base → up to 60s when fully idle

/// Canonical `embedding_model` id written for on-device (local-mode) vectors.
/// Kept as a literal here (not behind the feature) so `effective_embed_model`
/// and its test compile whether or not `local-embed` is enabled. Must match
/// `local_embed::MODEL_NAME`.
const LOCAL_EMBED_MODEL: &str = "jina-embeddings-v2-base-de";

/// Runtime embedding-backend selector. `ECK_EMBED_MODE=local` → on-device Jina
/// (requires the `local-embed` feature to be compiled in); anything else, or an
/// unset var, → the cloud (Gemini/Vertex) path. The fleet ships one binary and
/// this env var alone decides. Parsed once and cached (the value can't change
/// mid-process), which also makes the "asked for local but built cloud-only"
/// warning fire exactly once.
pub fn embed_mode() -> &'static str {
    use std::sync::OnceLock;
    static MODE: OnceLock<&'static str> = OnceLock::new();
    *MODE.get_or_init(|| {
        let raw = std::env::var("ECK_EMBED_MODE").ok();
        let mode = resolve_embed_mode(raw.as_deref());
        let asked_local = raw
            .as_deref()
            .map(|v| v.trim().eq_ignore_ascii_case("local"))
            .unwrap_or(false);
        if asked_local && mode == "cloud" {
            warn!(
                "[Embeddings] ECK_EMBED_MODE=local requested but this binary was built \
                 without the `local-embed` feature — using cloud embeddings"
            );
        }
        mode
    })
}

/// Pure mode resolver: maps the raw `ECK_EMBED_MODE` value to `"local"`/`"cloud"`
/// with the feature gate applied, and no process-global state. Split out from
/// `embed_mode` so it can be unit-tested without racing other tests on the env.
fn resolve_embed_mode(env_val: Option<&str>) -> &'static str {
    let want_local = env_val
        .map(|v| v.trim().eq_ignore_ascii_case("local"))
        .unwrap_or(false);
    if !want_local {
        return "cloud";
    }
    // Env asked for local; honour it only if the backend was compiled in.
    #[cfg(feature = "local-embed")]
    {
        "local"
    }
    #[cfg(not(feature = "local-embed"))]
    {
        "cloud"
    }
}

/// Model name to record with a vector and to name in telemetry. Local mode
/// always writes the on-device model id (so a future migration can tell
/// on-device vectors from cloud ones and requeue across spaces); cloud mode
/// keeps the configured cloud model name unchanged.
pub fn effective_embed_model(cloud_model: &str) -> String {
    effective_embed_model_for(embed_mode(), cloud_model)
}

/// Pure `(mode, cloud_model) → model name` mapping — unit-testable without env.
fn effective_embed_model_for(mode: &str, cloud_model: &str) -> String {
    if mode == "local" {
        LOCAL_EMBED_MODEL.to_string()
    } else {
        cloud_model.to_string()
    }
}

/// Dispatch one embed call to the on-device model (local mode) or the cloud
/// provider (cloud mode). Returns `(vector, usage)`; local usage is `Value::Null`
/// (nothing was billed) so the existing telemetry guard naturally skips it.
async fn embed_dispatch(
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    emb_model: &str,
    text: &str,
    local: bool,
) -> Result<(Vec<f32>, Value), anyhow::Error> {
    #[cfg(feature = "local-embed")]
    if local {
        let vector = super::local_embed::embed(text).await?;
        return Ok((vector, Value::Null));
    }
    // `local` is never true when the feature is compiled out (embed_mode can't
    // return "local" then); bind it so the parameter isn't flagged unused.
    let _ = local;
    auth.embed_content(http, emb_model, text, EMBEDDING_DIM).await
}

/// Spawns the background embedding worker that processes pending documents and orders.
///
/// Vectors are mesh-synced content (see merkle.rs IGNORED_FIELDS note,
/// 2026-07-12): the worker only picks rows whose vector is ABSENT, and bumps
/// this node's `_vclock` on every success write so its vector dominates
/// vector-less peer copies. Symmetric mesh: every AI-enabled node runs the
/// worker and whoever meets the missing vector first authors it. Asymmetric
/// (one AI-dedicated box): set `ECK_EMBED_WORKER=0` on all other nodes — they
/// consume vectors from the mesh and never call the embed API themselves.
pub async fn start_embedding_worker(db: SurrealDb, gen_model: String, emb_model: String, instance_id: String) {
    // Initial delay to let the server finish startup
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    info!("[Embeddings] Worker started ({LOOP_INTERVAL_SECS}s interval)");

    // Reset retryable embeddings on startup. Includes 'paused_by_observer'
    // (retry counter cleared — loop root cause was fixed at the import
    // layer). Docs exhausted at MAX_RETRIES stay 'error' permanently.
    reset_retryable(&db).await;

    let http = HttpClient::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("failed to build embeddings HTTP client");
    let guard = Arc::new(LoopGuard::new());

    // Adaptive idle backoff. The per-cycle "any pending?" probe is now an index
    // seek (see the embedding_status indexes in main.rs / migrator), but when a
    // node is fully embedded we additionally stretch the interval so it stays
    // truly idle instead of waking all 5 tables every 5s. Resets to the base
    // interval the moment work reappears.
    let mut idle_cycles: u64 = 0;
    // Periodic self-heal: transient-failure victims (429/502 bursts) used to
    // sit in 'error' until the next process restart.
    let mut last_reset = std::time::Instant::now();
    const RESET_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

    loop {
        let sleep_secs = LOOP_INTERVAL_SECS + idle_cycles.min(IDLE_BACKOFF_CAP) * LOOP_INTERVAL_SECS;
        tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
        eck_core::metrics::tick(eck_core::metrics::M::EmbeddingCycle);
        if last_reset.elapsed() >= RESET_EVERY {
            reset_retryable(&db).await;
            last_reset = std::time::Instant::now();
        }

        // Resolve auth each cycle: in managed mode this returns a process-cached
        // Vertex bearer and transparently re-mints it before expiry.
        let auth = match eck_core::ai::AiAuth::resolve(&http).await {
            Ok(a) if a.is_configured() => a,
            resolved => {
                // Local mode embeds on-device and skips extract_and_anonymize,
                // so it needs NO cloud auth. Fall through with a placeholder
                // Studio auth (process_table never uses it for a network call in
                // local mode). Cloud mode keeps the original back-off/skip.
                if embed_mode() == "local" {
                    eck_core::ai::AiAuth::studio(String::new())
                } else {
                    if let Err(e) = resolved {
                        warn!("[Embeddings] token resolve failed: {e}");
                    }
                    idle_cycles = (idle_cycles + 1).min(IDLE_BACKOFF_CAP); // not configured / error — back off
                    continue;
                }
            }
        };

        let mut found_total = 0usize;
        for table in &["document", "order", "partner", "product", "picking"] {
            let mut attempts = 0;
            loop {
                attempts += 1;
                match process_table(&db, &http, &auth, table, &gen_model, &emb_model, &guard, &instance_id).await {
                    Ok(found) => {
                        found_total += found;
                        break;
                    }
                    Err(e) if e.to_string().contains("Transaction conflict") && attempts <= 3 => {
                        warn!("[Embeddings] {table} write conflict, retry {attempts}/3");
                        tokio::time::sleep(std::time::Duration::from_millis(500 * attempts as u64)).await;
                    }
                    Err(e) => {
                        warn!("[Embeddings] {table} cycle error: {e}");
                        break;
                    }
                }
            }
        }

        eck_core::metrics::add(eck_core::metrics::M::EmbeddingRows, found_total as u64);
        if found_total > 0 {
            if idle_cycles > 0 {
                info!("[Embeddings] work resumed ({found_total} pending) — polling every {LOOP_INTERVAL_SECS}s");
            }
            idle_cycles = 0;
        } else {
            let was_active = idle_cycles == 0;
            idle_cycles = (idle_cycles + 1).min(IDLE_BACKOFF_CAP);
            if was_active {
                info!(
                    "[Embeddings] no pending rows across all tables — backing off polling toward {}s idle",
                    LOOP_INTERVAL_SECS + IDLE_BACKOFF_CAP * LOOP_INTERVAL_SECS
                );
            }
        }
    }
}

/// Requeue retryable rows (error/skipped/paused, plus Observer-killed
/// zombies) across all embedded tables. Runs at worker start and every ~6h
/// so transient-failure victims self-heal without a process restart.
async fn reset_retryable(db: &SurrealDb) {
    for table in &["document", "order", "partner", "product", "picking"] {
        // Seed rows that never entered the pipeline (embedding_status absent —
        // e.g. scraper/migrator imports that predate embedding, or import
        // paths that don't set 'pending'). Policy: everything that helps run
        // the workshop/warehouse gets embedded — orders, partners/addresses,
        // products, pickings, and summarized documents. Raw conversation
        // bodies (`support_thread`) are deliberately NOT embedded: only the
        // ticket-level summaries are (2026-07-12 decision).
        let thread_filter = if *table == "document" {
            " AND type != 'support_thread'"
        } else {
            ""
        };
        let seed = db
            .query(&format!(
                "UPDATE {table} SET \
                    embedding_status = 'pending', \
                    embedding_retries = 0 \
                 WHERE embedding_status IS NONE AND embedding IS NONE{thread_filter} \
                 RETURN NONE"
            ))
            .await;
        if let Err(e) = seed {
            warn!("[Embeddings] Failed to seed {table}: {e}");
        }

        // Adopt mesh-arrived vectors: a row whose vector synced in from the
        // authoring node may still carry stale LOCAL bookkeeping ('pending',
        // 'error', 'unavailable' — status fields are hash-ignored and don't
        // sync on their own). Vector present = done; align the local status
        // so health views and requeue sweeps see the truth.
        let adopt = db
            .query(&format!(
                "UPDATE {table} SET \
                    embedding_status = 'complete', \
                    embedding_error = NONE \
                 WHERE embedding IS NOT NONE AND embedding_status != 'complete' \
                 RETURN NONE"
            ))
            .await;
        if let Err(e) = adopt {
            warn!("[Embeddings] Failed to adopt synced vectors on {table}: {e}");
        }

        let reset = db
            .query(&format!(
                "UPDATE {table} SET \
                    embedding_status = 'pending', \
                    embedding_retries = IF embedding_status = 'paused_by_observer' THEN 0 ELSE embedding_retries END, \
                    embedding_error = NONE \
                 WHERE embedding_status IN ['error', 'skipped', 'paused_by_observer'] \
                 AND (embedding_retries IS NONE OR embedding_retries < {MAX_RETRIES}) \
                 RETURN NONE"
            ))
            .await;
        if let Err(e) = reset {
            warn!("[Embeddings] Failed to reset {table}: {e}");
        }

        // Resurrect Observer-killed zombies (retries=99, status='failed') —
        // legacy of the 2026-04-21 loop mitigation. Loop root cause is fixed;
        // give them another MAX_RETRIES window. If a real loop recurs,
        // Observer will re-mitigate.
        let resurrect = db
            .query(&format!(
                "UPDATE {table} SET \
                    embedding_status = 'pending', \
                    embedding_retries = 0, \
                    embedding_error = NONE \
                 WHERE embedding_status = 'failed' AND embedding_retries = 99 \
                 RETURN NONE"
            ))
            .await;
        if let Err(e) = resurrect {
            warn!("[Embeddings] Failed to resurrect {table} zombies: {e}");
        }
    }
}

async fn process_table(
    db: &SurrealDb,
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    table: &str,
    gen_model: &str,
    emb_model: &str,
    guard: &LoopGuard,
    instance_id: &str,
) -> Result<usize, anyhow::Error> {
    // ── Circuit breaker check ──
    match current_budget_level() {
        BudgetLevel::Halt => return Ok(0), // complete stop
        BudgetLevel::Throttle => {
            tokio::time::sleep(std::time::Duration::from_secs(THROTTLE_DELAY_SECS)).await;
        }
        _ => {}
    }

    // Exponential backoff: docs with N retries wait 2^N minutes before next attempt.
    // Hard cap at MAX_RETRIES — after that the record stays as 'error' permanently.
    let backoff_filter = format!(
        "AND (embedding_retries IS NONE OR embedding_retries < {MAX_RETRIES}) \
         AND (embedding_retries IS NONE OR embedding_retries = 0 \
              OR updated_at IS NONE \
              OR time::now() > updated_at + type::duration(string::concat(math::pow(2, embedding_retries ?? 0), 'm')))"
    );
    // `embedding IS NONE` is what makes vectors safe to mesh-sync: a row whose
    // vector already arrived from the authoring node is never re-embedded here,
    // so the fleet converges on exactly one vector per row instead of N
    // divergent ones (the pre-2026-07 non-convergence). Re-embedding is
    // requested by NULLING the vector (a synced, hash-visible signal) — see
    // the summarization worker's re-queue write.
    let query = if table == "document" {
        format!(
            "SELECT record::id(id) AS id, type, status, meta, ai_summary, ticket_id, embedding_status, \
             (SELECT payload FROM document_raw WHERE record::id(id) = record::id($parent.id) LIMIT 1)[0].payload AS payload \
             FROM {table} WHERE embedding_status = 'pending' AND embedding IS NONE {backoff_filter} LIMIT {BATCH_LIMIT}"
        )
    } else {
        format!(
            "SELECT record::id(id) AS id, `payload`, type, ai_summary, name, description, email, phone, order_number, embedding_status \
             FROM {table} WHERE embedding_status = 'pending' AND embedding IS NONE {backoff_filter} LIMIT {BATCH_LIMIT}"
        )
    };

    let rows: Vec<Value> = db
        .query(&query)
        .await?
        .take(0)?;

    if rows.is_empty() {
        return Ok(0);
    }

    // Local mode: on-device embedding, no cloud calls. Decided once per cycle
    // (the env var can't change mid-process).
    let local = embed_mode() == "local";
    // Model name stamped onto every vector written this cycle (and used for
    // telemetry naming). Identical to `emb_model` in cloud mode.
    let db_model = effective_embed_model(emb_model);

    let mut count = 0u32;
    for row in &rows {
        let id = match row.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let guard_key = format!("{table}:{id}");
        if !guard.check_and_record(&guard_key) {
            info!("[Embeddings] loop_guard: skipping {guard_key} (cooldown)");
            continue;
        }

        // Tiered PII policy (ai::pii_policy): effective-clear embeddings skip
        // every masking pass below and embed raw field values — either the
        // backend is local (nothing leaves the box) or the customer explicitly
        // opted out (no-secrets mesh, wants recall).
        let clear = crate::ai::pii_policy::effective_clear(crate::ai::pii_policy::Surface::Embedding);
        let text = build_embedding_text(table, row, clear);
        if text.is_empty() {
            db.query(&format!(
                "UPDATE type::record($rid) SET embedding_status = 'skipped', embedding_retries = (embedding_retries ?? 0) + 1, \
                 updated_at = time::now()"
            ))
            .bind(("rid", format!("{table}:`{id}`")))
            .await?
            .check()?;
            continue;
        }

        // Anonymize PII before embedding (all passes skipped in clear mode).
        let (mut anonymized_text, mut fingerprints) = match table {
            _ if clear => (text.clone(), vec![]),
            // Local mode skips the Gemini-based extract_and_anonymize pass: the
            // deterministic field masking already ran in build_embedding_text and
            // the scrub_pii_regex egress filter below still runs, so on-device
            // vectors land in the same masked space as cloud ones. In local mode
            // the "document" arm therefore falls through to the raw-text default.
            "document" if !local => {
                match extract_and_anonymize(http, auth, &text, gen_model).await {
                    Ok(result) => result,
                    Err(e) => {
                        debug!("[Embeddings] PII extraction failed for {table}:{id}, using raw text: {e}");
                        (text.clone(), vec![])
                    }
                }
            }
            "order" => anonymize_order_fields(row, &text),
            "partner" => anonymize_partner_fields(row, &text),
            "picking" => anonymize_picking_fields(row, &text),
            _ => (text.clone(), vec![]),
        };

        if !clear {
            // Final deterministic egress filter: nothing matching a high-confidence
            // PII pattern (email/phone/IBAN/card/VAT-Id) reaches the cloud embedder,
            // regardless of path — including the extractor-failure fallback above
            // (which would otherwise embed RAW text) and the structured order/
            // partner/picking paths that only mask known name fields.
            let (scrubbed, mut egress_fps) = scrub_pii_regex(&anonymized_text);
            anonymized_text = scrubbed;
            fingerprints.append(&mut egress_fps);
        }
        // Tokens already BAKED into the source text (masked summaries carry
        // Name_/Email_ tokens from summarization; build_embedding_text
        // tokenizes the meta identity fields inline) never went through the
        // extractor/scrub, so they were MISSING from `pii_fingerprints` —
        // which silently excluded document rows from GDPR erasure matching
        // (`gdpr::erase` matches CONTAINSANY on this array) and from exact
        // token search. Union them in.
        let mut baked = eck_core::utils::anonymizer::extract_pii_tokens(&anonymized_text);
        fingerprints.append(&mut baked);
        // Identity-field tokens are ALWAYS derived for document rows — even on
        // clear meshes, where the text carries no tokens at all: they are the
        // GDPR-erasure index key (`gdpr::erase` CONTAINSANY) and the exact
        // token-search key, both of which must keep working regardless of the
        // egress policy. Pure local hashing, nothing leaves the node.
        if table == "document" {
            if let Some(meta) = row.get("meta") {
                for (field, label) in [
                    ("customer", "Name"),
                    ("email", "Email"),
                    ("phone", "Phone"),
                    ("address", "Address"),
                ] {
                    if let Some(v) = meta.get(field).and_then(|v| v.as_str()) {
                        if !v.trim().is_empty() {
                            fingerprints.push(obfuscate_pii(v.trim(), label));
                        }
                    }
                }
            }
        }
        fingerprints.sort();
        fingerprints.dedup();

        let record_id = format!("{table}:`{id}`");
        match embed_dispatch(http, auth, emb_model, &anonymized_text, local).await {
            Ok((embedding, usage)) => {
                // `embedding_model` travels with the vector (both are hashed
                // content) so a future model migration can find and requeue
                // foreign-model vectors instead of silently mixing spaces. Local
                // mode stamps the on-device model id (see effective_embed_model).
                let updated: Vec<Value> = db.query(&format!(
                    "UPDATE type::record($rid) SET embedding = $emb, embedding_model = $model, \
                         embedding_status = 'complete', \
                         embedding_error = NONE, pii_fingerprints = $fingerprints \
                     RETURN record::id(id) AS id"
                ))
                .bind(("rid", record_id.clone()))
                .bind(("emb", embedding))
                .bind(("model", db_model.clone()))
                .bind(("fingerprints", fingerprints))
                .await?
                .take(0)?;

                // Telemetry logs only when something was billed. Local mode
                // returns a null usage (nothing billed) so this is skipped.
                if !usage.is_null() {
                    log_telemetry(db, "embedding", &db_model, &record_id, &usage).await;
                }

                if updated.is_empty() {
                    // Same infinite-loop trap as summarization — see that module's comment.
                    warn!("[Embeddings] success-path UPDATE matched 0 rows for {record_id} — forcing retry counter");
                    let forced: Vec<Value> = db.query(&format!(
                        "UPDATE {table} SET \
                             embedding_status = 'error', \
                             embedding_error = 'phantom update: type::record matched 0 rows', \
                             embedding_retries = (embedding_retries ?? 0) + 1, \
                             updated_at = time::now() \
                         WHERE record::id(id) = $id AND embedding_status = 'pending' \
                         RETURN record::id(id) AS id"
                    ))
                    .bind(("id", id.clone()))
                    .await?
                    .take(0)?;
                    if forced.is_empty() {
                        warn!("[Embeddings] fallback UPDATE also matched 0 rows for {record_id} — record truly missing");
                    }
                } else {
                    count += 1;
                    guard.clear(&guard_key);
                    // `embedding` is part of the content hash (merkle) —
                    // advance this node's vclock so the fresh vector strictly
                    // dominates a peer's vector-less copy (same pattern as the
                    // summarization worker's ai_summary bump, d4c8559).
                    if let Err(e) = eck_core::sync::conflict::bump_local_vclock_by_leaf(
                        db, table, &id, instance_id,
                    ).await {
                        warn!("[Embeddings] vclock bump failed for {record_id}: {e}");
                    }
                    info!("[Embeddings] Embedded {record_id}");
                }
            }
            Err(e) => {
                let es = e.to_string();
                // The embedding BACKEND is unavailable (managed authority built
                // without a text-brain / embedding model). This is not a
                // per-document failure — retrying every doc forever just burns
                // the retry budget and trips the Observer's "stuck documents"
                // alert. Park the doc terminally as 'unavailable' (no retry
                // bump, not 'pending' so it won't be re-selected or alerted);
                // it re-embeds only when explicitly re-queued after the backend
                // gains embedding support.
                let backend_missing = es.contains("text-brain")
                    || es.contains("embedding requires")
                    || es.contains("503");
                if backend_missing {
                    // Backend has no embedding model at all → park the ENTIRE
                    // backlog for this table in one sweep (not one-by-one: the
                    // high-retry docs are past the backoff cap and would never
                    // be re-selected, so they'd loop through the Observer's
                    // "stuck" alert forever). One bulk UPDATE clears pending +
                    // error, leaving the pending pool empty so the alert stops.
                    // A re-queue revives them once the backend gains embeddings.
                    warn!("[Embeddings] {table}: embedding backend unavailable ({es}) — parking the whole pending/error backlog as 'unavailable'");
                    db.query(&format!(
                        "UPDATE {table} SET embedding_status = 'unavailable', embedding_error = $err, updated_at = time::now() \
                         WHERE embedding_status IN ['pending', 'error']"
                    ))
                    .bind(("err", es))
                    .await?
                    .check()?;
                    return Ok(rows.len());
                } else {
                    warn!("[Embeddings] Failed to embed {record_id}: {e}");
                    db.query(
                        "UPDATE type::record($rid) SET embedding_status = 'error', embedding_error = $err, \
                         embedding_retries = (embedding_retries ?? 0) + 1, updated_at = time::now()",
                    )
                    .bind(("rid", record_id.clone()))
                    .bind(("err", es))
                    .await?
                    .check()?;
                }
            }
        }

        // Rate-limit between API calls
        tokio::time::sleep(std::time::Duration::from_millis(RATE_LIMIT_MS)).await;
    }

    if count > 0 {
        info!("[Embeddings] {table}: {count} records embedded");
    }

    // Return the count of candidate rows found this cycle (not just successfully
    // embedded) so the worker's idle backoff treats "found rows but all errored/
    // skipped" as active and keeps polling at the base interval.
    Ok(rows.len())
}

// ── PII Anonymization ─────────────────────────────────────────────

/// Use Gemini to extract PII from unstructured document text, then replace with SimHash tokens.
///
/// `pub(crate)` so the on-device NER eval harness (`ai::local_ner`) can compare
/// its PER entities against this cloud path side by side.
pub(crate) async fn extract_and_anonymize(
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    text: &str,
    model: &str,
) -> Result<(String, Vec<String>), anyhow::Error> {
    // On-device path (ECK_PII_NER=local): run the deterministic regex backstop
    // + the local German NER for names, skip Gemini entirely. Streets are masked
    // by scrub_pii_regex's Address pattern; LOC/ORG are intentionally left in the
    // clear. On any local NER error we fall through to the Gemini path below
    // (never fail the document). Compiled out — and thus byte-identical to the
    // old cloud-only behaviour — when the feature or env is off.
    #[cfg(feature = "local-embed")]
    if crate::ai::local_ner::ner_mode() == "local" {
        match local_extract_and_anonymize(text).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                warn!("[Embeddings] local NER extraction failed ({e}) — falling back to Gemini");
            }
        }
    }

    // Deterministic regex backstop FIRST: emails/phones/IBANs/cards/VAT-Ids are
    // masked before the raw text is ever embedded in the cloud extraction
    // prompt below. The LLM then only has to find names/addresses the regex
    // can't (those have no fixed shape). `fingerprints` accumulates both layers.
    let (text, mut regex_fps) = scrub_pii_regex(text);
    let text = text.as_str();

    let prompt = format!(
        "Analyze the following support ticket text. Extract personal names and street addresses. \
         DO NOT extract cities, countries, or zip codes. Summarize the technical issue. \
         Return ONLY a valid JSON object with this structure: \
         {{ \"summary\": \"<cleaned summary>\", \"entities\": [ {{ \"original\": \"<extracted text>\", \"type\": \"Name\" | \"Address\" }} ] }}\n\n\
         Text:\n{text}"
    );

    let payload = serde_json::json!({
        "contents": [{ "parts": [{ "text": prompt }] }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "temperature": 0.1,
            "maxOutputTokens": 2048,
        }
    });

    let (response_text, usage) = auth.generate_content(http, model, payload).await?;

    // Log PII extraction usage (generation model call)
    if !usage.is_null() {
        debug!("[Embeddings] PII extraction usage: {usage}");
    }

    let parsed: Value = serde_json::from_str(&response_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse PII extraction JSON: {e}"))?;

    let _summary = parsed["summary"]
        .as_str()
        .unwrap_or(text)
        .to_string();

    let entities = parsed["entities"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let mut anonymized = text.to_string();
    let mut fingerprints = Vec::new();
    fingerprints.append(&mut regex_fps);

    for entity in &entities {
        let original = match entity["original"].as_str() {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let pii_type = entity["type"].as_str().unwrap_or("PII");
        let token = obfuscate_pii(original, pii_type);
        anonymized = replace_pii_ci(&anonymized, original, &token);
        fingerprints.push(token);
    }

    Ok((anonymized, fingerprints))
}

/// On-device replacement for `extract_and_anonymize` (ECK_PII_NER=local). Runs
/// the same deterministic `scrub_pii_regex` backstop as the cloud path (now also
/// masking German street+house-number addresses), then masks person names found
/// by the local German NER with the identical `obfuscate_pii`/`replace_pii_ci`
/// tokens — so on-device summaries land in the same masked space as cloud ones,
/// with zero Gemini calls. LOC/ORG entities are deliberately NOT masked (cities
/// stay in the clear for the embedding's coarse-geo signal).
#[cfg(feature = "local-embed")]
async fn local_extract_and_anonymize(text: &str) -> Result<(String, Vec<String>), anyhow::Error> {
    let (mut anonymized, mut fingerprints) = scrub_pii_regex(text);
    let names = crate::ai::local_ner::extract_person_names(&anonymized).await?;
    for name in names {
        let token = obfuscate_pii(&name, "Name");
        anonymized = replace_pii_ci(&anonymized, &name, &token);
        fingerprints.push(token);
    }
    Ok((anonymized, fingerprints))
}

/// Case-insensitive, whitespace-tolerant replacement of a PII entity with its
/// token. The extraction LLM frequently echoes an entity with different casing
/// or collapsed/extra whitespace than the source text (e.g. returns "Hans
/// Müller" while the body has "hans  müller"); a plain `str::replace` then
/// silently no-ops and the raw PII survives into the embed text. We build a
/// regex where each whitespace run in `needle` matches `\s+` and everything
/// else is escaped, matched case-insensitively. Falls back to plain replace if
/// the pattern fails to compile.
fn replace_pii_ci(haystack: &str, needle: &str, token: &str) -> String {
    let needle = needle.trim();
    if needle.is_empty() {
        return haystack.to_string();
    }
    let pattern = needle
        .split_whitespace()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"\s+");
    if pattern.is_empty() {
        return haystack.to_string();
    }
    match regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
    {
        Ok(re) => re.replace_all(haystack, token).into_owned(),
        Err(_) => haystack.replace(needle, token),
    }
}

/// Anonymize structured order fields (customer_name) directly — no LLM needed.
fn anonymize_order_fields(row: &Value, text: &str) -> (String, Vec<String>) {
    let mut result = text.to_string();
    let mut fingerprints = Vec::new();

    if let Some(name) = row.get("customer_name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            let token = obfuscate_pii(name, "Name");
            result = result.replace(name, &token);
            fingerprints.push(token);
        }
    }

    (result, fingerprints)
}

/// Anonymize structured partner fields (name, street) directly.
fn anonymize_partner_fields(row: &Value, text: &str) -> (String, Vec<String>) {
    let mut result = text.to_string();
    let mut fingerprints = Vec::new();

    if let Some(name) = row.get("name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            let token = obfuscate_pii(name, "Name");
            result = result.replace(name, &token);
            fingerprints.push(token);
        }
    }
    if let Some(street) = row.get("street").and_then(|v| v.as_str()) {
        if !street.is_empty() {
            let token = obfuscate_pii(street, "Address");
            result = result.replace(street, &token);
            fingerprints.push(token);
        }
    }

    (result, fingerprints)
}

/// Anonymize structured picking fields (recipient_name) directly.
fn anonymize_picking_fields(row: &Value, text: &str) -> (String, Vec<String>) {
    let mut result = text.to_string();
    let mut fingerprints = Vec::new();

    if let Some(name) = row.get("recipient_name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            let token = obfuscate_pii(name, "Name");
            result = result.replace(name, &token);
            fingerprints.push(token);
        }
    }

    (result, fingerprints)
}

// ── Embedding text construction ───────────────────────────────────

/// Build text for embedding by concatenating relevant fields based on table type.
/// Replace the street address on the "**Adressen:**" line of an ai_summary
/// with an Address_<SimHash> token. city/zip on the same line stay visible —
/// they're coarse geo, not PII. Returns the summary unchanged when no
/// Adressen block is found (free-form summaries, non-ticket docs).
///
/// The heuristic: grab the first non-empty, non-own-HQ line after
/// "**Adressen:**", strip any trailing "(ermittelt via ...)" parenthetical,
/// then extract the street portion (= everything before the 5-digit zip)
/// and mask only that. When parse_zip_city fails — e.g. the model returned
/// a non-German address — mask the whole line as a fallback.
fn mask_ai_summary_address(summary: &str) -> String {
    mask_ai_summary_address_with(summary, eck_core::utils::anonymizer::own_address_markers())
}

/// Inner form taking the own-company address markers explicitly (env
/// `ECK_OWN_ADDRESS_MARKERS` in production; injected in tests). EMPTY markers =
/// skip nothing.
fn mask_ai_summary_address_with(summary: &str, own_markers: &[String]) -> String {
    let marker = "**Adressen:**";
    let Some(marker_pos) = summary.find(marker) else { return summary.to_string(); };
    let after = &summary[marker_pos + marker.len()..];

    for raw_line in after.lines() {
        let clean = raw_line.trim().trim_start_matches('-').trim_start_matches('*').trim();
        if clean.is_empty() { continue; }
        if clean.starts_with("===") { break; }
        let lower = clean.to_lowercase();
        // Skip the operating company's OWN HQ / office address — not customer
        // PII. Markers come from env `ECK_OWN_ADDRESS_MARKERS` (empty = skip
        // nothing).
        if own_markers.iter().any(|m| lower.contains(m.as_str())) {
            continue;
        }

        // Drop trailing "(ermittelt via Google Search)" / "[quelle ...]" notes.
        let mut final_addr = clean;
        if let Some(idx) = final_addr.rfind('(').or_else(|| final_addr.rfind('[')) {
            let inside = final_addr[idx..].to_lowercase();
            if inside.contains("ermittelt")
                || inside.contains("domain")
                || inside.contains("homepage")
                || inside.contains("google")
                || inside.contains("suche")
                || inside.contains("internet")
                || inside.contains("website")
            {
                final_addr = final_addr[..idx].trim();
            }
        }
        if final_addr.is_empty() { continue; }

        // Street = everything before the 5-digit zip. If parser fails, mask
        // the whole line so nothing PII-shaped slips through untouched.
        let (zip_opt, _city_opt) = crate::services::support::parse_zip_city(final_addr);
        let street_part = match zip_opt.as_deref() {
            Some(zip) => final_addr.find(zip).map(|i| {
                final_addr[..i]
                    .trim_end_matches(|c: char| c.is_whitespace() || c == ',')
                    .to_string()
            }),
            None => None,
        };
        let to_mask = match street_part {
            Some(s) if !s.is_empty() => s,
            _ => final_addr.to_string(),
        };
        let token = obfuscate_pii(&to_mask, "Address");
        return summary.replacen(&to_mask, &token, 1);
    }
    summary.to_string()
}

/// `clear` = effective-clear embedding policy (ai::pii_policy): identity
/// fields go in RAW instead of as SimHash tokens — recall over pseudonymity,
/// by mesh-wide decision or because the embedder is local.
fn build_embedding_text(table: &str, row: &Value, clear: bool) -> String {
    let mut parts = Vec::new();

    match table {
        "document" => {
            let doc_type = row.get("type").and_then(|v| v.as_str()).unwrap_or("");

            // repair_folder: build from structured meta fields
            if doc_type == "repair_folder" {
                if let Some(meta) = row.get("meta") {
                    let get = |k: &str| meta.get(k).and_then(|v| v.as_str()).unwrap_or("");
                    parts.push(format!("Repair: {}", get("repair_number")));
                    if !get("company").is_empty() { parts.push(format!("Company: {}", get("company"))); }
                    if !get("city").is_empty() { parts.push(format!("City: {}", get("city"))); }
                    if !get("model").is_empty() { parts.push(format!("Device: {}", get("model"))); }
                    if !get("serial_number").is_empty() { parts.push(format!("Serial: {}", get("serial_number"))); }
                    if !get("warranty").is_empty() { parts.push(format!("Warranty: {}", if get("warranty") == "J" { "Yes" } else { "No" })); }
                    if !get("status").is_empty() { parts.push(format!("Status: {}", get("status"))); }
                    if !get("error_description").is_empty() { parts.push(format!("Error: {}", get("error_description"))); }
                    if let Some(dp) = meta.get("defective_parts").and_then(|v| v.as_array()) {
                        let parts_list: Vec<&str> = dp.iter().filter_map(|v| v.as_str()).collect();
                        if !parts_list.is_empty() {
                            parts.push(format!("Defective parts: {}", parts_list.join(", ")));
                        }
                    }
                    // Subfolder inventory summary
                    if let Some(subs) = meta.get("subfolders") {
                        let mut inv = Vec::new();
                        for (key, label) in [
                            ("fotos", "Photos"), ("qc_report", "QC reports"), ("auftrag", "Repair order"),
                            ("rechnung", "Invoice"), ("kv_garantie", "KV/Warranty"), ("kv_signed", "KV signed"),
                            ("dsgvo", "DSGVO"), ("emails", "Emails"),
                        ] {
                            let n = subs.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
                            if n > 0 { inv.push(format!("{label}: {n}")); }
                        }
                        if !inv.is_empty() { parts.push(format!("Documents: {}", inv.join(", "))); }
                    }
                    if !get("ticket_number").is_empty() { parts.push(format!("Zoho ticket: #{}", get("ticket_number"))); }
                }
                let full = parts.join("\n");
                return if full.chars().count() > 8000 { full.chars().take(8000).collect() } else { full };
            }

            // support_ticket: embed structured fields with deterministic PII
            // masking up-front. Everything we already know is PII (customer
            // name, street address, email, phone) gets replaced with a
            // SimHash token BEFORE the LLM-based `extract_and_anonymize`
            // pass — so raw PII never ships to Gemini, tokens are stable
            // across runs, and identical street addresses from different
            // sources (meta vs. ai_summary) collapse to the same token.
            // City + zip stay in the clear: they're coarse geo, not PII, and
            // carry useful semantics for the embedding.
            if doc_type == "support_ticket" {
                if let Some(meta) = row.get("meta") {
                    let get = |k: &str| meta.get(k).and_then(|v| v.as_str()).unwrap_or("").trim();
                    // Identity fields: SimHash token by default, RAW under the
                    // clear embedding policy (recall over pseudonymity).
                    let ident = |v: &str, label: &str| -> String {
                        if clear { v.to_string() } else { obfuscate_pii(v, label) }
                    };
                    if !get("ticket_number").is_empty() { parts.push(format!("Ticket #{}", get("ticket_number"))); }
                    if !get("subject").is_empty() { parts.push(format!("Subject: {}", get("subject"))); }
                    if !get("status").is_empty() { parts.push(format!("Status: {}", get("status"))); }
                    if !get("customer").is_empty() {
                        parts.push(format!("Customer: {}", ident(get("customer"), "Name")));
                    }
                    if !get("company").is_empty() { parts.push(format!("Company: {}", get("company"))); }
                    if !get("email").is_empty() {
                        parts.push(format!("Email: {}", ident(get("email"), "Email")));
                    }
                    if !get("phone").is_empty() {
                        parts.push(format!("Phone: {}", ident(get("phone"), "Phone")));
                    }
                    if !get("device_model").is_empty() { parts.push(format!("Device: {}", get("device_model"))); }
                    if !get("serial_number").is_empty() { parts.push(format!("Serial: {}", get("serial_number"))); }
                    if !get("address").is_empty() {
                        parts.push(format!("Address: {}", ident(get("address"), "Address")));
                    }
                    if !get("city").is_empty() { parts.push(format!("City: {}", get("city"))); }
                    if !get("zip").is_empty() { parts.push(format!("Zip: {}", get("zip"))); }
                }
                // ai_summary carries a "**Adressen:**" block when the model
                // used Google Search to resolve an incomplete address. Mask
                // that street line with the same Address_<hash> token so the
                // open-web-sourced address never reaches the embedder (as-is
                // under the clear policy — the summary is the LLM surface's
                // output and follows ITS policy at write time).
                if let Some(s) = row.get("ai_summary").and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        let s = if clear { s.to_string() } else { mask_ai_summary_address(s) };
                        parts.push(format!("Summary: {s}"));
                    }
                }
                let full = parts.join("\n");
                let end = crate::ai::floor_char_boundary(&full, 8000);
                return full[..end].to_string();
            }

            // support_thread: embed the actual thread content
            if doc_type == "support_thread" {
                if let Some(tid) = row.get("ticket_id").and_then(|v| v.as_str()) {
                    parts.push(format!("Thread for ticket {tid}"));
                }
                if let Some(payload) = row.get("payload") {
                    if let Some(s) = payload.get("fromEmailAddress").and_then(|v| v.as_str()) {
                        parts.push(format!("From: {s}"));
                    }
                    if let Some(s) = payload.get("to").and_then(|v| v.as_str()) {
                        parts.push(format!("To: {s}"));
                    }
                    if let Some(s) = payload.get("summary").and_then(|v| v.as_str()) {
                        if !s.is_empty() { parts.push(format!("Subject: {s}")); }
                    }
                    if let Some(s) = payload.get("content").and_then(|v| v.as_str()) {
                        let plain = strip_html(s);
                        if !plain.is_empty() { parts.push(plain); }
                    } else if let Some(s) = payload.get("plainText").and_then(|v| v.as_str()) {
                        if !s.is_empty() { parts.push(s.to_string()); }
                    }
                }
                let full = parts.join("\n");
                return if full.chars().count() > 8000 { full.chars().take(8000).collect() } else { full };
            }

            // Fallback for other document types
            if let Some(s) = row.get("type").and_then(|v| v.as_str()) {
                parts.push(format!("Type: {s}"));
            }
            if let Some(s) = row.get("status").and_then(|v| v.as_str()) {
                parts.push(format!("Status: {s}"));
            }
            if let Some(payload) = row.get("payload") {
                if let Some(s) = payload.get("subject").and_then(|v| v.as_str()) {
                    parts.push(format!("Subject: {s}"));
                }
                if let Some(s) = payload.get("content").and_then(|v| v.as_str()) {
                    let plain = strip_html(s);
                    if !plain.is_empty() {
                        parts.push(plain);
                    }
                }
            }
        }
        "order" => {
            for (key, label) in [
                ("order_number", "Order"),
                ("customer_name", "Customer"),
                ("product_name", "Product"),
                ("serial_number", "Serial"),
                ("issue_description", "Issue"),
                ("diagnosis_notes", "Diagnosis"),
                ("repair_notes", "Repair"),
                ("resolution", "Resolution"),
            ] {
                if let Some(s) = row.get(key).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        parts.push(format!("{label}: {s}"));
                    }
                }
            }
        }
        "partner" => {
            for (key, label) in [
                ("name", "Firma/Name"),
                ("email", "Email"),
                ("phone", "Telefon"),
                ("street", "Adresse"),
                ("zip", "PLZ"),
                ("city", "Stadt"),
                ("vat", "USt-IdNr"),
            ] {
                if let Some(s) = row.get(key).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        parts.push(format!("{label}: {s}"));
                    }
                }
            }
        }
        "product" => {
            for (key, label) in [
                ("name", "Produkt"),
                ("default_code", "SKU"),
                ("barcode", "Barcode"),
            ] {
                if let Some(s) = row.get(key).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        parts.push(format!("{label}: {s}"));
                    }
                }
            }
        }
        "picking" => {
            for (key, label) in [
                ("tracking_number", "Tracking"),
                ("recipient_name", "Empfänger"),
                ("recipient_city", "Stadt"),
                ("status", "Status"),
                ("picking_type", "Typ"),
                ("origin", "Herkunft"),
            ] {
                if let Some(s) = row.get(key).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        parts.push(format!("{label}: {s}"));
                    }
                }
            }
        }
        _ => {}
    }

    let full = parts.join("\n");
    // Truncate to ~8000 bytes (char-boundary-safe) to stay within embedding model limits
    let end = crate::ai::floor_char_boundary(&full, 8000);
    full[..end].to_string()
}

// ── Embedding API ─────────────────────────────────────────────────

/// Embed a search query using Gemini. Anonymizes PII before embedding to match the database vectors. Returns 768-dim vector.
pub async fn embed_query(text: &str) -> Result<Vec<f32>, anyhow::Error> {
    // Local mode: embed on-device with no cloud auth and no GEMINI_* env — a
    // local-only node may have none set. The query still passes through the
    // deterministic PII egress filter so it lands in the SAME masked space as
    // the stored document vectors (the LLM-based extract pass is skipped).
    #[cfg(feature = "local-embed")]
    if embed_mode() == "local" {
        let (scrubbed, _fingerprints) = scrub_pii_regex(text);
        return super::local_embed::embed(&scrubbed).await;
    }

    let http = HttpClient::new();
    let auth = eck_core::ai::AiAuth::resolve(&http).await?;
    if !auth.is_configured() {
        anyhow::bail!("AI auth not configured ({} mode)", auth.mode());
    }

    // GEMINI_* are read only on the cloud path — local mode returned above.
    let gen_model = std::env::var("GEMINI_GENERATION_MODEL")
        .expect("GEMINI_GENERATION_MODEL must be set in .env");
    let emb_model = std::env::var("GEMINI_EMBEDDING_MODEL")
        .expect("GEMINI_EMBEDDING_MODEL must be set in .env");

    // 1. Anonymize the search query (extract PII to Keyed SimHash tokens)
    let (anonymized_query, _) = match extract_and_anonymize(&http, &auth, text, &gen_model).await {
        Ok(res) => {
            tracing::debug!("[Embeddings] Query anonymized: {}", res.0);
            res
        },
        Err(e) => {
            tracing::warn!("[Embeddings] Failed to anonymize search query, falling back to raw text: {}", e);
            (text.to_string(), vec![])
        }
    };

    // 2. Embed the anonymized query
    let (embedding, _usage) = auth.embed_content(&http, &emb_model, &anonymized_query, EMBEDDING_DIM).await?;
    Ok(embedding)
}

/// Naive HTML tag stripper.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Collapse whitespace
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_embed_mode_parses_env_without_global_state() {
        // Pure parser — no env mutation, so it can't race other tests. With the
        // default feature set `local-embed` is compiled in, so "local" resolves
        // to local; a cloud-only build resolves everything to cloud.
        let local_expected = if cfg!(feature = "local-embed") { "local" } else { "cloud" };
        assert_eq!(resolve_embed_mode(None), "cloud");
        assert_eq!(resolve_embed_mode(Some("")), "cloud");
        assert_eq!(resolve_embed_mode(Some("cloud")), "cloud");
        assert_eq!(resolve_embed_mode(Some("gemini")), "cloud");
        assert_eq!(resolve_embed_mode(Some("local")), local_expected);
        assert_eq!(resolve_embed_mode(Some("  LOCAL  ")), local_expected);
    }

    #[test]
    fn effective_embed_model_switches_on_mode() {
        // Pure mapping keyed on the mode argument — no global state.
        assert_eq!(
            effective_embed_model_for("cloud", "gemini-embedding-2"),
            "gemini-embedding-2"
        );
        assert_eq!(
            effective_embed_model_for("local", "gemini-embedding-2"),
            "jina-embeddings-v2-base-de"
        );
    }

    /// End-to-end on-device embed. Ignored: it downloads ~322 MB of weights on
    /// first run and needs the `local-embed` feature. Run explicitly with:
    ///   cargo test -p wms --bin wms -- --ignored local_embed --nocapture
    #[cfg(feature = "local-embed")]
    #[tokio::test]
    #[ignore]
    async fn local_embed_produces_normalized_768d_vector() {
        // ~520-char German support-ticket-style sentence.
        let sentence = "Der ZetaBody 770 Körperanalyse-Automat im Fitnessstudio zeigt beim \
            Einschalten sofort einen Fehlercode auf dem Display an und bricht die Messung \
            nach wenigen Sekunden mit einem lauten Klicken wieder ab. Der Betreiber \
            berichtet, dass das Gerät zuvor mehrfach neu gestartet und die Steckverbindungen \
            geprüft wurden, ohne dass sich das Verhalten geändert hat. Er bittet dringend um \
            eine schnelle Diagnose und Reparatur, da das Gerät täglich im Dauereinsatz \
            benötigt wird und der Ausfall den Betrieb erheblich stört.";
        assert!(
            sentence.chars().count() >= 500,
            "test sentence must be ~500 chars, got {}",
            sentence.chars().count()
        );

        let t0 = std::time::Instant::now();
        let a = crate::ai::local_embed::embed(sentence)
            .await
            .expect("first embed failed");
        let first_ms = t0.elapsed().as_millis();

        let t1 = std::time::Instant::now();
        let b = crate::ai::local_embed::embed(sentence)
            .await
            .expect("second embed failed");
        let second_ms = t1.elapsed().as_millis();

        assert_eq!(a.len(), 768, "expected 768 dims, got {}", a.len());
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "vector not L2-normalized: |v|={norm}");
        assert_eq!(a, b, "same input must produce an identical vector");

        println!(
            "local embed latency: first(incl. model load/download)={first_ms}ms, \
             warm={second_ms}ms, |v|={norm:.6}"
        );
    }

    #[test]
    fn replace_pii_ci_tolerates_case_and_whitespace() {
        // LLM echoes "Hans Müller"; source has different casing + double space.
        let body = "Kunde hans  müller meldet Defekt.";
        let out = replace_pii_ci(body, "Hans Müller", "Name_DEADBEEF");
        assert!(!out.to_lowercase().contains("müller"), "raw name leaked: {out}");
        assert!(out.contains("Name_DEADBEEF"), "token not inserted: {out}");
    }

    #[test]
    fn replace_pii_ci_noops_on_empty_and_missing() {
        assert_eq!(replace_pii_ci("abc", "   ", "T"), "abc");
        assert_eq!(replace_pii_ci("abc", "xyz", "T"), "abc");
    }

    #[test]
    fn mask_ai_summary_address_masks_street_keeps_zip_city() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let summary = "=== LOGISTIK & KONTAKTE ===\n\
            **Firma / Einrichtung:** Acme GmbH\n\
            **Adressen:** Musterstraße 42, 12345 Berlin (ermittelt via Google Search)\n\
            === TECHNISCHE DETAILS ===\n\
            **Gerät / Modell:** ZetaBody 770";

        let masked = mask_ai_summary_address(summary);

        assert!(
            !masked.contains("Musterstraße 42"),
            "street should be masked; got:\n{masked}"
        );
        assert!(
            masked.contains("Address_"),
            "Address_<hash> token missing; got:\n{masked}"
        );
        assert!(
            masked.contains("12345 Berlin"),
            "zip+city must stay in the clear; got:\n{masked}"
        );
    }

    #[test]
    fn mask_ai_summary_address_skips_own_hq() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        // Own-company markers injected (the env `ECK_OWN_ADDRESS_MARKERS`
        // equivalent) — the HQ header line is left untouched.
        let own = ["beispielstadt".to_string(), "musterallee".to_string()];
        let summary = "**Adressen:** Musterallee 73, 12345 Beispielstadt (own HQ)";
        assert_eq!(mask_ai_summary_address_with(summary, &own), summary);
    }

    #[test]
    fn mask_ai_summary_address_noop_without_marker() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let summary = "Just a free-form summary with no Adressen block.";
        assert_eq!(mask_ai_summary_address(summary), summary);
    }

    #[test]
    fn mask_ai_summary_address_masks_whole_line_when_no_zip() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let summary = "**Adressen:** 221B Baker Street, London";
        let masked = mask_ai_summary_address(summary);
        assert!(!masked.contains("Baker Street"), "non-German address line should still be masked; got:\n{masked}");
        assert!(masked.contains("Address_"));
    }

    #[tokio::test]
    #[ignore]
    async fn test_live_pii_extraction() {
        dotenvy::dotenv().ok();

        let api_key = match std::env::var("GEMINI_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                println!("⚠ GEMINI_API_KEY not set — skipping live PII test");
                return;
            }
        };

        std::env::set_var("SYNC_SECRET", "test_secret");

        let sample_text = "Ticket from Hans Müller. Address: Alexanderplatz 1, 10115 Berlin. \
            Issue: My ZetaBody 770 device is sparking and won't turn on. \
            Please call me at +49 123 456789.";

        println!("\n=== PII Anonymization E2E Test ===");
        println!("\nOriginal text:\n  {sample_text}");

        let client = HttpClient::new();
        let auth = eck_core::ai::AiAuth::studio(&api_key);
        let (anonymized, fingerprints) = extract_and_anonymize(&client, &auth, sample_text, "gemini-3.5-flash-lite")
            .await
            .expect("extract_and_anonymize failed");

        println!("\nExtracted fingerprints:");
        for fp in &fingerprints {
            println!("  {fp}");
        }

        println!("\nAnonymized summary:\n  {anonymized}");

        // Verify that original PII is not present in the anonymized text
        assert!(
            !anonymized.contains("Hans Müller"),
            "Anonymized text must not contain the original name"
        );
        assert!(
            !anonymized.contains("Alexanderplatz"),
            "Anonymized text must not contain the original address"
        );

        // Verify fingerprints were generated
        assert!(!fingerprints.is_empty(), "Should have extracted at least one PII entity");

        // Verify tokens are in the expected format
        for fp in &fingerprints {
            assert!(
                fp.starts_with("Name_") || fp.starts_with("Address_"),
                "Token should start with Name_ or Address_, got: {fp}"
            );
        }

        println!("\n✓ All assertions passed");
    }

    #[tokio::test]
    async fn test_surrealdb_rrf_support() {
        // Initialize an in-memory DB for a quick syntax check
        let db = surrealdb::Surreal::new::<surrealdb::engine::local::Mem>(()).await.unwrap();
        db.use_ns("test").use_db("test").await.unwrap();

        // Try to execute a dummy RRF query
        let result = db.query("RETURN search::rrf([[], []], 10, 60);").await;

        match result {
            Ok(_) => println!("\n✅ SUCCESS: SurrealDB supports search::rrf()!"),
            Err(e) => println!("\n❌ FAILURE: search::rrf() not supported. Error: {}", e),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn test_live_hybrid_search_rrf() {
        dotenvy::dotenv().ok();

        let api_key = match std::env::var("GEMINI_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                println!("⚠ GEMINI_API_KEY not set — skipping hybrid search RRF test");
                return;
            }
        };

        std::env::set_var("SYNC_SECRET", "test_secret");

        let gen_model = std::env::var("GEMINI_GENERATION_MODEL")
            .unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string());
        let emb_model = std::env::var("GEMINI_EMBEDDING_MODEL")
            .unwrap_or_else(|_| "gemini-embedding-2".to_string());
        let auth = eck_core::ai::AiAuth::studio(&api_key);

        // 1. Initialize SurrealDB via eck_core (same path as production)
        let tmp_dir = std::env::temp_dir().join(format!("surreal_rrf_test_{}", std::process::id()));
        let db = eck_core::db::connect(tmp_dir.to_str().unwrap()).await.unwrap();

        // 2. Define BM25 + HNSW indexes (v3 syntax: FULLTEXT replaces SEARCH)
        db.query(
            "DEFINE ANALYZER custom_analyzer TOKENIZERS blank,class,camel,punct FILTERS lowercase,ascii;
             DEFINE INDEX issue_bm25 ON order FIELDS issue_description FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX customer_name_bm25 ON order FIELDS customer_name FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX embedding_hnsw ON order FIELDS embedding HNSW DIMENSION 768 DIST COSINE;"
        ).await.unwrap();

        // 3. Define test documents
        let docs = [
            ("Hans Müller", "The display is broken and completely shattered."),   // doc1: Perfect Match
            ("Julia Weber", "The screen is smashed into pieces."),                // doc2: Vector Decoy
            ("Hans Müller", "The battery is dead and won't charge."),             // doc3: BM25 Decoy
        ];

        let client = HttpClient::new();

        println!("\n=== Hybrid Search RRF Integration Test ===\n");

        for (i, (name, issue)) in docs.iter().enumerate() {
            let combined = format!("Customer: {name}\nIssue: {issue}");
            let (anonymized, _) = extract_and_anonymize(&client, &auth, &combined, &gen_model)
                .await
                .unwrap_or_else(|e| panic!("Failed to anonymize doc{}: {e}", i + 1));

            let (embedding, _) = auth.embed_content(&client, &emb_model, &anonymized, EMBEDDING_DIM)
                .await
                .unwrap_or_else(|e| panic!("Failed to embed doc{}: {e}", i + 1));

            println!("Doc {}: {} | {} → anonymized: {}", i + 1, name, issue, anonymized);

            db.query("CREATE order CONTENT {
                customer_name: $name,
                issue_description: $issue,
                embedding: $emb
            }")
            .bind(("name", *name))
            .bind(("issue", *issue))
            .bind(("emb", embedding))
            .await
            .unwrap();

            // Small delay to respect rate limits
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        // Wait for indexing
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;


        // 4. Query: should match doc1 best (both name + display/broken terms)
        let search_query = "broken display for Hans Müller";
        println!("\nSearch query: \"{search_query}\"");

        let query_emb = embed_query(search_query)
            .await
            .expect("Failed to embed search query");

        // 5. Build per-term BM25 queries — each becomes a separate ranked list for RRF.
        // SurrealDB BM25 has AND semantics for multi-word, so we split into individual terms.
        // Each term+field combination gets its own @N@ reference for proper BM25 scoring.
        let terms: Vec<String> = search_query
            .split_whitespace()
            .filter(|t| t.len() > 2)
            .map(|t| t.replace('\'', "''").replace('\\', "\\\\"))
            .collect();

        let mut let_stmts = Vec::new();
        let mut rrf_vars = vec!["$vec_results".to_string()];

        // Vector search is always the first ranked list
        let_stmts.push(
            "LET $vec_results = SELECT id, vector::distance::knn() AS distance FROM order WHERE embedding <|3,100|> $query_emb".to_string()
        );

        // Each term gets a BM25 query per field (each using its own @N@ reference)
        for (i, term) in terms.iter().enumerate() {
            let var_issue = format!("$bm25_issue_{i}");
            let var_name = format!("$bm25_name_{i}");
            let r1 = i * 2 + 1;
            let r2 = i * 2 + 2;
            let_stmts.push(format!(
                "LET {var_issue} = SELECT id, search::score({r1}) AS s FROM order WHERE issue_description @{r1}@ '{term}' ORDER BY s DESC"
            ));
            let_stmts.push(format!(
                "LET {var_name} = SELECT id, search::score({r2}) AS s FROM order WHERE customer_name @{r2}@ '{term}' ORDER BY s DESC"
            ));
            rrf_vars.push(var_issue);
            rrf_vars.push(var_name);
        }

        let rrf_array = rrf_vars.join(", ");
        let sql = format!(
            "{stmts};\
             LET $hybrid = search::rrf([{rrf_array}], 10, 60);\
             SELECT customer_name, issue_description FROM $hybrid.id;",
            stmts = let_stmts.join(";\n")
        );

        let total_stmts = let_stmts.len() + 1 /* RRF LET */ + 1 /* final SELECT */;
        let final_idx = total_stmts - 1;

        let mut response = db.query(&sql)
            .bind(("query_emb", query_emb))
            .await
            .expect("Hybrid RRF query failed");

        let results: Vec<Value> = response.take(final_idx).unwrap_or_default();

        println!("\n--- Hybrid RRF Results ---");
        for (i, r) in results.iter().enumerate() {
            let name = r.get("customer_name").and_then(|v| v.as_str()).unwrap_or("?");
            let issue = r.get("issue_description").and_then(|v| v.as_str()).unwrap_or("?");
            println!("  #{}: {} | {}", i + 1, name, issue);
        }

        // 6. Assertions
        assert!(!results.is_empty(), "RRF should return at least one result");

        let top = &results[0];
        let top_name = top.get("customer_name").and_then(|v| v.as_str()).unwrap_or("");
        let top_issue = top.get("issue_description").and_then(|v| v.as_str()).unwrap_or("");

        assert_eq!(top_name, "Hans Müller", "Top result must be Hans Müller (got: {top_name})");
        assert!(
            top_issue.contains("display") || top_issue.contains("shattered"),
            "Top result must be the display issue (got: {top_issue})"
        );

        println!("\n✓ All assertions passed — Doc 1 ranked first by hybrid RRF");

        // Cleanup temp DB
        drop(db);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

}
