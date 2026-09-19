//! Relay-carried MCP channel (`/E/c/*`) — the PAID-subscription gate.
//!
//! Unlike the blind `/E/x/*` xelixir channel, `c_dispatch` is NOT a dumb pipe:
//! it admits a request only if it carries a valid, authority-signed
//! `SubscriptionCert` (`ECK_SUB_ROOT_PUBKEY`). Free clients run open-source
//! code, so this signature — produced only by the offline subscription root —
//! is the one thing they cannot forge. The relay is the enforcement point we
//! control; the check lives here, never in shipped client code.
//!
//! The relay stays blind to CONTENT: it validates the cert + the client's
//! signature over the request, then stores the opaque `mcp` body for the node
//! to pull. poll/ack/result mirror the xelixir channel on a separate
//! `client_mcp_task` table so a gated MCP request can never be confused with a
//! free ops envelope.
//!
//! ## Channel authentication (F3 hardening)
//! The capability-URLs alone are not enough: anyone who learned a node UUID
//! could `poll` it (claiming/suppressing its queued payloads) and anyone who
//! learned a `task_id` could forge an `ack`. So:
//!   * `dispatch` mints a random `result_token` and returns it ONLY to the
//!     subscriber that passed the cert gate; `result` requires it.
//!   * `poll` and `ack` must be SIGNED by the node's Ed25519 server-identity
//!     key (`core::utils::identity`). The relay binds an instance_id → pubkey
//!     on first authenticated contact (trust-on-first-use) and rejects a later
//!     mismatch.
//! `ECK_RELAY_CLIENT_MCP_AUTH=required|optional` (default `optional`) lets a
//! mixed fleet upgrade relays before nodes: `optional` accepts an unauthenticated
//! poll/ack/result but logs a warn; flip to `required` once every node signs.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde_json::{json, Value};
use tokio::sync::Notify;

use eck_core::utils::identity::verify_signature;
use eck_core::xelixir::envelope::DEFAULT_MAX_AGE_SECS;
use eck_core::xelixir::subscription::{
    read_sub_root_from_env, SignedClientMcp, DEFAULT_GRACE_SECS,
};

use crate::db::RelayDb;

/// With long-polling the hold does the pacing — the node may re-poll
/// immediately (old pollers clamp this to their [1,60] window anyway).
const POLL_INTERVAL_SECS: u64 = 1;
/// How long an empty poll is HELD waiting for a task before returning empty.
/// MUST stay comfortably under the fleet's poll client timeout (old WMS
/// pollers use 15 s), including the DB query latency on a slow relay box.
const LONG_POLL_HOLD_SECS: u64 = 8;
/// Fallback re-check cadence inside the hold (the dispatch Notify normally
/// wakes the hold instantly; this covers a notify lost to a race).
const LONG_POLL_CHECK_SECS: u64 = 2;
/// A delivered-but-unacked task is re-delivered after this long (node crashed
/// mid-execution). Before it elapses the task is INVISIBLE to poll — that, not
/// the node's transient in-flight set, is what prevents double execution.
const REDELIVER_AFTER_SECS: i64 = 60;
/// How far a node-auth signature timestamp may drift from the relay clock
/// before a poll/ack signature is rejected as stale (replay window bound).
const NODE_AUTH_MAX_AGE_SECS: i64 = 300;

// ─── channel authentication (F3) ───────────────────────────────────────────

/// Relay-side enforcement level for the client-MCP channel. `optional` (the
/// default) accepts unauthenticated poll/ack/result with a warn so relays can be
/// upgraded ahead of nodes; `required` rejects anything unsigned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AuthMode {
    Optional,
    Required,
}

fn auth_mode() -> AuthMode {
    match std::env::var("ECK_RELAY_CLIENT_MCP_AUTH")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("required") => AuthMode::Required,
        _ => AuthMode::Optional,
    }
}

/// Constant-time byte compare (no timing oracle on the per-task read secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The node's signature over a poll/ack, carried in `X-Eck-Node-*` headers.
struct NodeAuth {
    /// Base64 Ed25519 public key of the signing node.
    key: String,
    /// Unix timestamp the node signed (freshness bound).
    ts: i64,
    /// Base64 detached signature over the canonical string.
    sig: String,
}

