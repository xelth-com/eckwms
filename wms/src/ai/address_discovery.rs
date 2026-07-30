//! Lever 2A — AI address discovery for the HQ-fallback pile.
//!
//! Most office-pinned tickets have NO customer address anywhere in the data, so
//! the free re-geocode (Lever 1) can't place them. The only signals are the
//! **business identifiers** — the company / clinic / gym name and the email
//! domain — which are *public* facts about a business. This pass sends those
//! (and only those) to Gemini with Google-Search grounding to look up the
//! business's public physical address, then geocodes it.
//!
//! PRIVACY MODEL (owner decision 2026-06-27): an address the customer *gave* us
//! stays masked like all PII and is never exposed — those tickets already
//! geocode from zip+city and never reach this pass. An address we *found*
//! ourselves via public search is public anyway, so we store it openly and tag
//! `geo_source = "derived"`. Personal name / phone / street from the customer are
//! never sent here — only the company name + email domain. The whole behaviour is
//! gated behind the `system_config:geo.grounding_enabled` switch (operator UI,
//! the `/api/geo/grounding-config` endpoint, or the internal agent can flip it).

use eck_core::db::SurrealDb;
use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use tracing::{info, warn};

use super::telemetry::{current_budget_level, log_telemetry, BudgetLevel};

const PROMPT_TEMPLATE: &str = r#"You are a business-address lookup assistant for {{DEVICE_SUPPORT}} (German-speaking DE/AT/CH market).
You are given a COMPANY / clinic / gym / practice name and/or its email domain. Use Google Search to find that business's PUBLIC physical address (its office / studio / Praxis / Impressum address).

STRICT RULES:
- Only return an address you can actually verify via search (official website, Impressum, Google Business). NEVER invent or guess one.
- The email domain is a strong hint to the company's website.
- Output ONLY a single minified JSON object, no prose, no markdown fences:
  {"street":"...","zip":"...","city":"...","country":"DE","confidence":0.0}
- confidence is your 0..1 certainty that this is the right business and address.
- If you cannot find a confident address, output {"confidence":0}."#;

/// The address-lookup system prompt with the tenant's device-support phrase
/// spliced in (env `ECK_TENANT_BRAND`; neutral "device support" when unset).
fn prompt_text() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        PROMPT_TEMPLATE.replace("{{DEVICE_SUPPORT}}", &crate::ai::branding::device_support_phrase())
    })
}

