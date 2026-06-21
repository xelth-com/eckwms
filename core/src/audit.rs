//! Tamper-evident audit chain — shared by the Kasse (`pos`) and `wms`.
//!
//! Each fiscal / inventory mutation appends one **append-only** [`AuditEvent`]
//! into a per-`chain_id` SHA-256 hash-chain (`prev_hash → this_hash`), signed
//! with the server's Ed25519 identity. Periodically (or immediately, for
//! `fiscal-touch` events) the un-anchored events are Merkle-batched and the
//! root is published to Hedera HCS via [`crate::sync::hedera`], producing a
//! second [`Anchor`] chain that ties our private log to a public, timestamped
//! ledger.
//!
//! This is the GoBD *Unveränderbarkeit* + *Nachprüfbarkeit* layer: it sits
//! **next to** (never replaces) the BSI-certified TSE for the Kasse — see
//! `.eck/AUDIT_CHAIN_9ECK.md`. Nothing here is encrypted; only hashes leave the
//! box, so the records stay readable for a tax audit (`§ 147 AO`).
//!
//! The wire format is locked against `.eck/verify_audit.py` /
//! `.eck/verify_anchor.py` — an auditor recomputes the chain with stdlib Python
//! and no access to our server. **Do not change [`event_preimage`] without
//! updating those verifiers in lockstep.**

use crate::db::SurrealDb;
use crate::utils::identity::ServerIdentity;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use surrealdb::types::SurrealValue;
use tracing::{debug, warn};

/// Sentinel `prev_hash` for the first event in a chain (and the first anchor).
pub const GENESIS: &str = "GENESIS";

/// Event class — drives the anchoring cadence and the compliance posture.
///
/// * `read`        — a query / inspection; batch-anchored.
/// * `mutate`      — any state change (the WMS default); batch-anchored.
/// * `fiscal-touch`— a write that touches a fiscal record (Kasse); anchored
///   immediately because German fiscal law treats it as evidentiary.
pub mod class {
    pub const READ: &str = "read";
    pub const MUTATE: &str = "mutate";
    pub const FISCAL_TOUCH: &str = "fiscal-touch";
}

/// One link in a per-`chain_id` tamper-evident chain.
#[derive(Debug, Clone, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct AuditEvent {
    /// 1-based, strictly monotonic **within** `chain_id`. A gap = a removed event.
    pub seq: u64,
    /// e.g. `"9eck:kasse:<kasse_id>"` or `"9eck:wms:<node_id>"`.
    pub chain_id: String,
    pub ts_ms: i64,
    /// Who/what acted (user id, device id, `"system"`).
    pub actor: String,
    /// Machine action name, e.g. `"tx.finish"`, `"storno.approve"`, `"inventory.move"`.
    pub action: String,
    pub class: String,
    /// Human-readable one-liner for the audit UI.
    pub summary: String,
    /// SHA-256 (hex) of the canonical JSON payload — the payload itself is NOT
    /// stored here (GDPR / trade-secret safe; only its hash is chained/anchored).
    pub payload_hash: String,
    /// `this_hash` of the previous event in the chain, or [`GENESIS`].
    pub prev_hash: String,
    /// SHA-256 (hex) of [`event_preimage`].
    pub this_hash: String,
    /// Ed25519 signature (hex) over the `this_hash` string. `"UNSIGNED"` if no key.
    pub sig: String,
    /// Signer public key (hex) — lets a verifier pick the right key per event.
    pub signer_pub: String,
    /// Set once the event has been folded into an [`Anchor`] Merkle batch.
    pub anchored: bool,
}