/// Pure: pull a node-auth triple out of the request headers, if all present.
fn parse_node_auth(headers: &HeaderMap) -> Option<NodeAuth> {
    let get = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_string())
    };
    let key = get("x-eck-node-key").filter(|s| !s.is_empty())?;
    let ts = get("x-eck-node-ts").and_then(|s| s.parse::<i64>().ok())?;
    let sig = get("x-eck-node-sig").filter(|s| !s.is_empty())?;
    Some(NodeAuth { key, ts, sig })
}

/// The canonical string a node signs for a `poll` of its own queue.
fn poll_canonical(self_uuid: &str, ts: i64) -> String {
    format!("eck-cmcp-poll:{self_uuid}:{ts}")
}

/// The canonical string a node signs when it `ack`s a task's result.
fn ack_canonical(task_id: &str, ts: i64) -> String {
    format!("eck-cmcp-ack:{task_id}:{ts}")
}

/// Pure: is `auth` a fresh, cryptographically valid signature over `canonical`?
fn node_sig_valid(auth: &NodeAuth, canonical: &str, now: i64, max_age: i64) -> bool {
    if (now - auth.ts).abs() > max_age {
        return false;
    }
    verify_signature(&auth.key, canonical, &auth.sig).unwrap_or(false)
}

/// Precedence outcome of consulting the STRONG registry binding (the pubkey the
/// node proved at `/E/register`) before the trust-on-first-use table.
#[derive(Debug, PartialEq, Eq)]
enum RegistryBinding {
    /// Registry holds a key and it equals the presented one → allow.
    Match,
    /// Registry holds a key and it DIFFERS → reject, never fall through to TOFU.
    Mismatch,
    /// Registry has no key for this node → fall back to TOFU (old node).
    NoKey,
}

/// Pure precedence rule: registry key (if any) wins over TOFU.
fn classify_registry(registry_key: Option<&str>, presented: &str) -> RegistryBinding {
    match registry_key {
        Some(k) if !k.is_empty() => {
            if ct_eq(k.as_bytes(), presented.as_bytes()) {
                RegistryBinding::Match
            } else {
                RegistryBinding::Mismatch
            }
        }
        _ => RegistryBinding::NoKey,
    }
}

/// Nodes we've already logged a TOFU-fallback line for this process, so the
/// rollout notice fires once per node instead of on every poll.
fn tofu_fallback_logged(instance_id: &str) -> bool {
    static SEEN: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let set = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let mut guard = set.lock().unwrap_or_else(|p| p.into_inner());
    !guard.insert(instance_id.to_string())
}