/// Read the grounding toggle. Defaults to false (no surprise cloud spend / data
/// egress) — the operator or the internal agent flips it on when ready.
async fn grounding_enabled(db: &SurrealDb) -> bool {
    let cfg: Vec<Value> = db
        .query("SELECT grounding_enabled FROM system_config:geo")
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    cfg.first()
        .and_then(|v| v.get("grounding_enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Extract the first balanced-looking JSON object from model text (it may wrap
/// the object in prose or ```json fences despite instructions).
fn extract_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

fn nonempty(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "null")
        .map(String::from)
}

/// Consumer email providers — a domain like these is NOT a company, so it's a
/// dead end for a business-address lookup (a private individual). Skipping them
/// avoids spending grounding calls that can only fail.
fn is_freemail(domain: &str) -> bool {
    let d = domain.to_lowercase();
    const FREE: &[&str] = &[
        "gmail.com", "googlemail.com", "yahoo.", "ymail.com", "hotmail.", "outlook.",
        "live.", "msn.com", "gmx.", "web.de", "t-online.de", "freenet.de", "aol.",
        "icloud.com", "me.com", "mail.com", "online.de", "arcor.de", "1und1.de",
        "vodafone.de", "posteo.de", "protonmail.com", "proton.me",
    ];
    FREE.iter().any(|f| if f.ends_with('.') { d.contains(f) } else { d == *f })
}

/// True when we have something a business-address lookup can actually use: a
/// company name, or a non-freemail (i.e. likely company) email domain.
///
/// A value that is really a PPRL pseudonym (`Company_<hex>`, `Email_<hex>`, …) is
/// NOT data — it is treated as absent, so a masked ticket never trips a grounding
/// call. We must never ask Google Search to look up a hash.
fn has_business_signal(company: Option<&str>, domain: Option<&str>) -> bool {
    let company = company.filter(|c| !crate::services::customer_geo::is_pprl_token(c));
    let domain = domain.filter(|d| !crate::services::customer_geo::is_pprl_token(d));
    company.is_some() || domain.map(|d| !is_freemail(d)).unwrap_or(false)
}

/// Result of one grounding attempt (for the caller's tally).
pub enum GroundOutcome {
    /// Geocoded and written (geo_source="derived").
    Placed,
    /// AI returned no usable / low-confidence / ungeocodable address — marked
    /// `derived_failed` so we never pay for it again.
    NoAddress,
    /// Transient Gemini/HTTP error — NOT marked, so it can be retried later.
    Error,
}

/// Look up + geocode ONE ticket's business address via Gemini + Google grounding,
/// from its (clear-text, non-personal) company name + email domain. On success
/// writes geo + the public found address tagged `geo_source="derived"`; on a
/// confident-miss marks `derived_failed`. Shared by the bulk batch and the
/// per-ticket auto-fill worker. Does its own telemetry log.
pub async fn try_grounding_one(
    db: &SurrealDb,
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    model: &str,
    id: &str,
    company: Option<&str>,
    domain: Option<&str>,
    known_city: Option<&str>,
) -> GroundOutcome {
    // Never send a PPRL pseudonym to Google — a hash is not a business we can
    // look up (and leaking the masked token defeats the point of masking).
    let company = company.filter(|c| !crate::services::customer_geo::is_pprl_token(c));
    let domain = domain.filter(|d| !crate::services::customer_geo::is_pprl_token(d));

    let mut q = String::new();
    if let Some(c) = company { q.push_str(&format!("Company: {c}\n")); }
    if let Some(d) = domain { q.push_str(&format!("Email domain: {d}\n")); }
    if let Some(c) = known_city { q.push_str(&format!("Possibly in city: {c}\n")); }

    let payload = json!({
        "systemInstruction": { "parts": [{ "text": prompt_text() }] },
        "contents": [{ "parts": [{ "text": format!("Find the business address for:\n{q}") }] }],
        "tools": [{ "googleSearch": {} }],
        "generationConfig": { "temperature": 0.1, "maxOutputTokens": 512 }
    });

    let (text, usage) = match auth.generate_content(http, model, payload).await {
        Ok(t) => t,
        Err(e) => { warn!("[Discover] {id}: gemini error: {e}"); return GroundOutcome::Error; }
    };
    if !usage.is_null() {
        log_telemetry(db, "address_discovery", model, id, &usage).await;
    }

    let obj = match extract_json(&text) {
        Some(o) => o,
        None => { warn!("[Discover] {id}: no JSON in response"); mark_derived_failed(db, id).await; return GroundOutcome::NoAddress; }
    };

    let confidence = obj.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let zip = nonempty(&obj, "zip");
    let city = nonempty(&obj, "city");
    let street = nonempty(&obj, "street");
    let country = nonempty(&obj, "country");

    if (city.is_none() && zip.is_none()) || confidence < 0.5 {
        mark_derived_failed(db, id).await;
        return GroundOutcome::NoAddress;
    }

    // Geocode via the cached resolver (zip+city only — never the street), scoped
    // to the country the AI reported so non-DACH hits (e.g. a Swedish "Malmö")
    // aren't forced into a wrong German match. Coarse when no zip was found.
    let cc = crate::services::geocoder::cc_for_country(country.as_deref());
    match crate::services::geocoder::resolve_zip_city_cc_cached(db, zip.as_deref(), city.as_deref(), cc).await {
        Some((lat, lng)) => {
            let coarse = zip.is_none();
            // The found address is public → store it openly, tagged derived.
            let derived = json!({
                "street": street, "zip": zip, "city": city,
                "country": country, "confidence": confidence,
            });
            let upd = format!(
                "UPDATE document SET \
                    meta.geo = {{ lat: $lat, lng: $lng }}, \
                    meta.geo_fallback = NONE, meta.geo_failed = NONE, \
                    meta.geo_source = 'derived', \
                    meta.geo_coarse = {coarse}, \
                    meta.derived_address = $derived, \
                    updated_at = time::now() \
                 WHERE record::id(id) = $id RETURN NONE",
                coarse = if coarse { "true" } else { "NONE" }
            );
            if let Err(e) = db.query(&upd)
                .bind(("id", id.to_string()))
                .bind(("lat", lat))
                .bind(("lng", lng))
                .bind(("derived", derived))
                .await
            {
                warn!("[Discover] {id}: db update failed: {e}");
                return GroundOutcome::Error;
            }
            // Advance causality so the grounded address survives mesh merge.
            crate::services::geocoder::bump_geo_vclock(db, "document", id).await;
            info!("[Discover] {id}: {} {} (conf {:.2}) -> {:.4},{:.4}",
                zip.as_deref().unwrap_or("?"), city.as_deref().unwrap_or("?"), confidence, lat, lng);
            GroundOutcome::Placed
        }
        None => { mark_derived_failed(db, id).await; GroundOutcome::NoAddress }
    }
}

/// Mark a ticket as AI-tried-but-unfound so it isn't looked up (and paid for)
/// again. Advances `updated_at` + `_vclock` like every other geo writer — an
/// un-bumped marker loses the mesh merge and the sweep re-pays for the same
/// ticket every pass (observed 2026-07-21: derived_failed count decayed 11→10
/// within hours and discover re-grounded the reverted ticket hourly).
async fn mark_derived_failed(db: &SurrealDb, id: &str) {
    let q = "UPDATE document SET meta.geo_source = 'derived_failed', \
             updated_at = time::now() WHERE record::id(id) = $id";
    match db.query(q).bind(("id", id.to_string())).await {
        Ok(_) => crate::services::geocoder::bump_geo_vclock(db, "document", id).await,
        Err(e) => warn!("[Discover] {id}: derived_failed mark failed: {e}"),
    }
}

/// On-demand batch: discover + geocode addresses for office-pinned tickets that
/// carry a business identifier. Spawned (Gemini + Nominatim are rate-limited);
/// logs progress under `[Discover]`. `limit` caps the batch (0 = a safety cap).
pub async fn discover_addresses_batch(db: SurrealDb, model: String, limit: usize) {
    if !grounding_enabled(&db).await {
        warn!("[Discover] grounding disabled (system_config:geo.grounding_enabled != true) — nothing to do");
        return;
    }
    let lim = if limit == 0 { 200 } else { limit };
    info!("[Discover] Address-discovery pass started (limit={lim}, model={model})");

    let http = match HttpClient::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => { warn!("[Discover] http client build failed: {e}"); return; }
    };

    let mut auth = match eck_core::ai::AiAuth::resolve(&http).await {
        Ok(a) if a.is_configured() => a,
        Ok(_) => { warn!("[Discover] AI not configured — aborting"); return; }
        Err(e) => { warn!("[Discover] auth resolve failed: {e}"); return; }
    };
    // The managed Vertex bearer lives ~1h; a full pile run is longer, so
    // re-mint periodically (the summarizer re-resolves every cycle for the same
    // reason). Counts calls since the last resolve.
    let mut since_auth = 0usize;
    const AUTH_REFRESH_EVERY: usize = 60;

    // Candidates: office-pinned, not operator-pinned, with a company name or
    // email to look up. Skip ones we already tried (geo_source set).
    let sql = format!(
        "SELECT record::id(id) AS id, meta.company AS company, meta.email AS email, \
                meta.city AS city \
         FROM document \
         WHERE type = 'support_ticket' AND meta.geo_fallback = true \
           AND meta.geo_override IS NOT true AND meta.geo_source IS NONE \
           AND status != 'Closed' AND status != 'Auto closed' \
           AND ((meta.company != NONE AND meta.company != '') \
                OR (meta.email != NONE AND meta.email != '')) \
         LIMIT {lim}"
    );
    let rows: Vec<Value> = match db.query(&sql).await.and_then(|mut r| r.take(0)) {
        Ok(r) => r,
        Err(e) => { warn!("[Discover] candidate select failed: {e}"); return; }
    };

    let mut scanned = 0usize;
    let mut resolved = 0usize;
    let mut no_address = 0usize;
    let mut low_conf = 0usize;

    for row in &rows {
        // Budget circuit-breaker — stop entirely on Halt.
        if let BudgetLevel::Halt = current_budget_level() {
            warn!("[Discover] budget Halt — stopping early at {scanned}/{}", rows.len());
            break;
        }

        // Refresh the (managed) bearer before it expires mid-run.
        if since_auth >= AUTH_REFRESH_EVERY {
            if let Ok(a) = eck_core::ai::AiAuth::resolve(&http).await {
                if a.is_configured() { auth = a; }
            }
            since_auth = 0;
        }
        since_auth += 1;

        scanned += 1;
        let id = row.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let company = nonempty(row, "company");
        let email = nonempty(row, "email");
        let domain = email.as_deref().and_then(|e| e.split('@').nth(1)).map(str::to_string);
        let known_city = nonempty(row, "city");

        // Freemail-only (private individual) → nothing a business lookup can find.
        if !has_business_signal(company.as_deref(), domain.as_deref()) {
            continue;
        }

        match try_grounding_one(&db, &http, &auth, &model, &id,
            company.as_deref(), domain.as_deref(), known_city.as_deref()).await
        {
            GroundOutcome::Placed => resolved += 1,
            GroundOutcome::NoAddress => no_address += 1,
            GroundOutcome::Error => low_conf += 1, // tallied as "other" for the summary
        }

        // Gentle pacing on top of the resolver's own Nominatim throttle.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    info!(
        "[Discover] Done: scanned={scanned} resolved={resolved} no_address={no_address} errors={low_conf}"
    );
}

/// Resolve ONE ticket's location at summary time — the single place new tickets
/// get an address. Called from the summarization pass right after a summary is
/// written (the worker already has http+auth there), so every ticket is handled
/// exactly once, at the start, with no separate later pass. Order, cheapest →
/// priciest, and ALWAYS leaves a terminal state so it is never re-attempted:
/// 1. already located (real geo, not fallback) or operator-pinned → leave it.
/// 2. free: meta zip/city, then the summary's PLZ-Ort, then phone Vorwahl.
/// 3. paid: AI grounding (only if the switch is on, there's a business signal,
///    and we haven't already AI-tried it) → `derived` / `derived_failed`.
/// 4. nothing worked → pin HQ + tag `needs_grounding` (grounding off, retry on
///    enable) or `auto_nomatch` (genuinely no signal) so it's never re-tried.
pub async fn resolve_ticket_address(
    db: &SurrealDb,
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    model: &str,
    id: &str,
    meta: &Value,
    summary: &str,
) {
    // 1. Respect an existing real placement / operator pin.
    let is_fallback = meta.get("geo_fallback").and_then(|v| v.as_bool()).unwrap_or(false);
    let has_geo = meta.get("geo").map(|g| g.is_object()).unwrap_or(false);
    let is_override = meta.get("geo_override").and_then(|v| v.as_bool()).unwrap_or(false);
    if is_override || (has_geo && !is_fallback) {
        return;
    }
    let prior_source = nonempty(meta, "geo_source");

    let company = nonempty(meta, "company");
    let email = nonempty(meta, "email");
    let domain = email.as_deref().and_then(|e| e.split('@').nth(1)).map(str::to_string);
    let city = nonempty(meta, "city");
    let zip = nonempty(meta, "zip");
    let phone = nonempty(meta, "phone");

    // 2a. Free: structured meta zip/city.
    if zip.is_some() || city.is_some() {
        if let Some((lat, lng)) = crate::services::geocoder::resolve_zip_city_cached(db, zip.as_deref(), city.as_deref()).await {
            crate::services::geocoder::save_geo_resolved(db, "document", id, "meta", lat, lng, zip.is_none()).await;
            return;
        }
    }
    // 2b. Free: the summary's "PLZ Ort".
    if let Some((z, c)) = crate::services::geocoder::parse_summary_address(summary) {
        if let Some((lat, lng)) = crate::services::geocoder::resolve_zip_city_cached(db, Some(&z), Some(&c)).await {
            crate::services::geocoder::save_geo_resolved(db, "document", id, "meta", lat, lng, false).await;
            return;
        }
    }
    // 2b′. Free + local: place from a customer record we already trust (another
    //      located ticket, or an Exact partner, with the same email / phone /
    //      company). A real zip/city we already hold beats the area-code centroid
    //      below, so it comes first. Identity never leaves the box.
    if crate::services::customer_geo::try_place_by_customer_db(db, id, meta)
        .await
        .is_some()
    {
        return;
    }
    // 2b″. Free + local: an address OCR'd out of the ticket's OWN attachments
    //      (invoice / form). A real PLZ, so it outranks the area-code centroid
    //      below but sits under customer_db. Our HQ header on every form is
    //      hard-blacklisted inside the lever.
    if crate::services::attachment_geo::try_place_by_attachment_ocr(db, id, meta)
        .await
        .is_some()
    {
        return;
    }
    // 2c. Free: phone area code (local; phone never leaves the box).
    if let Some(p) = &phone {
        if crate::services::vorwahl::try_place_by_vorwahl(db, id, p).await.is_some() {
            return;
        }
    }

    // 3. Paid AI grounding — only with a real business signal, the switch on, a
    //    budget, and not already AI-tried (so we never pay for the same ticket
    //    twice).
    let business = has_business_signal(company.as_deref(), domain.as_deref());
    let grounding = grounding_enabled(db).await;
    if business
        && grounding
        && !matches!(current_budget_level(), BudgetLevel::Halt)
        && prior_source.as_deref() != Some("derived_failed")
    {
        match try_grounding_one(db, http, auth, model, id,
            company.as_deref(), domain.as_deref(), city.as_deref()).await
        {
            GroundOutcome::Placed | GroundOutcome::NoAddress => return, // marked inside
            GroundOutcome::Error => return, // transient — leave un-pinned for a future retry
        }
    }

    // 4. Exhausted → pin HQ with a terminal marker so it is never re-attempted.
    let marker = if business && !grounding { "needs_grounding" } else { "auto_nomatch" };
    crate::services::geocoder::pin_home_with_source(db, id, marker).await;
}

#[cfg(test)]
mod tests {
    use super::has_business_signal;

    #[test]
    fn pprl_token_is_not_a_business_signal() {
        // A masked company / email token must NOT trip a grounding call — we
        // never ask Google Search to look up a hash.
        assert!(!has_business_signal(Some("Company_89A102ED05B7577E"), None));
        assert!(!has_business_signal(
            Some("Company_89A102ED05B7577E"),
            Some("gmail.com")
        ));
        // A real company name (or a real business email domain) still counts.
        assert!(has_business_signal(Some("Hector Sport-Centrum"), None));
        assert!(has_business_signal(
            Some("Company_89A102ED05B7577E"),
            Some("hector-sport.de")
        ));
        // Freemail alone is still not a business signal.
        assert!(!has_business_signal(None, Some("gmx.de")));
    }
}