/// One link in the anchor chain — a Merkle batch of events sealed in Hedera.
#[derive(Debug, Clone, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct Anchor {
    pub anchor_seq: u64,
    pub batch_root: String,
    pub from_ts: i64,
    pub to_ts: i64,
    pub count: u64,
    pub prev_anchor: String,
    pub this_hash: String,
    /// HCS topic the root was submitted to (`0.0.N`), `None` if Hedera unconfigured.
    pub hcs_topic: Option<String>,
    /// Mirror-node transaction id (`0.0.op-secs-nanos`) returned by the node.
    pub hcs_tx_id: Option<String>,
    /// Consensus sequence number / timestamp — filled later from the mirror node
    /// (a node submit returns only a precheck + tx id, not the consensus result).
    pub hcs_seq: Option<u64>,
    pub hcs_timestamp: Option<String>,
    pub created_at: String,
}

// ── hashing / preimage ───────────────────────────────────────────────────────

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// The byte-exact preimage of an event's `this_hash`.
///
/// **Locked** against `.eck/verify_audit.py::preimage`. Field order and the
/// `|` separator are part of the on-disk contract — changing either silently
/// breaks every external verifier and every previously sealed chain.
pub fn event_preimage(
    seq: u64,
    chain_id: &str,
    ts_ms: i64,
    actor: &str,
    action: &str,
    class: &str,
    payload_hash: &str,
    prev_hash: &str,
) -> String {
    format!("{seq}|{chain_id}|{ts_ms}|{actor}|{action}|{class}|{payload_hash}|{prev_hash}")
}

/// SHA-256 (hex) of the canonical JSON of `payload`. serde_json sorts map keys
/// by default (no `preserve_order`), so this is stable for equal payloads.
pub fn payload_hash(payload: &Value) -> String {
    sha256_hex(&serde_json::to_string(payload).unwrap_or_default())
}

// ── signing ──────────────────────────────────────────────────────────────────

/// Sign `msg` with the identity's Ed25519 key, returning a **hex** signature
/// and the **hex** public key. Hex (not base64) to match `verify_audit.py`,
/// which does `bytes.fromhex(sig)` / `bytes.fromhex(pubkey)`.
fn sign_hex(identity: &ServerIdentity, msg: &str) -> (String, String) {
    let decode = |b64: &str| BASE64.decode(b64).ok().filter(|v| v.len() == 32);
    match (decode(&identity.private_key), decode(&identity.public_key)) {
        (Some(sk), Some(pk)) => {
            let arr: [u8; 32] = sk.try_into().unwrap();
            let signing = SigningKey::from_bytes(&arr);
            let sig = signing.sign(msg.as_bytes());
            (hex::encode(sig.to_bytes()), hex::encode(pk))
        }
        _ => {
            warn!("audit: identity key unusable; appending UNSIGNED event");
            ("UNSIGNED".to_string(), String::new())
        }
    }
}

// ── append ───────────────────────────────────────────────────────────────────

/// Append one event to `chain_id`. Reads the chain head, links + signs, and
/// persists into the `audit_event` table. Returns the sealed event.
///
/// Low-rate by construction (one per fiscal/inventory mutation); the head read
/// + insert is not transactional, so two truly-simultaneous appends to the
/// *same* chain could race on `seq`. Acceptable for a single-writer Kasse/WMS
/// node; see `.eck/AUDIT_CHAIN_9ECK.md` for the per-node-writer assumption.
pub async fn append(
    db: &SurrealDb,
    identity: &ServerIdentity,
    chain_id: &str,
    actor: &str,
    action: &str,
    class: &str,
    summary: &str,
    payload: Value,
) -> anyhow::Result<AuditEvent> {
    // 1. chain head (highest seq for this chain).
    let head: Option<Value> = db
        .query("SELECT seq, this_hash FROM audit_event WHERE chain_id = $c ORDER BY seq DESC LIMIT 1")
        .bind(("c", chain_id.to_string()))
        .await?
        .take(0)?;

    let (prev_seq, prev_hash) = match head {
        Some(v) => (
            v.get("seq").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("this_hash")
                .and_then(|x| x.as_str())
                .unwrap_or(GENESIS)
                .to_string(),
        ),
        None => (0, GENESIS.to_string()),
    };

    let seq = prev_seq + 1;
    let ts_ms = Utc::now().timestamp_millis();
    let p_hash = payload_hash(&payload);

    // 2. link.
    let preimage = event_preimage(
        seq, chain_id, ts_ms, actor, action, class, &p_hash, &prev_hash,
    );
    let this_hash = sha256_hex(&preimage);

    // 3. sign the this_hash string.
    let (sig, signer_pub) = sign_hex(identity, &this_hash);

    let event = AuditEvent {
        seq,
        chain_id: chain_id.to_string(),
        ts_ms,
        actor: actor.to_string(),
        action: action.to_string(),
        class: class.to_string(),
        summary: summary.to_string(),
        payload_hash: p_hash,
        prev_hash,
        this_hash,
        sig,
        signer_pub,
        anchored: false,
    };

    // 4. persist (random record id; the chain order lives in `seq`/`prev_hash`).
    let _: Option<Value> = db
        .create("audit_event")
        .content(serde_json::to_value(&event)?)
        .await?;

    debug!(
        "audit append {} #{} {} ({})",
        chain_id, event.seq, action, class
    );
    Ok(event)
}

