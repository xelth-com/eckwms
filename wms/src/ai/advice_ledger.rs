//! Remote-advice ledger → xelixir KB sync (`advice_ledger_sync`).
//!
//! Interim bridge until `device_event` kind `ADVICE` exists (ROADMAP "Remote
//! advice → outcome statistics"): the hand-kept, append-only table
//! `.eck/advice/REMOTE_ADVICE_LEDGER.md` is parsed (READ-ONLY — this module never
//! writes the file) and each decided row becomes KB evidence (xelixir
//! kb-curation CONTRACT §7):
//!
//! * `HELPED` + a cited `kb_finding:<id>` → `kb_verify {finding_id, method:
//!   "repair_success", author: "9eck-ledger"}` on every cited finding;
//! * `NOT_HELPED` → `kb_record {kind: "negative", polarity: "neg", origin:
//!   {system: "ledger", id: <RA id>}}` — "<advice> did not resolve <symptom> on <model>";
//! * `OPEN` (and anything else) → skipped until the row is decided.
//!
//! Idempotent through the local table `advice_ledger_sync {ra_id, action,
//! result_id, at}`: an action already recorded there is never re-sent (the
//! server side is idempotent too — origin for kb_record, evidence_sha256 for
//! kb_verify). Runs from the repair-distiller worker on its own tick (default
//! 6 h, `ECK_ADVICE_LEDGER_SYNC_SECS`), only when the xelixir bridge is
//! configured; `ECK_ADVICE_LEDGER_SYNC=off` disables it,
//! `ECK_ADVICE_LEDGER_PATH` overrides the file location.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use regex::Regex;
use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::AppState;

const DEFAULT_INTERVAL_SECS: u64 = 6 * 3600;
const DEFAULT_LEDGER_PATH: &str = ".eck/advice/REMOTE_ADVICE_LEDGER.md";
const AUTHOR: &str = "9eck-ledger";

/// False only when `ECK_ADVICE_LEDGER_SYNC` is `0|off|false|no`. The loop
/// itself additionally no-ops while the xelixir bridge is unconfigured.
pub fn enabled() -> bool {
    !matches!(
        std::env::var("ECK_ADVICE_LEDGER_SYNC").unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "0" | "off" | "false" | "no"
    )
}

/// Sync period: `ECK_ADVICE_LEDGER_SYNC_SECS` (min 300 s), default 6 h.
fn interval_secs() -> u64 {
    std::env::var("ECK_ADVICE_LEDGER_SYNC_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.max(300))
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

/// Ledger location: `ECK_ADVICE_LEDGER_PATH`, default the repo-relative path.
fn ledger_path() -> String {
    std::env::var("ECK_ADVICE_LEDGER_PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LEDGER_PATH.to_string())
}

/// The periodic loop, spawned by the repair-distiller worker. Each tick reads the
/// ledger and sends the not-yet-synced actions; a missing file or unconfigured
/// bridge is a quiet skip.
pub async fn run_loop(state: Arc<AppState>, http: HttpClient) {
    tokio::time::sleep(Duration::from_secs(60)).await;
    let secs = interval_secs();
    info!("[AdviceLedger] sync started ({}s interval, file {})", secs, ledger_path());
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    loop {
        interval.tick().await;
        if !crate::ai::kb_client::configured() {
            debug!("[AdviceLedger] xelixir bridge not configured — skipping");
            continue;
        }
        let path = ledger_path();
        let md = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => {
                debug!("[AdviceLedger] ledger {} not readable ({}) — skipping", path, e);
                continue;
            }
        };
        match sync_once(&state, &http, &md).await {
            Ok(n) if n > 0 => info!("[AdviceLedger] synced {n} new ledger action(s) to xelixir KB"),
            Ok(_) => {}
            Err(e) => warn!("[AdviceLedger] sync failed: {}", e),
        }
    }
}

/// Outcome column of a ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Helped,
    NotHelped,
    Open,
    /// PARTIAL / UNKNOWN / blank / anything else — not synced.
    Other(String),
}

impl Outcome {
    /// Parse the `outcome` cell (case-insensitive; blank → `Open`).
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "HELPED" => Outcome::Helped,
            "NOT_HELPED" => Outcome::NotHelped,
            "OPEN" | "" => Outcome::Open,
            o => Outcome::Other(o.to_string()),
        }
    }
}