/// Enforce the `instance_id → pubkey` binding, STRONG source first.
///
/// The authoritative key is the one the node proved at `/E/register` (stored on
/// the `registration` row): if present, the presented key MUST match it and we
/// never touch TOFU. Only when the registry has no key (a node that hasn't
/// upgraded to sign its heartbeats yet) do we fall back to the legacy
/// trust-on-first-use table, so old nodes keep working during rollout.
/// `Ok(())` = allowed; `Err(status)` = deny with that HTTP status.
async fn check_node_binding(
    db: &RelayDb,
    instance_id: &str,
    pubkey: &str,
) -> Result<(), StatusCode> {
    // STRONG: consult the node-registration pubkey first.
    let reg: Option<Value> = db
        .query("SELECT pubkey FROM registration WHERE instance_id = $id LIMIT 1")
        .bind(("id", instance_id.to_string()))
        .await
        .map_err(|e| {
            tracing::error!("client-mcp registry pubkey lookup failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();
    let registry_key = reg
        .as_ref()
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str());

    match classify_registry(registry_key, pubkey) {
        RegistryBinding::Match => return Ok(()),
        RegistryBinding::Mismatch => {
            tracing::warn!(
                "client-mcp: node {} presented a key that does NOT match its registered \
                 server-identity pubkey — rejecting (no TOFU fallback for registered nodes)",
                instance_id
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
        RegistryBinding::NoKey => {
            if !tofu_fallback_logged(instance_id) {
                tracing::info!(
                    "client-mcp: node {} has no registered pubkey yet — falling back to \
                     trust-on-first-use (upgrade the node to sign its heartbeats to get the \
                     strong binding)",
                    instance_id
                );
            }
        }
    }

    // FALLBACK — trust-on-first-use (legacy `client_mcp_node_key` table).
    let existing: Option<Value> = db
        .query("SELECT pubkey FROM type::record('client_mcp_node_key', $id) LIMIT 1")
        .bind(("id", instance_id.to_string()))
        .await
        .map_err(|e| {
            tracing::error!("client-mcp node-key lookup failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    if let Some(row) = existing {
        let bound = row.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
        if !bound.is_empty() {
            return if ct_eq(bound.as_bytes(), pubkey.as_bytes()) {
                Ok(())
            } else {
                Err(StatusCode::FORBIDDEN)
            };
        }
    }

    // Trust-on-first-use: record the binding for future requests.
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = db
        .query(
            // UPSERT, not UPDATE: in SurrealDB v3 UPDATE on a missing record is
            // a no-op, which silently re-ran TOFU on every poll (seen live
            // 2026-09-03) and never enforced the binding.
            "UPSERT type::record('client_mcp_node_key', $id) SET \
                pubkey = $pk, first_seen = $now",
        )
        .bind(("id", instance_id.to_string()))
        .bind(("pk", pubkey.to_string()))
        .bind(("now", now))
        .await
    {
        tracing::error!("client-mcp node-key TOFU insert failed: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    tracing::warn!(
        "client-mcp: TOFU bound node {} to pubkey {}…",
        instance_id,
        &pubkey[..pubkey.len().min(12)]
    );
    Ok(())
}

/// What is being authenticated (drives the canonical string).
enum AuthSubject<'a> {
    Poll(&'a str),
    Ack(&'a str),
}

/// Gate a signed node request (poll/ack). Returns `Ok(())` when the request may
/// proceed, or an HTTP status to return. Honors `ECK_RELAY_CLIENT_MCP_AUTH`.
async fn authorize_node(
    db: &RelayDb,
    headers: &HeaderMap,
    subject: AuthSubject<'_>,
    bind_id: &str,
    label: &str,
) -> Result<(), StatusCode> {
    let mode = auth_mode();
    let auth = match parse_node_auth(headers) {
        Some(a) => a,
        None => {
            return match mode {
                AuthMode::Required => {
                    tracing::warn!(
                        "client-mcp {label} REJECTED: unauthenticated and auth=required (node={bind_id})"
                    );
                    Err(StatusCode::UNAUTHORIZED)
                }
                AuthMode::Optional => {
                    tracing::warn!(
                        "client-mcp {label} accepted UNAUTHENTICATED (auth=optional, node={bind_id}) — \
                         upgrade this node to sign, then set ECK_RELAY_CLIENT_MCP_AUTH=required"
                    );
                    Ok(())
                }
            };
        }
    };

    let now = chrono::Utc::now().timestamp();
    let canonical = match subject {
        AuthSubject::Poll(u) => poll_canonical(u, auth.ts),
        AuthSubject::Ack(t) => ack_canonical(t, auth.ts),
    };
    if !node_sig_valid(&auth, &canonical, now, NODE_AUTH_MAX_AGE_SECS) {
        tracing::warn!("client-mcp {label} REJECTED: bad or stale signature (node={bind_id})");
        return Err(StatusCode::UNAUTHORIZED);
    }
    if let Err(status) = check_node_binding(db, bind_id, &auth.key).await {
        tracing::warn!(
            "client-mcp {label} REJECTED ({status}): presented key not bound to node {bind_id} (possible spoof)"
        );
        return Err(status);
    }
    Ok(())
}

/// The per-task read secret the subscriber must present on `result`.
fn provided_result_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-eck-result-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Per-target wakeups: dispatch() rings the bell, a held poll() re-checks
/// immediately instead of on the 2 s cadence. Process-local by design — the
/// client dispatches to the SAME relay the node polls.
fn task_notify(target: &str) -> Arc<Notify> {
    static NOTIFIES: OnceLock<std::sync::Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    let map = NOTIFIES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|p| p.into_inner());
    guard.entry(target.to_string()).or_default().clone()
}

// ─── POST /E/c/dispatch/:target_uuid ───────────────────────────────────────
// Admit a subscriber's MCP request for a NAT'd node, or 403. The gate.

pub async fn dispatch(
    State(db): State<RelayDb>,
    Path(target_uuid): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    // Fail closed: no configured subscription root ⇒ the channel is off, and no
    // cert could verify anyway. Never silently downgrade to an open pipe.
    let root = read_sub_root_from_env().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "relay MCP channel disabled (ECK_SUB_ROOT_PUBKEY unset)".into(),
    ))?;

    let signed: SignedClientMcp = serde_json::from_value(payload).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("malformed SignedClientMcp: {e}"),
        )
    })?;

    let now = chrono::Utc::now().timestamp();
    let cert = signed
        .admit(
            &root,
            &target_uuid,
            now,
            DEFAULT_MAX_AGE_SECS,
            DEFAULT_GRACE_SECS,
        )
        .map_err(|e| {
            // One line so a probing free client shows up in the relay log, but
            // don't leak which check failed to the caller.
            tracing::warn!(
                "client-mcp dispatch refused: target={} signer={} err={}",
                target_uuid,
                signed.signer_pubkey,
                e
            );
            (StatusCode::FORBIDDEN, "subscription not authorized".into())
        })?;

    // Mid-period revocation (ROADMAP "Revocation list distributed to relays"):
    // the CRL is signed by the LICENSE issuer (ECK_LICENSE_PUBKEY — the
    // relay's product-license trust anchor), deliberately not the sub root —
    // one revocation authority for the whole fleet. Matched on the cert's
    // `subject` label OR its `client_pubkey`. Same opaque 403 as a bad cert.
    if let Ok(lic_pub) = std::env::var("ECK_LICENSE_PUBKEY") {
        if let Some(crl) = eck_core::licensing::load_revocations(&lic_pub) {
            if crl.revokes_cert(&cert.subject, &cert.client_pubkey) {
                tracing::warn!(
                    "client-mcp dispatch refused: cert subject='{}' REVOKED (crl updated {})",
                    cert.subject,
                    crl.updated
                );
                return Err((StatusCode::FORBIDDEN, "subscription not authorized".into()));
            }
        }
    }

    // A per-task read secret handed ONLY to this subscriber (who just proved a
    // valid cert). `result` requires it, so learning a task_id is not enough to
    // read the answer (F3a). 256 bits from two v4 UUIDs.
    let result_token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );

    // Store the whole signed request (cert included) so the NODE re-verifies
    // and derives its own tier — a compromised relay can't forge access or
    // escalate to master by lying about the tier.
    let now_rfc = chrono::Utc::now().to_rfc3339();
    let task_id = uuid::Uuid::new_v4().to_string();
    let stored = serde_json::to_value(&signed).unwrap_or(Value::Null);

    let res = db
        .query(
            "CREATE type::record('client_mcp_task', $tid) SET \
                target_uuid = $tu, \
                payload = $p, \
                result_token = $rt, \
                created_at = $now;",
        )
        .bind(("tid", task_id.clone()))
        .bind(("tu", target_uuid.clone()))
        .bind(("p", stored))
        .bind(("rt", result_token.clone()))
        .bind(("now", now_rfc.clone()))
        .await;

    if let Err(e) = res {
        tracing::error!("client-mcp dispatch insert failed: {e}");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not queue request".into(),
        ));
    }

    tracing::info!(
        "client-mcp queued: task={} target={} plan={} tier={}",
        task_id,
        target_uuid,
        cert.plan,
        if cert.grants_master() { "master" } else { "agent" }
    );
    // Wake a long-poll held for this node so pickup is ~instant.
    task_notify(&target_uuid).notify_waiters();
    Ok(Json(json!({
        "task_id": task_id,
        "queued_at": now_rfc,
        "result_token": result_token,
    })))
}