/// Fire-and-forget [`append`] that logs instead of propagating errors — audit
/// logging must never break the business transaction it records.
pub async fn append_soft(
    db: &SurrealDb,
    identity: &ServerIdentity,
    chain_id: &str,
    actor: &str,
    action: &str,
    class: &str,
    summary: &str,
    payload: Value,
) {
    if let Err(e) = append(
        db, identity, chain_id, actor, action, class, summary, payload,
    )
    .await
    {
        warn!("audit append failed ({chain_id}/{action}): {e}");
    }
}

// ── Merkle root ──────────────────────────────────────────────────────────────

/// Binary Merkle root over ordered leaf hashes. Odd nodes are duplicated; a
/// parent is `sha256_hex(left || right)`. Empty → `GENESIS`. One leaf → itself.
///
/// Locked against `.eck/verify_anchor.py::merkle_root`.
pub fn merkle_root(leaves: &[String]) -> String {
    if leaves.is_empty() {
        return GENESIS.to_string();
    }
    let mut level: Vec<String> = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            let left = &level[i];
            let right = if i + 1 < level.len() { &level[i + 1] } else { left };
            next.push(sha256_hex(&format!("{left}{right}")));
            i += 2;
        }
        level = next;
    }
    level.into_iter().next().unwrap()
}

// ── anchoring ────────────────────────────────────────────────────────────────

