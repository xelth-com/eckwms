//! Stub and recently-wired handlers for frontend endpoints not yet fully ported.
//! Returns valid responses so the UI doesn't crash.

use axum::{extract::{Path, Query, State}, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

/// GET /api/odoo/pickings — stub
pub async fn odoo_pickings() -> Json<Vec<Value>> {
    Json(vec![])
}

/// The shipment projection shared by the list and the lookup endpoint.
///
/// `updated_at` is coerced to a real `datetime` on the way out: legacy rows were
/// written as RFC3339 STRINGS (see `services::delivery::persist`) and SurrealDB
/// `ORDER BY` sorts by TYPE first, which parked the freshest rows at the very end
/// of the list — the PDA fetches the head of the list and never saw them. Mesh
/// adoption can re-introduce string timestamps too (the JSON wire format has no
/// datetime), so the cast stays for good.
///
/// Same total IF/ELSE shape as `sync::conflict::write_adopted`, and for the same
/// reason: a bare `<datetime>` cast would ERROR on a row where `updated_at` is
/// missing (`<datetime> NONE`) and take the whole query down with it. Only
/// strings are cast.
const SHIPMENT_SELECT: &str = "SELECT record::id(id) AS id, tracking_number, status, provider, \
            recipient_name, recipient_street, recipient_city, recipient_zip, \
            recipient_country, pickup_name, pickup_street, pickup_city, \
            pickup_zip, pickup_country, description, reference, weight, \
            dimensions, status_date, created_ext, delivery_date, hwb_number, \
            (IF type::is_string(updated_at) THEN <datetime> updated_at \
             ELSE updated_at END) AS updated_at \
     FROM stock_picking_delivery";

fn field(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// One distilled shipment row → the API shape the SPA and the PDA expect.
///
/// The dashboard map parses a few keys out of `raw_response`, so we reconstruct
/// that object from the distilled fields — the SPA can't tell it from the
/// original scraper blob. `score` is present only on the lookup endpoint.
fn shipment_row_json(r: &Value, score: Option<i64>) -> Value {
    let city = field(r, "recipient_city");
    // The PDA receiving flow reads recipient_*/delivery_* (with fallbacks),
    // plus product/description for what the parcel is. This is the operator
    // surface — recipient PII is intentionally in clear here, unlike the
    // ops/brain plane where stock_picking_delivery is Zone-1-denied.
    let raw = json!({
        "recipient_name": field(r, "recipient_name"),
        "recipient_street": field(r, "recipient_street"),
        "recipient_zip": field(r, "recipient_zip"),
        "recipient_country": field(r, "recipient_country"),
        "recipient_city": city,
        "delivery_city": city,
        "pickup_name": field(r, "pickup_name"),
        "pickup_street": field(r, "pickup_street"),
        "pickup_zip": field(r, "pickup_zip"),
        "pickup_city": field(r, "pickup_city"),
        "pickup_country": field(r, "pickup_country"),
        "product": field(r, "description"),
        "description": field(r, "description"),
        "reference": field(r, "reference"),
        "hwb_number": field(r, "hwb_number"),
        "weight": field(r, "weight"),
        "dimensions": field(r, "dimensions"),
        "status_date": field(r, "status_date"),
        "created_at": field(r, "created_ext"),
        "delivery_date": field(r, "delivery_date"),
    });
    let mut out = json!({
        "id": r.get("id").cloned().unwrap_or(Value::Null),
        "tracking_number": r.get("tracking_number").cloned().unwrap_or(Value::Null),
        "status": r.get("status").cloned().unwrap_or(Value::Null),
        "provider": r.get("provider").cloned().unwrap_or(Value::Null),
        "updated_at": r.get("updated_at").cloned().unwrap_or(Value::Null),
        "raw_response": serde_json::to_string(&raw).unwrap_or_default(),
    });
    if let (Some(obj), Some(sc)) = (out.as_object_mut(), score) {
        obj.insert("score".into(), json!(sc));
    }
    out
}

/// GET /api/delivery/shipments — reads the meshed `stock_picking_delivery` model
/// (works on every node, not just the scraper node).
pub async fn list_shipments(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let sql = format!("{SHIPMENT_SELECT} ORDER BY updated_at DESC LIMIT 200");
    let rows: Vec<Value> = state.db
        .query(sql)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .unwrap_or_default();

    Ok(Json(rows.iter().map(|r| shipment_row_json(r, None)).collect()))
}

#[derive(Deserialize)]
pub struct ShipmentLookupParams {
    #[serde(default)]
    pub q: String,
    pub limit: Option<usize>,
}

/// Lowercase + fold the German umlauts, so "Müller" and "mueller" both hit.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'ä' | 'Ä' => out.push_str("ae"),
            'ö' | 'Ö' => out.push_str("oe"),
            'ü' | 'Ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Folded query → deduped search tokens (unicode letters/digits, min 3 chars).
fn tokenize(q: &str) -> Vec<String> {
    let mut toks: Vec<String> = Vec::new();
    for part in fold(q).split(|c: char| !c.is_alphanumeric()) {
        if part.chars().count() < 3 {
            continue;
        }
        let t = part.to_string();
        if !toks.contains(&t) {
            toks.push(t);
        }
    }
    toks
}

/// Identifier normalization for the barcode class: uppercase, alphanumerics only.
/// A CODE_128 label barcode and the field it corresponds to are the same digits
/// wearing different punctuation, so both ends get flattened the same way.
fn norm_id(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Below this length "one contains the other" is noise, not identity.
const MIN_ID_LEN: usize = 8;

/// The barcode-containment class: a scanned label carries an identifier that
/// only PARTIALLY equals the stored one. The OPAL label reads
/// `SEVFRA` + 2 digits + the 12-digit HWB + 2 digits (e.g. `SEVFRA0904194078709001`
/// wrapping HWB `041940787090`), so an exact match on any single field fails and
/// the PDA got an empty list. Match if either string contains the other — the
/// load-bearing direction being "q contains the identifier", because `q` may be
/// the barcode PLUS the OCR of the rest of the camera frame (address block and
/// all), not a clean identifier.
///
/// Deliberately scored BELOW the 1000 of an exact tracking number: containment is
/// strong evidence, not proof, so it ranks a candidate — it must never be treated
/// as an auto-select.
///
/// `q` must already be `norm_id`-ed. Returns the class score, or None for no hit.
fn containment_score(r: &Value, q: &str) -> Option<i64> {
    if q.len() < MIN_ID_LEN {
        return None;
    }
    // Priority order: the strong identifiers first, so a row that hits both
    // reports the 800.
    for (key, score) in [
        ("tracking_number", 800i64),
        ("hwb_number", 800),
        ("reference", 700),
    ] {
        let id = norm_id(&field(r, key));
        if id.len() < MIN_ID_LEN {
            continue; // a 3-char reference inside a barcode proves nothing
        }
        if id == q || q.contains(&id) || id.contains(q) {
            return Some(score);
        }
    }
    None
}

fn is_zip5(s: &str) -> bool {
    s.len() == 5 && s.chars().all(|c| c.is_ascii_digit())
}

/// Score one row against the tokens. 3 = a zip hit, 2 = an identity field
/// (tracking/name/city), 1 = free text (street/reference/description).
fn score_row(r: &Value, tokens: &[String]) -> i64 {
    const ZIP: [&str; 2] = ["recipient_zip", "pickup_zip"];
    const STRONG: [&str; 5] = [
        "tracking_number",
        "recipient_name",
        "pickup_name",
        "recipient_city",
        "pickup_city",
    ];
    const WEAK: [&str; 4] = [
        "recipient_street",
        "pickup_street",
        "reference",
        "description",
    ];

    let mut score = 0i64;
    for t in tokens {
        for k in ZIP {
            let v = field(r, k);
            if is_zip5(&v) && v == *t {
                score += 3;
            }
        }
        for k in STRONG {
            if fold(&field(r, k)).contains(t.as_str()) {
                score += 2;
            }
        }
        for k in WEAK {
            if fold(&field(r, k)).contains(t.as_str()) {
                score += 1;
            }
        }
    }
    score
}

/// GET /api/delivery/shipments/lookup?q=…&limit=… — point lookup for the PDA.
///
/// Three classes:
///   1. exact tracking number — a single indexed DB hit, score 1000, returned
///      alone. 1000 means PROVEN, and only this class earns it;
///   2. barcode containment — the scanned label vs tracking/hwb/reference,
///      score 800 / 800 / 700 (see `containment_score`);
///   3. tokenized scoring of every remaining row (see `score_row`).
/// Class 1 short-circuits; 2 and 3 are concatenated (containment first) into one
/// ranked list, because a scan can carry OCR'd address text alongside the code.
/// A full scan of a few thousand rows per lookup is cheap enough and keeps the
/// ranking logic in one testable place.
pub async fn lookup_shipments(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ShipmentLookupParams>,
) -> ApiResult<Json<Vec<Value>>> {
    let q = params.q.trim().to_string();
    let limit = params.limit.unwrap_or(20).clamp(1, 50);
    if q.is_empty() {
        return Ok(Json(vec![]));
    }

    // 1. exact tracking number (case-insensitive, and with whitespace stripped).
    let q_lower = q.to_lowercase();
    let q_tight: String = q_lower.chars().filter(|c| !c.is_whitespace()).collect();
    let sql = format!(
        "{SHIPMENT_SELECT} WHERE string::lowercase(tracking_number) IN $keys LIMIT 1"
    );
    let exact: Vec<Value> = state.db
        .query(sql)
        .bind(("keys", vec![q_lower, q_tight]))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .unwrap_or_default();
    if let Some(row) = exact.first() {
        return Ok(Json(vec![shipment_row_json(row, Some(1000))]));
    }

    // The single full scan, reused by both the containment class and the tokens.
    // The rows arrive newest-first, so a STABLE sort on the score alone gives
    // "score desc, then updated_at desc" for free.
    let sql = format!("{SHIPMENT_SELECT} ORDER BY updated_at DESC LIMIT 5000");
    let rows: Vec<Value> = state.db
        .query(sql)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .unwrap_or_default();

    // 2. barcode containment. Ranked ABOVE the token hits but not INSTEAD of
    //    them: `q` can be the barcode plus the OCR of the rest of the camera
    //    frame, so the address words are real signal too. The operator gets one
    //    ranked list — barcode candidates on top, token matches below.
    let q_norm = norm_id(&q);
    let mut hits: Vec<(i64, usize)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| containment_score(r, &q_norm).map(|s| (s, i)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0));

    // 3. tokenized scoring over everything the containment class did not claim.
    let tokens = tokenize(&q);
    let mut scored: Vec<(i64, usize)> = Vec::new();
    if !tokens.is_empty() {
        let claimed: std::collections::HashSet<usize> = hits.iter().map(|(_, i)| *i).collect();
        scored = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| !claimed.contains(i))
            .map(|(i, r)| (score_row(r, &tokens), i))
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
    }

    Ok(Json(
        hits.into_iter()
            .chain(scored)
            .take(limit)
            .map(|(s, i)| shipment_row_json(&rows[i], Some(s)))
            .collect(),
    ))
}

/// GET /api/delivery/config — stub
pub async fn delivery_config() -> Json<Value> {
    Json(json!({ "opal": true, "dhl": true, "carriers": [], "defaults": {} }))
}

/// POST /api/delivery/shipments — stub
pub async fn create_shipment(Json(_body): Json<Value>) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, Json(json!({ "error": "Manual shipment creation not yet ported" })))
}