// ─── GET /E/c/poll/:self_uuid ──────────────────────────────────────────────
// The node pulls pending MCP requests addressed to it. Same shape as the
// xelixir poll; the node re-verifies each `payload` before executing.

/// Claim the node's pending tasks: return every un-acked task that is either
/// never-delivered or stale-delivered (crash re-delivery), and stamp
/// `delivered_at` on each so subsequent polls DON'T see it again while the
/// node executes. This closes the observed double-serve → the replayed run's
/// 403 ack clobbering the good result.
async fn claim_pending(db: &RelayDb, self_uuid: &str) -> Result<Vec<Value>, StatusCode> {
    let stale = (chrono::Utc::now() - chrono::Duration::seconds(REDELIVER_AFTER_SECS)).to_rfc3339();
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, target_uuid, payload, \
                    type::string(created_at) AS created_at \
             FROM client_mcp_task \
             WHERE target_uuid = $tu AND acked = NONE \
               AND (delivered_at = NONE OR delivered_at < $stale) \
             ORDER BY created_at ASC",
        )
        .bind(("tu", self_uuid.to_string()))
        .bind(("stale", stale))
        .await
        .map_err(|e| {
            tracing::error!("client-mcp poll failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    if !rows.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        for row in &rows {
            if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
                if let Err(e) = db
                    .query("UPDATE type::record('client_mcp_task', $tid) SET delivered_at = $now")
                    .bind(("tid", id.to_string()))
                    .bind(("now", now.clone()))
                    .await
                {
                    tracing::warn!("client-mcp delivered_at stamp failed for {id}: {e}");
                }
            }
        }
    }
    Ok(rows)
}