/// Merkle-batch every event newer than the last anchor, submit the root to
/// Hedera HCS over gRPC ([`crate::anchor`]), and append to the `audit_anchor`
/// chain. Returns `Ok(None)` when Hedera is unconfigured or nothing is pending.
///
/// The committed batch is `events WHERE ts_ms > prev_anchor.to_ts`, ordered
/// `(ts_ms, chain_id, seq)`, and the HCS message + anchor preimage are **locked**
/// byte-for-byte against `.eck/verify_anchor.py` so an auditor can recompute the
/// Merkle root from the public ledger and our DB export. Every `audit_anchor`
/// row therefore corresponds to a real, accepted ledger message — if the node
/// rejects the submit we bail and retry next cycle (the chain does not advance).
pub async fn anchor_pending(db: &SurrealDb) -> anyhow::Result<Option<Anchor>> {
    if !crate::anchor::is_configured() {
        debug!("audit anchor: Hedera unconfigured (HEDERA_ACCOUNT_ID/KEY/TOPIC_ID) — skipping");
        return Ok(None);
    }
    let topic_num = std::env::var("HEDERA_TOPIC_ID")
        .ok()
        .and_then(|s| crate::anchor::entity_num(&s))
        .ok_or_else(|| anyhow::anyhow!("HEDERA_TOPIC_ID unset/bad"))?;

    // 1. anchor-chain head (drives the batch lower bound, exclusive).
    let head: Option<Value> = db
        .query("SELECT anchor_seq, to_ts, this_hash FROM audit_anchor ORDER BY anchor_seq DESC LIMIT 1")
        .await?
        .take(0)?;
    let (prev_seq, prev_to_ts, prev_anchor) = match head {
        Some(v) => (
            v.get("anchor_seq").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("to_ts").and_then(|x| x.as_i64()).unwrap_or(0),
            v.get("this_hash").and_then(|x| x.as_str()).unwrap_or(GENESIS).to_string(),
        ),
        None => (0, 0, GENESIS.to_string()),
    };

    // 2. pending events, ordered exactly as verify_anchor.py recomputes them.
    let pending: Vec<Value> = db
        .query("SELECT this_hash, ts_ms FROM audit_event WHERE ts_ms > $f ORDER BY ts_ms ASC, chain_id ASC, seq ASC")
        .bind(("f", prev_to_ts))
        .await?
        .take(0)?;
    if pending.is_empty() {
        return Ok(None);
    }

    let leaves: Vec<String> = pending
        .iter()
        .filter_map(|v| v.get("this_hash").and_then(|x| x.as_str()).map(String::from))
        .collect();
    let from_ts = pending
        .first()
        .and_then(|v| v.get("ts_ms").and_then(|x| x.as_i64()))
        .unwrap_or(prev_to_ts);
    let to_ts = pending
        .last()
        .and_then(|v| v.get("ts_ms").and_then(|x| x.as_i64()))
        .unwrap_or(from_ts);
    let count = leaves.len() as u64;
    let batch_root = merkle_root(&leaves);
    let anchor_seq = prev_seq + 1;

    // 3. link the anchor chain (preimage order locked to verify_anchor.py).
    let this_hash = sha256_hex(&format!(
        "{anchor_seq}|{batch_root}|{from_ts}|{to_ts}|{count}|{prev_anchor}"
    ));

    // 4. the HCS message is this JSON object (what the verifier reads back).
    let msg = serde_json::json!({
        "v": 1,
        "anchor_seq": anchor_seq,
        "batch_root": batch_root,
        "from_ts": from_ts,
        "to_ts": to_ts,
        "count": count,
        "prev_anchor": prev_anchor,
        "this_hash": this_hash,
    })
    .to_string();

    // 5. submit over gRPC; only record the anchor if the node accepted it.
    let (precheck, tx_id) = crate::anchor::submit_message(topic_num, msg.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("HCS submit: {e}"))?;
    if precheck != 0 {
        anyhow::bail!("HCS precheck {precheck} (not accepted) — not advancing anchor chain");
    }

    let anchor = Anchor {
        anchor_seq,
        batch_root,
        from_ts,
        to_ts,
        count,
        prev_anchor,
        this_hash,
        hcs_topic: Some(format!("0.0.{topic_num}")),
        hcs_tx_id: Some(tx_id),
        hcs_seq: None,
        hcs_timestamp: None,
        created_at: Utc::now().to_rfc3339(),
    };

    let _: Option<Value> = db
        .create("audit_anchor")
        .content(serde_json::to_value(&anchor)?)
        .await?;

    // mark the folded events anchored (UI / status only; selection above is by ts).
    let _: Vec<Value> = db
        .query("UPDATE audit_event SET anchored = true WHERE anchored = false AND ts_ms <= $to")
        .bind(("to", to_ts))
        .await?
        .take(0)?;

    debug!(
        "audit anchor #{} sealed {} events → HCS {} root={}",
        anchor.anchor_seq,
        anchor.count,
        anchor.hcs_tx_id.as_deref().unwrap_or("-"),
        anchor.batch_root
    );
    Ok(Some(anchor))
}

// ── consensus backfill ───────────────────────────────────────────────────────