/// One parsed ledger row (only the columns the sync needs).
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerRow {
    pub ra_id: String,
    pub ticket: String,
    pub serial: String,
    pub model: String,
    pub symptom: String,
    /// The controlled-vocabulary advice codes in the "advice given (type)" cell
    /// (`UPPER_SNAKE` tokens); the trimmed cell itself when it has none.
    pub advice_types: Vec<String>,
    pub outcome: Outcome,
    pub outcome_evidence: String,
    /// Every `kb_finding:<id>` cited anywhere in the row, in order, de-duplicated.
    pub finding_ids: Vec<String>,
}

/// Split one markdown table line into trimmed cells (outer pipes dropped).
fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// `UPPER_SNAKE` advice codes (at least one underscore) in an advice cell.
fn advice_codes(cell: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+\b").expect("valid advice-code regex"));
    let mut out: Vec<String> = Vec::new();
    for m in re.find_iter(cell) {
        if !out.iter().any(|x| x == m.as_str()) {
            out.push(m.as_str().to_string());
        }
    }
    if out.is_empty() && !cell.trim().is_empty() {
        out.push(cell.trim().to_string());
    }
    out
}

/// Parse the ledger markdown. Columns are located by HEADER NAME (`id`,
/// `ticket`, `serial`, `model`, `symptom…`, `advice…`, `outcome`,
/// `outcome_evidence`), so a reordered or widened table still parses; rows are
/// the table lines whose first cell starts with `RA-`.
pub fn parse_ledger(md: &str) -> Vec<LedgerRow> {
    let mut idx: Option<std::collections::HashMap<&'static str, usize>> = None;
    let mut rows = Vec::new();
    for line in md.lines() {
        if !line.trim_start().starts_with('|') {
            continue;
        }
        let c = cells(line);
        if idx.is_none() {
            let lower: Vec<String> = c.iter().map(|x| x.to_ascii_lowercase()).collect();
            if lower.first().map(String::as_str) == Some("id") && lower.iter().any(|x| x == "outcome") {
                let mut m = std::collections::HashMap::new();
                for (i, h) in lower.iter().enumerate() {
                    let key = match h.as_str() {
                        "id" => "id",
                        "ticket" => "ticket",
                        "serial" => "serial",
                        "model" => "model",
                        "outcome" => "outcome",
                        "outcome_evidence" => "outcome_evidence",
                        h if h.starts_with("symptom") => "symptom",
                        h if h.starts_with("advice") => "advice",
                        _ => continue,
                    };
                    m.entry(key).or_insert(i);
                }
                idx = Some(m);
            }
            continue;
        }
        let Some(m) = idx.as_ref() else { continue };
        if !c.first().is_some_and(|x| x.starts_with("RA-")) {
            continue;
        }
        let get = |k: &str| m.get(k).and_then(|&i| c.get(i)).cloned().unwrap_or_default();
        rows.push(LedgerRow {
            ra_id: get("id"),
            ticket: get("ticket"),
            serial: get("serial"),
            model: get("model"),
            symptom: get("symptom"),
            advice_types: advice_codes(&get("advice")),
            outcome: Outcome::parse(&get("outcome")),
            outcome_evidence: get("outcome_evidence"),
            finding_ids: crate::ai::kb_client::parse_finding_ids(&c.join(" | ")),
        });
    }
    rows
}

/// One KB call a ledger row asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedAction {
    /// Local idempotency key, unique per row (`verify:<finding>` | `negative`).
    pub action: String,
    /// MCP tool name (`kb_verify` | `kb_record`).
    pub tool: &'static str,
    pub args: Value,
}

/// The KB calls a row maps to (pure). `HELPED` without a cited finding, `OPEN`
/// and other outcomes yield none.
pub fn plan_actions(row: &LedgerRow) -> Vec<PlannedAction> {
    match row.outcome {
        Outcome::Helped => {
            let evidence = format!("{} ticket {} serial {}", row.ra_id, row.ticket, row.serial);
            row.finding_ids
                .iter()
                .map(|fid| {
                    let sha = hex::encode(Sha256::digest(format!("{evidence}|{fid}").as_bytes()));
                    PlannedAction {
                        action: format!("verify:{fid}"),
                        tool: "kb_verify",
                        args: json!({
                            "finding_id": fid,
                            "method": "repair_success",
                            "author": AUTHOR,
                            "evidence": evidence,
                            "evidence_sha256": sha,
                        }),
                    }
                })
                .collect()
        }
        Outcome::NotHelped => {
            let advice = row.advice_types.join(" + ");
            let mut text = format!(
                "Remote advice {advice} did not resolve '{}' on {} (ticket {}, remote-advice ledger {}).",
                row.symptom, row.model, row.ticket, row.ra_id
            );
            if !row.outcome_evidence.is_empty() {
                text.push_str(&format!(" Outcome evidence: {}.", row.outcome_evidence.trim_end_matches('.')));
            }
            // The ledger is PII-free by rule; the regex scrub is the safety net.
            let (text, _) = eck_core::utils::anonymizer::scrub_pii_regex(&text);
            let mut origin = json!({ "system": "ledger", "id": row.ra_id });
            if !row.serial.is_empty() {
                origin["device"] = json!(row.serial);
            }
            let mut args = json!({
                "text": text,
                "kind": "negative",
                "polarity": "neg",
                "model": row.model,
                "author": AUTHOR,
                "confidence": "med",
                "tags": ["src:ledger"],
                "origin": origin,
            });
            if !row.serial.is_empty() {
                args["device_uuid"] = json!(row.serial);
            }
            vec![PlannedAction { action: "negative".into(), tool: "kb_record", args }]
        }
        Outcome::Open | Outcome::Other(_) => Vec::new(),
    }
}