pub async fn poll(
    State(db): State<RelayDb>,
    Path(self_uuid): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    // AuthN: the node proves possession of its server-identity key (F3c).
    authorize_node(&db, &headers, AuthSubject::Poll(&self_uuid), &self_uuid, "poll").await?;

    // LONG-POLL: hold an empty poll so a task dispatched mid-hold is handed
    // over ~instantly (dispatch rings `task_notify`). Total hold stays under
    // the fleet's 15 s poll client timeout.
    let notify = task_notify(&self_uuid);
    let deadline = Instant::now() + Duration::from_secs(LONG_POLL_HOLD_SECS);
    let rows = loop {
        let rows = claim_pending(&db, &self_uuid).await?;
        if !rows.is_empty() {
            break rows;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break rows; // hold expired — return empty
        };
        let wait = remaining.min(Duration::from_secs(LONG_POLL_CHECK_SECS));
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    };

    Ok(Json(json!({
        "tasks": rows,
        "next_poll_in_seconds": POLL_INTERVAL_SECS,
    })))
}

// ─── POST /E/c/ack/:task_id ────────────────────────────────────────────────
// The node posts the MCP result body. Kept so the client can read it.

pub async fn ack(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    // AuthN: the ack must be signed by the key bound to the task's TARGET node,
    // so a forged result for a known task_id is refused (F3b). Look up the
    // target first to know which node identity the signature must match.
    let target: Option<Value> = db
        .query(
            "SELECT target_uuid FROM type::record('client_mcp_task', $tid) LIMIT 1",
        )
        .bind(("tid", task_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("client-mcp ack target lookup failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();
    let target_uuid = target
        .as_ref()
        .and_then(|v| v.get("target_uuid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if target_uuid.is_empty() {
        // Unknown task — nothing to bind against; treat as not found.
        return Err(StatusCode::NOT_FOUND);
    }
    authorize_node(
        &db,
        &headers,
        AuthSubject::Ack(&task_id),
        &target_uuid,
        "ack",
    )
    .await?;

    let result = body.map(|Json(v)| v).unwrap_or(json!({}));
    let now = chrono::Utc::now().to_rfc3339();

    // FIRST ACK WINS: a re-delivered task's second run fails the node's nonce
    // guard and acks a 403 — that must never overwrite the real result the
    // first run already stored.
    let res = db
        .query(
            "UPDATE type::record('client_mcp_task', $tid) SET \
                acked = true, \
                result = $r, \
                acked_at = $now \
             WHERE acked = NONE",
        )
        .bind(("tid", task_id.clone()))
        .bind(("r", result))
        .bind(("now", now))
        .await;

    if let Err(e) = res {
        tracing::error!("client-mcp ack update failed: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(Json(json!({ "ok": true, "task_id": task_id })))
}

// ─── GET /E/c/result/:task_id ──────────────────────────────────────────────
// The subscriber's client polls this until the node acks.

pub async fn result(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let row: Option<Value> = db
        .query(
            "SELECT acked, result, result_token, \
                    (delivered_at != NONE) AS delivered, \
                    type::string(created_at) AS created_at, \
                    type::string(acked_at) AS acked_at \
             FROM type::record('client_mcp_task', $tid) \
             LIMIT 1",
        )
        .bind(("tid", task_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("client-mcp result fetch failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    let row = match row {
        Some(r) => r,
        None => return Err(StatusCode::NOT_FOUND),
    };

    // AuthZ: only the dispatcher holding the per-task read secret may read the
    // result (F3a). Absent/wrong token: reject when auth=required, warn+accept
    // under auth=optional so an old shim (no token) still works during rollout.
    let stored_token = row
        .get("result_token")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !stored_token.is_empty() {
        let ok = provided_result_token(&headers)
            .map(|t| ct_eq(t.as_bytes(), stored_token.as_bytes()))
            .unwrap_or(false);
        if !ok {
            match auth_mode() {
                AuthMode::Required => {
                    tracing::warn!("client-mcp result REJECTED: missing/invalid result_token (task={task_id})");
                    return Err(StatusCode::UNAUTHORIZED);
                }
                AuthMode::Optional => {
                    tracing::warn!(
                        "client-mcp result accepted WITHOUT valid result_token (auth=optional, task={task_id}) — \
                         upgrade the client to send X-Eck-Result-Token, then set ECK_RELAY_CLIENT_MCP_AUTH=required"
                    );
                }
            }
        }
    }

    let acked = row.get("acked").and_then(|v| v.as_bool()).unwrap_or(false);
    if acked {
        let result = row.get("result").cloned().unwrap_or(Value::Null);
        // The node acks both successful MCP results and rejections (re-verify
        // failure, unknown tool). MCP tool errors carry `isError`; transport
        // rejections carry `error`. Treat either as not-success.
        let succeeded = !result.get("error").is_some()
            && !result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        Ok(Json(json!({
            "status": if succeeded { "completed" } else { "failed" },
            "success": succeeded,
            "result": result,
            "created_at": row.get("created_at"),
            "acked_at": row.get("acked_at"),
        })))
    } else {
        // `delivered` lets the waiting client tell a queued-but-busy node (the
        // node polled the task) from an absent one (never picked up) when its
        // ack window elapses — a busy node earns a structured retry, not a
        // generic timeout.
        Ok(Json(json!({
            "status": "pending",
            "delivered": row.get("delivered").and_then(|v| v.as_bool()).unwrap_or(false),
            "created_at": row.get("created_at"),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eck_core::utils::identity::sign_message;

    // A deterministic Ed25519 keypair (base64) for signing tests.
    fn keypair() -> (String, String) {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use ed25519_dalek::SigningKey;
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        (
            STANDARD.encode(sk.to_bytes()),
            STANDARD.encode(sk.verifying_key().to_bytes()),
        )
    }

    #[test]
    fn auth_mode_defaults_optional() {
        std::env::remove_var("ECK_RELAY_CLIENT_MCP_AUTH");
        assert_eq!(auth_mode(), AuthMode::Optional);
        std::env::set_var("ECK_RELAY_CLIENT_MCP_AUTH", "required");
        assert_eq!(auth_mode(), AuthMode::Required);
        std::env::set_var("ECK_RELAY_CLIENT_MCP_AUTH", "optional");
        assert_eq!(auth_mode(), AuthMode::Optional);
        std::env::remove_var("ECK_RELAY_CLIENT_MCP_AUTH");
    }

    #[test]
    fn ct_eq_matches_only_equal() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"x"));
    }

    #[test]
    fn parse_node_auth_needs_all_three() {
        let mut h = HeaderMap::new();
        assert!(parse_node_auth(&h).is_none());
        h.insert("x-eck-node-key", "k".parse().unwrap());
        h.insert("x-eck-node-ts", "100".parse().unwrap());
        assert!(parse_node_auth(&h).is_none()); // no sig
        h.insert("x-eck-node-sig", "s".parse().unwrap());
        let a = parse_node_auth(&h).expect("all present");
        assert_eq!(a.key, "k");
        assert_eq!(a.ts, 100);
        assert_eq!(a.sig, "s");
    }

    #[test]
    fn node_sig_valid_roundtrip_poll() {
        let (priv_b64, pub_b64) = keypair();
        let ts = 1_000_000i64;
        let canonical = poll_canonical("node-42", ts);
        let sig = sign_message(&priv_b64, &canonical).unwrap();
        let auth = NodeAuth {
            key: pub_b64,
            ts,
            sig,
        };
        // Fresh + correct canonical → valid.
        assert!(node_sig_valid(&auth, &canonical, ts + 5, NODE_AUTH_MAX_AGE_SECS));
        // Stale timestamp → rejected.
        assert!(!node_sig_valid(
            &auth,
            &canonical,
            ts + NODE_AUTH_MAX_AGE_SECS + 1,
            NODE_AUTH_MAX_AGE_SECS
        ));
        // Wrong canonical (different node / replayed for another target) → rejected.
        let other = poll_canonical("node-99", ts);
        assert!(!node_sig_valid(&auth, &other, ts + 5, NODE_AUTH_MAX_AGE_SECS));
    }

    #[test]
    fn node_sig_valid_rejects_forged() {
        // Attacker signs the right canonical with the WRONG key; presents the
        // legit node's pubkey → signature verification fails.
        use base64::{engine::general_purpose::STANDARD, Engine};
        use ed25519_dalek::SigningKey;
        let legit_pub = STANDARD.encode(SigningKey::from_bytes(&[1u8; 32]).verifying_key().to_bytes());
        let attacker_priv = STANDARD.encode(SigningKey::from_bytes(&[2u8; 32]).to_bytes());
        let ts = 500_000i64;
        let canonical = ack_canonical("task-abc", ts);
        let sig = sign_message(&attacker_priv, &canonical).unwrap();
        let auth = NodeAuth {
            key: legit_pub,
            ts,
            sig,
        };
        assert!(!node_sig_valid(&auth, &canonical, ts, NODE_AUTH_MAX_AGE_SECS));
    }

    #[test]
    fn registry_binding_precedence() {
        // Registry has a key → it wins: exact match allows, any difference is a
        // hard mismatch (the caller turns this into 401, never TOFU).
        assert_eq!(classify_registry(Some("KEYX"), "KEYX"), RegistryBinding::Match);
        assert_eq!(classify_registry(Some("KEYX"), "KEYY"), RegistryBinding::Mismatch);
        // No registry key (empty or absent) → fall back to TOFU.
        assert_eq!(classify_registry(Some(""), "KEYX"), RegistryBinding::NoKey);
        assert_eq!(classify_registry(None, "KEYX"), RegistryBinding::NoKey);
    }

    #[test]
    fn tofu_fallback_logs_once_per_node() {
        // First observation of a node returns false (not-yet-logged → do log);
        // subsequent observations return true (already logged → suppress).
        let node = "unit-test-node-abc";
        assert!(!tofu_fallback_logged(node));
        assert!(tofu_fallback_logged(node));
        assert!(tofu_fallback_logged(node));
    }

    #[test]
    fn ack_canonical_binds_task_id() {
        assert_ne!(ack_canonical("t1", 1), ack_canonical("t2", 1));
        assert_ne!(ack_canonical("t1", 1), ack_canonical("t1", 2));
        assert_ne!(poll_canonical("t1", 1), ack_canonical("t1", 1));
    }
}