/// Backfill the Hedera **consensus** sequence number + timestamp into
/// `audit_anchor` rows that were written from the node submit (which returns
/// only a precheck + a tx id — the consensus result lands a few seconds later).
///
/// Reads the public mirror node, decodes each topic message, and matches it to
/// our anchor by `anchor_seq` **and** `this_hash` (so a spoofed message can't
/// backfill the wrong row). Result: each anchor becomes a **self-contained
/// proof** — the network's consensus time lives in our own DB, no live Hedera
/// call needed to read it back. Returns the number of rows updated. No-op (0)
/// when Hedera is unconfigured or nothing is missing.
pub async fn backfill_anchor_consensus(db: &SurrealDb) -> anyhow::Result<u64> {
    let topic = match std::env::var("HEDERA_TOPIC_ID") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return Ok(0),
    };

    // Anchors still missing consensus info (filter in Rust — NULL handling is
    // fiddly across SurrealDB versions, and anchors are few).
    let rows: Vec<Value> = db
        .query("SELECT anchor_seq, this_hash, hcs_seq FROM audit_anchor ORDER BY anchor_seq DESC LIMIT 500")
        .await?
        .take(0)?;
    let missing: Vec<(u64, String)> = rows
        .iter()
        .filter(|v| v.get("hcs_seq").map(|x| x.is_null()).unwrap_or(true))
        .filter_map(|v| {
            Some((
                v.get("anchor_seq")?.as_u64()?,
                v.get("this_hash")?.as_str()?.to_string(),
            ))
        })
        .collect();
    if missing.is_empty() {
        return Ok(0);
    }

    // Recent topic messages from the mirror node (anchors are infrequent; the
    // newest 100 cover everything not yet backfilled in practice).
    let url = format!(
        "{}/api/v1/topics/{}/messages?limit=100&order=desc",
        crate::anchor::mirror_base_url(),
        topic
    );
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("mirror {url}: HTTP {}", resp.status());
    }
    let body: Value = resp.json().await?;

    // anchor_seq -> (hcs_seq, consensus_timestamp, ledger_this_hash)
    let mut found: std::collections::HashMap<u64, (u64, String, String)> =
        std::collections::HashMap::new();
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for m in msgs {
            let (b64, seq, ts) = match (
                m.get("message").and_then(|x| x.as_str()),
                m.get("sequence_number").and_then(|x| x.as_u64()),
                m.get("consensus_timestamp").and_then(|x| x.as_str()),
            ) {
                (Some(a), Some(b), Some(c)) => (a, b, c),
                _ => continue,
            };
            let raw = match BASE64.decode(b64) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let obj: Value = match serde_json::from_slice(&raw) {
                Ok(o) => o,
                Err(_) => continue, // not one of our JSON anchors
            };
            if let (Some(aseq), Some(lh)) = (
                obj.get("anchor_seq").and_then(|x| x.as_u64()),
                obj.get("this_hash").and_then(|x| x.as_str()),
            ) {
                found.insert(aseq, (seq, ts.to_string(), lh.to_string()));
            }
        }
    }

    let mut updated = 0u64;
    for (aseq, our_hash) in missing {
        if let Some((hcs_seq, ts, ledger_hash)) = found.get(&aseq) {
            if *ledger_hash != our_hash {
                warn!(
                    "audit backfill: anchor #{aseq} ledger this_hash != local — skipping (possible tamper)"
                );
                continue;
            }
            let _: Vec<Value> = db
                .query("UPDATE audit_anchor SET hcs_seq = $s, hcs_timestamp = $t WHERE anchor_seq = $a")
                .bind(("s", *hcs_seq))
                .bind(("t", ts.clone()))
                .bind(("a", aseq))
                .await?
                .take(0)?;
            updated += 1;
        }
    }
    if updated > 0 {
        debug!("audit backfill: filled consensus data for {updated} anchor(s)");
    }
    Ok(updated)
}

// ── verification ─────────────────────────────────────────────────────────────

/// Result of an in-process chain check (mirrors `verify_audit.py`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub chain_id: String,
    pub ok: bool,
    pub count: u64,
    pub message: String,
}