/// The id to remember for a successful call: the candidate/finding from a
/// `kb_record` IntakeReply, the finding for a `kb_verify`. `None` = the reply
/// does not prove delivery (not recorded → retried next tick).
fn result_id(action: &PlannedAction, reply_text: &str) -> Option<String> {
    match action.tool {
        "kb_record" => {
            let r = crate::ai::kb_client::parse_intake_reply(reply_text)?;
            r.candidate_id.or(r.finding_id)
        }
        _ => {
            let v: Value = serde_json::from_str(reply_text).ok()?;
            if v.get("ok").and_then(|b| b.as_bool()) == Some(false) {
                return None;
            }
            action.args.get("finding_id").and_then(|f| f.as_str()).map(String::from)
        }
    }
}

/// One pass over the ledger text: for every planned action not yet in
/// `advice_ledger_sync`, call the KB tool and record `{ra_id, action,
/// result_id, at}` on success. Returns the number of actions sent.
pub async fn sync_once(state: &Arc<AppState>, http: &HttpClient, md: &str) -> anyhow::Result<usize> {
    // Define first so the SELECT below doesn't error on a fresh node (SurrealDB 3
    // refuses a SELECT on an undefined table); the UNIQUE index makes a racing
    // double-CREATE of one (ra_id, action) fail instead of duplicating.
    state
        .db
        .query(
            "DEFINE TABLE IF NOT EXISTS advice_ledger_sync SCHEMALESS;              DEFINE INDEX IF NOT EXISTS advice_ledger_sync_key ON advice_ledger_sync FIELDS ra_id, action UNIQUE;",
        )
        .await?
        .check()?;
    let mut sent = 0usize;
    for row in parse_ledger(md) {
        let plan = plan_actions(&row);
        if plan.is_empty() {
            if row.outcome == Outcome::Helped {
                debug!("[AdviceLedger] {} HELPED but cites no kb_finding — nothing to verify", row.ra_id);
            }
            continue;
        }
        for a in plan {
            let done: Vec<Value> = state
                .db
                .query("SELECT ra_id FROM advice_ledger_sync WHERE ra_id = $ra AND action = $ac LIMIT 1")
                .bind(("ra", row.ra_id.clone()))
                .bind(("ac", a.action.clone()))
                .await?
                .take(0)?;
            if !done.is_empty() {
                continue;
            }
            let text = match crate::ai::kb_client::call_tool(http, a.tool, a.args.clone(), Duration::from_secs(30)).await {
                Ok(t) => t,
                Err(e) => {
                    warn!("[AdviceLedger] {} {} failed: {}", row.ra_id, a.action, e);
                    continue;
                }
            };
            let Some(rid) = result_id(&a, &text) else {
                warn!("[AdviceLedger] {} {}: reply did not confirm delivery — will retry", row.ra_id, a.action);
                continue;
            };
            state
                .db
                .query("CREATE advice_ledger_sync CONTENT { ra_id: $ra, action: $ac, result_id: $rid, at: time::now() }")
                .bind(("ra", row.ra_id.clone()))
                .bind(("ac", a.action.clone()))
                .bind(("rid", rid.clone()))
                .await?
                .check()?;
            info!("[AdviceLedger] {} {} -> {}", row.ra_id, a.action, rid);
            sent += 1;
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Header + the real rows of `.eck/advice/REMOTE_ADVICE_LEDGER.md` as of
    /// 2026-10-31, plus one synthetic HELPED row (the ledger has none yet).
    const LEDGER: &str = r#"# Remote-advice ledger (interim)

| id | date | ticket | serial | model | symptom (our reading of the QC report / evidence) | advice given (type) | expected signal if right | outcome | outcome_date | outcome_evidence |
|---|---|---|---|---|---|---|---|---|---|---|
| RA-2026-0001 | 2026-07-20 | 32115 | I422000201 | Acme 970 | abort at 80 % with contact screen | RELOCATE_AWAY_FROM_EMI | abort rate drops after the move | NOT_HELPED | 2026-10-21 | customer reopened as 33714: EMI was never indicated |
| RA-2026-0002 | 2026-10-30 | 33714 | I422000201 | Acme 970 | right-hand electrode contact/connector (kb_finding:jhwye58i7sjho819ehhe) | TAP_TEST_RIGHT_HAND_SENSOR + INSPECT_PLASTIC_SEALING + ASK_CLEANING_SPRAY_HABIT + PLAN_ONSITE_CLEANING + DEACTIVATE_LBWEB_SETUP_ITEM_4 | tap test confirms contact → on-site cleaning fixes | OPEN | | |
| RA-2026-9999 | 2026-10-31 | 40000 | F91802087 | Acme 270 | date resets (kb_finding:abc123) | CLEAN_CONTACT | resets stop | HELPED | 2026-10-31 | customer confirmed |
"#;

    #[test]
    fn parses_real_rows() {
        let rows = parse_ledger(LEDGER);
        assert_eq!(rows.len(), 3);
        let r1 = &rows[0];
        assert_eq!(r1.ra_id, "RA-2026-0001");
        assert_eq!(r1.ticket, "32115");
        assert_eq!(r1.serial, "I422000201");
        assert_eq!(r1.model, "Acme 970");
        assert_eq!(r1.outcome, Outcome::NotHelped);
        assert_eq!(r1.advice_types, vec!["RELOCATE_AWAY_FROM_EMI".to_string()]);
        assert!(r1.finding_ids.is_empty());

        let r2 = &rows[1];
        assert_eq!(r2.outcome, Outcome::Open);
        assert_eq!(r2.finding_ids, vec!["kb_finding:jhwye58i7sjho819ehhe".to_string()]);
        assert_eq!(r2.advice_types.len(), 5);
        assert_eq!(r2.advice_types[0], "TAP_TEST_RIGHT_HAND_SENSOR");
        assert!(r2.outcome_evidence.is_empty());

        assert_eq!(rows[2].outcome, Outcome::Helped);
    }

    #[test]
    fn plans_actions_per_outcome() {
        let rows = parse_ledger(LEDGER);
        let neg = plan_actions(&rows[0]);
        assert_eq!(neg.len(), 1);
        assert_eq!(neg[0].tool, "kb_record");
        assert_eq!(neg[0].action, "negative");
        let a = &neg[0].args;
        assert_eq!(a["kind"], "negative");
        assert_eq!(a["polarity"], "neg");
        assert_eq!(a["origin"]["system"], "ledger");
        assert_eq!(a["origin"]["id"], "RA-2026-0001");
        assert_eq!(a["author"], "9eck-ledger");
        assert_eq!(a["tags"], json!(["src:ledger"]));
        assert_eq!(a["model"], "Acme 970");
        let t = a["text"].as_str().unwrap();
        assert!(t.starts_with("Remote advice RELOCATE_AWAY_FROM_EMI did not resolve 'abort at 80 % with contact screen' on Acme 970 (ticket 32115"), "{t}");

        assert!(plan_actions(&rows[1]).is_empty(), "OPEN is skipped");

        let ver = plan_actions(&rows[2]);
        assert_eq!(ver.len(), 1);
        assert_eq!(ver[0].tool, "kb_verify");
        assert_eq!(ver[0].action, "verify:kb_finding:abc123");
        assert_eq!(ver[0].args["method"], "repair_success");
        assert_eq!(ver[0].args["evidence"], "RA-2026-9999 ticket 40000 serial F91802087");
        assert_eq!(ver[0].args["evidence_sha256"].as_str().unwrap().len(), 64);
        // Deterministic → server-side idempotent.
        assert_eq!(plan_actions(&rows[2]), ver);
    }

    #[test]
    fn result_id_requires_delivery() {
        let rows = parse_ledger(LEDGER);
        let neg = &plan_actions(&rows[0])[0];
        assert_eq!(
            result_id(neg, r#"{"candidate_id":"kb_candidate:aa","outcome":"new_candidate","finding_id":null}"#).as_deref(),
            Some("kb_candidate:aa")
        );
        assert!(result_id(neg, "not json").is_none());
        let ver = &plan_actions(&rows[2])[0];
        assert_eq!(result_id(ver, r#"{"ok":true,"finding":{},"target":{}}"#).as_deref(), Some("kb_finding:abc123"));
        assert!(result_id(ver, r#"{"ok":false}"#).is_none());
    }
}