/// POST /api/delivery/shipments/:id/cancel — stub
pub async fn cancel_shipment() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, Json(json!({ "error": "Shipment cancellation not yet ported" })))
}

/// POST /api/delivery/shipments/:id/resolve — force-mark a stuck shipment as delivered.
/// Used by the operator when the carrier's status feed is broken but the parcel
/// actually arrived. Bumps updated_at so SLA timers reset.
pub async fn resolve_shipment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let rid = format!("shipment:{}", id);
    let _: Vec<Value> = state.db
        .query("UPDATE type::record($rid) SET status = 'delivered', resolved_manually = true, updated_at = time::now()")
        .bind(("rid", rid))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .unwrap_or_default();
    Ok(Json(json!({ "success": true, "id": id })))
}

/// POST /api/delivery/import/opal — Triggers OPAL scraper
pub async fn import_opal(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap_or_default();
    crate::services::scheduler::sync_opal(&state.db, &client, &state.instance_id).await;
    Ok(Json(json!({ "success": true, "message": "OPAL sync completed" })))
}

/// POST /api/delivery/import/dhl — Triggers DHL scraper
pub async fn import_dhl(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap_or_default();
    crate::services::scheduler::sync_dhl(&state.db, &client, &state.instance_id).await;
    Ok(Json(json!({ "success": true, "message": "DHL sync completed" })))
}