/// Recompute the hash-chain for `chain_id` in process. Cheap integrity probe
/// for the admin UI / health checks; the authoritative check is the offline
/// `verify_audit.py` an auditor runs themselves.
pub async fn verify_chain(db: &SurrealDb, chain_id: &str) -> anyhow::Result<VerifyReport> {
    let events: Vec<AuditEvent> = db
        .query("SELECT seq, chain_id, ts_ms, actor, action, class, summary, payload_hash, prev_hash, this_hash, sig, signer_pub, anchored FROM audit_event WHERE chain_id = $c ORDER BY seq ASC")
        .bind(("c", chain_id.to_string()))
        .await?
        .take(0)?;

    let mut prev = GENESIS.to_string();
    let mut expect = 1u64;
    for e in &events {
        if e.seq != expect {
            return Ok(VerifyReport {
                chain_id: chain_id.to_string(),
                ok: false,
                count: events.len() as u64,
                message: format!("sequence gap at #{} (expected {expect}) — event removed", e.seq),
            });
        }
        if e.prev_hash != prev {
            return Ok(VerifyReport {
                chain_id: chain_id.to_string(),
                ok: false,
                count: events.len() as u64,
                message: format!("broken link at #{}", e.seq),
            });
        }
        let recomputed = sha256_hex(&event_preimage(
            e.seq, &e.chain_id, e.ts_ms, &e.actor, &e.action, &e.class, &e.payload_hash, &e.prev_hash,
        ));
        if recomputed != e.this_hash {
            return Ok(VerifyReport {
                chain_id: chain_id.to_string(),
                ok: false,
                count: events.len() as u64,
                message: format!("tampered event #{} (this_hash mismatch)", e.seq),
            });
        }
        prev = e.this_hash.clone();
        expect += 1;
    }

    Ok(VerifyReport {
        chain_id: chain_id.to_string(),
        ok: true,
        count: events.len() as u64,
        message: format!("hash-chain intact for {chain_id}"),
    })
}

/// Fetch the full ordered chain for `chain_id` (for the audit UI / export).
pub async fn chain_events(db: &SurrealDb, chain_id: &str) -> anyhow::Result<Vec<AuditEvent>> {
    let events: Vec<AuditEvent> = db
        .query("SELECT seq, chain_id, ts_ms, actor, action, class, summary, payload_hash, prev_hash, this_hash, sig, signer_pub, anchored FROM audit_event WHERE chain_id = $c ORDER BY seq ASC")
        .bind(("c", chain_id.to_string()))
        .await?
        .take(0)?;
    Ok(events)
}

/// Convenience chain-id builders so callers agree on the namespace.
pub fn kasse_chain(kasse_id: &str) -> String {
    format!("9eck:kasse:{kasse_id}")
}
pub fn wms_chain(node_id: &str) -> String {
    format!("9eck:wms:{node_id}")
}

/// Build the `{actor, action, class}` for a Kasse fiscal mutation. Every write
/// on a Kasse is `fiscal-touch`; pure reads stay `read`.
pub fn kasse_event(action: &str) -> &'static str {
    match action {
        a if a.starts_with("read") || a.ends_with(".read") => class::READ,
        _ => class::FISCAL_TOUCH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preimage_is_locked_format() {
        let p = event_preimage(1, "9eck:kasse:k1", 1700, "cashier", "tx.finish", class::FISCAL_TOUCH, "ph", GENESIS);
        assert_eq!(p, "1|9eck:kasse:k1|1700|cashier|tx.finish|fiscal-touch|ph|GENESIS");
    }

    #[test]
    fn merkle_known_vectors() {
        assert_eq!(merkle_root(&[]), GENESIS);
        assert_eq!(merkle_root(&["a".into()]), "a");
        // two leaves: sha256("ab")
        let two = merkle_root(&["a".into(), "b".into()]);
        assert_eq!(two, sha256_hex("ab"));
        // three leaves: parent(sha256(ab), sha256(cc))
        let three = merkle_root(&["a".into(), "b".into(), "c".into()]);
        let l = sha256_hex("ab");
        let r = sha256_hex("cc");
        assert_eq!(three, sha256_hex(&format!("{l}{r}")));
    }
}