/// GET /api/delivery/shipments/:id/ai-match — stub
pub async fn ai_match_shipment() -> Json<Value> {
    Json(json!({ "matches": [] }))
}

/// GET /api/delivery/sync/history — Real implementation (fetches from sync_history table)
pub async fn delivery_sync_history(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let history: Vec<Value> = state.db
        .query("SELECT * FROM sync_history ORDER BY started_at DESC LIMIT 100")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(history))
}

/// GET /api/delivery/carriers — stub
pub async fn delivery_carriers() -> Json<Vec<Value>> {
    Json(vec![])
}

/// GET /api/analysis/support-dump — Real implementation
pub async fn analysis_support_dump(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let tickets: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, status, summary_status, payload.subject AS subject FROM document WHERE type = 'support_ticket' ORDER BY updated_at DESC LIMIT 100")
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    Ok(Json(json!({ "tickets": tickets })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_folds_umlauts_and_drops_short_tokens() {
        assert_eq!(tokenize("Müller, Köln 50667"), vec!["mueller", "koeln", "50667"]);
        assert_eq!(tokenize("a bc straße"), vec!["strasse"]);
        // deduped
        assert_eq!(tokenize("koeln Köln"), vec!["koeln"]);
        assert!(tokenize("a b").is_empty());
    }

    #[test]
    fn scoring_weights_zip_then_identity_then_free_text() {
        let row = json!({
            "tracking_number": "00340434123456789012",
            "recipient_name": "Müller GmbH",
            "recipient_city": "Köln",
            "recipient_zip": "50667",
            "recipient_street": "Domkloster 4",
            "description": "DHL Paket",
            "reference": "REP-20260101-0001",
        });
        assert_eq!(score_row(&row, &tokenize("50667")), 3);
        assert_eq!(score_row(&row, &tokenize("mueller")), 2);
        assert_eq!(score_row(&row, &tokenize("koeln")), 2);
        assert_eq!(score_row(&row, &tokenize("domkloster")), 1);
        // additive across tokens
        assert_eq!(score_row(&row, &tokenize("Müller 50667")), 5);
        assert_eq!(score_row(&row, &tokenize("nothing here")), 0);
    }

    /// The field case: the PDA scans the OPAL label barcode, which WRAPS the
    /// 12-digit HWB in a `SEVFRA`+2 prefix and a 2-digit suffix. Nothing matched
    /// it before — the lookup returned `[]` on a parcel that was in the table.
    #[test]
    fn barcode_containment_matches_wrapped_and_exact_ids() {
        let opal = json!({
            "tracking_number": "AB-12345",
            "hwb_number": "041940787090",
            "reference": "REP-20260101-0001",
        });
        // label barcode SEVFRA + 09 + <hwb> + 01 → the HWB, contained
        assert_eq!(
            containment_score(&opal, &norm_id("SEVFRA0904194078709001")),
            Some(800)
        );
        // …and the same barcode with the OCR of the rest of the camera frame
        // glued on: "q contains the identifier" must survive the extra words.
        assert_eq!(
            containment_score(
                &opal,
                &norm_id("SEVFRA0904194078709001 Joy Fitness Buxtehude DE-21614 Zum Fruchthof 7")
            ),
            Some(800)
        );
        // the bare HWB scanned on its own still hits
        assert_eq!(containment_score(&opal, &norm_id("041940787090")), Some(800));

        let dhl = json!({ "tracking_number": "00340434326960038335" });
        assert_eq!(
            containment_score(&dhl, &norm_id("00340434326960038335")),
            Some(800)
        );
        // punctuation and case are flattened on both ends
        assert_eq!(
            containment_score(&dhl, &norm_id(" 0034 0434 3269 6003 8335 ")),
            Some(800)
        );

        // reference is the weaker identity class
        let ref_only = json!({ "reference": "REP-20260101-0001" });
        assert_eq!(
            containment_score(&ref_only, &norm_id("REP-20260101-0001")),
            Some(700)
        );

        // A short query must NOT trigger containment — "one contains the other"
        // over 5 chars would match half the table.
        assert_eq!(containment_score(&dhl, &norm_id("00340")), None);
        // …nor may a short stored identifier swallow a long barcode.
        let short_id = json!({ "reference": "A-1" });
        assert_eq!(
            containment_score(&short_id, &norm_id("SEVFRA0904194078709001")),
            None
        );
        // and an unrelated parcel stays unmatched
        assert_eq!(
            containment_score(&dhl, &norm_id("SEVFRA0904194078709001")),
            None
        );
    }

    /// The whole point of the projection: rows whose `updated_at` is still a
    /// legacy RFC3339 STRING must sort next to the real datetimes, not after
    /// every one of them (SurrealDB orders by type first).
    #[tokio::test]
    async fn shipment_select_normalizes_mixed_updated_at_types() {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("test").use_db("test").await.unwrap();
        db.query("DEFINE TABLE IF NOT EXISTS stock_picking_delivery SCHEMALESS")
            .await
            .unwrap();
        // old (datetime) / newest (legacy string) / middle (datetime)
        db.query(
            "CREATE stock_picking_delivery:old SET tracking_number = 'OLD', updated_at = d'2020-01-01T00:00:00Z'; \
             CREATE stock_picking_delivery:new SET tracking_number = 'NEW', updated_at = '2030-01-01T00:00:00+00:00'; \
             CREATE stock_picking_delivery:mid SET tracking_number = 'MID', updated_at = d'2025-01-01T00:00:00Z';              CREATE stock_picking_delivery:none SET tracking_number = 'NONE';",
        )
        .await
        .unwrap();

        let sql = format!("{SHIPMENT_SELECT} ORDER BY updated_at DESC LIMIT 200");
        let rows: Vec<Value> = db.query(sql).await.unwrap().take(0).unwrap();
        let order: Vec<String> = rows.iter().map(|r| field(r, "tracking_number")).collect();
        // …and a row with NO updated_at at all must not error the query out.
        assert_eq!(order, vec!["NEW", "MID", "OLD", "NONE"]);
    }
}
