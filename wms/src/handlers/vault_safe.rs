//! Vault-safe HTTP surface — the share-holder side of P0.5
//! (`.eck/PII_VAULT.md` §2/§6-P0.5). Runs ONLY on the safe nodes
//! (`eck1/2/3`) where `ECK_VAULT_SERVE=1`; every route 404s otherwise.
//!
//! # What a "safe" holds
//! A tenant's Tenant Master Key (TMK, and its legacy epoch-0 pepper/MDK) lives
//! sealed in an AEAD blob on each home/operator node, keyed by that node's
//! node-local KEK — **9eck never holds a key that opens data**. What 9eck's
//! safe nodes hold is ONE Shamir share (`k=2, n=3`) per node per kind. A single
//! safe therefore cannot reconstruct anything: a booting node fetches ≥2 shares
//! (one per safe), combines, and opens with its own KEK.
//!
//! Hard invariants:
//! * Mounted only when `ECK_VAULT_SERVE=1`; every handler re-checks and returns
//!   404 `{"error":"vault safe not served here"}` otherwise (belt-and-braces —
//!   the routes are registered unconditionally so `/health` can report state).
//! * AUTH = the tenant's paid mesh license in `Authorization: Bearer`, verified
//!   on the SAME seam as the OpenRouter proxy
//!   (`handlers::or_proxy::verify_paid_license`); `claims.sub` == tenant. The
//!   `/health` probe is the one unauthenticated route.
//! * Every WRITE (PUT share / node register / node revoke) that mutates state is
//!   authenticated a SECOND time by an Ed25519 signature from a registered node
//!   key over the [`canonical_msg`] string (300 s timestamp window).
//! * Tables `vault_share` / `vault_node` are node-LOCAL (NOT in
//!   `SYNC_ENTITY_TYPES`) — a share must never mesh-sync to another safe.
//! * Every write and every 403/404 is audited under
//!   [`eck_core::audit::class::VAULT`]. No key/secret material is ever logged.

use crate::AppState;
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

// ── config gates ─────────────────────────────────────────────────────────────

/// `true` only on a node explicitly configured to serve the vault safe.
fn served() -> bool {
    std::env::var("ECK_VAULT_SERVE").map(|v| v == "1").unwrap_or(false)
}

/// The single not-served rejection body, pinned by the contract.
fn not_served() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error":"vault safe not served here"}))).into_response()
}

/// Timestamp window for signed writes (seconds). A request whose `ts` is more
/// than this far from wall-clock is rejected (replay / clock-skew guard).
const TS_WINDOW_SECS: i64 = 300;

// ── canonical signed message ─────────────────────────────────────────────────

/// The byte-exact message an Ed25519 signature covers for every vault WRITE
/// (PUT share and POST node). **This is the single source of truth for the wire
/// signature** — the client side (admin/join/recover in `vault_admin.rs`) MUST
/// build the identical string, so it imports THIS function rather than
/// re-formatting it.
///
/// ```text
/// 9eck-vault-v1\n{tenant}\n{action}\n{node_id}\n{kind}\n{share_index}\n{version}\n{ts}
/// ```
///
/// `action` is `"put_share"` for `PUT /api/vault/share`, and the node action
/// (`"register"` / `"revoke"`) for `POST /api/vault/node`. Fields that do not
/// apply to the action are the empty string (node actions carry no
/// kind/share_index/version).
pub fn canonical_msg(
    tenant: &str,
    action: &str,
    node_id: &str,
    kind: &str,
    share_index: &str,
    version: &str,
    ts: i64,
) -> String {
    format!("9eck-vault-v1\n{tenant}\n{action}\n{node_id}\n{kind}\n{share_index}\n{version}\n{ts}")
}

/// `action` value signed for a `PUT /api/vault/share`.
const ACTION_PUT_SHARE: &str = "put_share";
/// `action` value that arms an already-active node to adopt a re-sealed blob
/// after a key rotation (P0.7). Mirrors `eck_core::vault::client::ACTION_REKEY`.
const ACTION_REKEY: &str = "rekey";

// ── constant-time compare (no `subtle` dependency) ───────────────────────────

/// Length-checked, data-independent byte compare for proof matching. The length
/// itself is not secret (proofs are fixed-width hex SHA-256), only the value is.
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

/// `true` iff `ts` is within [`TS_WINDOW_SECS`] of `now` (both unix seconds).
fn ts_in_window(now: i64, ts: i64) -> bool {
    (now - ts).abs() <= TS_WINDOW_SECS
}

// ── record ids (node-local, deterministic) ───────────────────────────────────

fn hex32(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))[..32].to_string()
}

/// Full SHA-256 hex of a string. Used to store a proof preimage as its hash at
/// rest: the row holds `sha256(proof)`, so a DB read yields no usable capability
/// (the client always presents the preimage, which the server re-hashes).
fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Deterministic id for a `vault_share` row: one per (tenant, node_id, kind).
fn share_row_id(tenant: &str, node_id: &str, kind: &str) -> String {
    hex32(&format!("{tenant}|{node_id}|{kind}"))
}

/// Deterministic id for a `vault_node` row: one per (tenant, node_id).
fn node_row_id(tenant: &str, node_id: &str) -> String {
    hex32(&format!("{tenant}|{node_id}"))
}

// ── per-tenant rate limiter (60 req / 60 s, fixed window, in-memory) ─────────

const RATE_LIMIT: u32 = 60;
const RATE_WINDOW_SECS: u64 = 60;

struct RateState {
    window_start: Instant,
    count: u32,
}

fn rate_map() -> &'static Mutex<HashMap<String, RateState>> {
    static MAP: OnceLock<Mutex<HashMap<String, RateState>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record one request against `tenant`; returns `false` when the tenant is over
/// [`RATE_LIMIT`] in the current fixed window (caller should 429).
fn rate_ok(tenant: &str) -> bool {
    let now = Instant::now();
    let mut map = rate_map().lock().unwrap();
    let entry = map.entry(tenant.to_string()).or_insert(RateState {
        window_start: now,
        count: 0,
    });
    if now.duration_since(entry.window_start).as_secs() >= RATE_WINDOW_SECS {
        entry.window_start = now;
        entry.count = 0;
    }
    entry.count += 1;
    entry.count <= RATE_LIMIT
}

// ── small response helpers ───────────────────────────────────────────────────

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn ok(val: Value) -> Response {
    (StatusCode::OK, Json(val)).into_response()
}

/// Lift the paid mesh license out of `Authorization: Bearer` and verify it on
/// the shared OpenRouter seam. `Ok(tenant)` is the license `sub`.
fn tenant_from_headers(headers: &HeaderMap) -> Result<String, Response> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    match crate::handlers::or_proxy::verify_paid_license(&token) {
        Ok(claims) => Ok(claims.sub),
        Err((code, msg)) => Err(err(code, msg)),
    }
}

/// Fire-and-forget audit under the VAULT class. `actor` is the tenant.
async fn audit(state: &Arc<AppState>, tenant: &str, action: &str, summary: &str, payload: Value) {
    eck_core::audit::append_soft(
        &state.db,
        &state.server_identity,
        &eck_core::audit::wms_chain(&state.instance_id),
        tenant,
        action,
        eck_core::audit::class::VAULT,
        summary,
        payload,
    )
    .await;
}

// ── DB helpers ───────────────────────────────────────────────────────────────

/// Load a single `vault_node` row (as JSON `Value`) for (tenant, node_id).
async fn load_node(state: &Arc<AppState>, tenant: &str, node_id: &str) -> Result<Option<Value>, String> {
    let id = node_row_id(tenant, node_id);
    state
        .db
        .query("SELECT * FROM type::record('vault_node', $id)")
        .bind(("id", id))
        .await
        .map_err(|e| e.to_string())?
        .take::<Option<Value>>(0)
        .map_err(|e| e.to_string())
}

/// Load a single `vault_share` row (as JSON `Value`) for (tenant, node_id, kind).
async fn load_share(
    state: &Arc<AppState>,
    tenant: &str,
    node_id: &str,
    kind: &str,
) -> Result<Option<Value>, String> {
    let id = share_row_id(tenant, node_id, kind);
    state
        .db
        .query("SELECT * FROM type::record('vault_share', $id)")
        .bind(("id", id))
        .await
        .map_err(|e| e.to_string())?
        .take::<Option<Value>>(0)
        .map_err(|e| e.to_string())
}

/// Does the tenant have ANY vault_node row? (drives the bootstrap branch)
async fn tenant_has_nodes(state: &Arc<AppState>, tenant: &str) -> Result<bool, String> {
    let rows: Vec<Value> = state
        .db
        .query("SELECT id FROM vault_node WHERE tenant = $t LIMIT 1")
        .bind(("t", tenant.to_string()))
        .await
        .map_err(|e| e.to_string())?
        .take(0)
        .map_err(|e| e.to_string())?;
    Ok(!rows.is_empty())
}

fn field_str<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(|v| v.as_str())
}

/// UPSERT a `vault_share` row. `expires_at` is stored as unix-seconds i64 (this
/// table is node-local and both reader & writer are this module, so a plain
/// integer is used in place of a SurrealDB datetime — see report).
async fn upsert_share(
    state: &Arc<AppState>,
    tenant: &str,
    node_id: &str,
    kind: &str,
    share_index: u8,
    share: &str,
    blob_len: u32,
    version: u32,
    expires_at: Option<i64>,
) -> Result<(), String> {
    let id = share_row_id(tenant, node_id, kind);
    state
        .db
        .query(
            "UPSERT type::record('vault_share', $id) SET \
             tenant = $t, node_id = $n, kind = $k, share_index = $si, share = $s, \
             blob_len = $bl, version = $v, expires_at = $exp, \
             created_at = created_at ?? time::now(), updated_at = time::now()",
        )
        .bind(("id", id))
        .bind(("t", tenant.to_string()))
        .bind(("n", node_id.to_string()))
        .bind(("k", kind.to_string()))
        .bind(("si", share_index))
        .bind(("s", share.to_string()))
        .bind(("bl", blob_len))
        .bind(("v", version))
        .bind(("exp", expires_at))
        .await
        .and_then(|r| r.check())
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn delete_share(state: &Arc<AppState>, tenant: &str, node_id: &str, kind: &str) -> Result<(), String> {
    let id = share_row_id(tenant, node_id, kind);
    state
        .db
        .query("DELETE type::record('vault_share', $id)")
        .bind(("id", id))
        .await
        .and_then(|r| r.check())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Create/update a `vault_node` row. Only the provided fields are set; the
/// query uses IF/ELSE-free plain SET on an explicit id (no WHERE clause, so the
/// v3 greedy-SET-before-WHERE gotcha does not apply).
#[allow(clippy::too_many_arguments)]
async fn upsert_node(
    state: &Arc<AppState>,
    tenant: &str,
    node_id: &str,
    role: &str,
    status: &str,
    pubkey: Option<&str>,
    signed_by: &str,
    recovery_proof: Option<&str>,
    enroll_proof: Option<&str>,
    revoked: bool,
) -> Result<(), String> {
    let id = node_row_id(tenant, node_id);
    // `?? role` / `?? pubkey`-style coalescing keeps an existing value when the
    // caller passes None (e.g. a re-register must not blank a bound pubkey).
    state
        .db
        .query(
            "UPSERT type::record('vault_node', $id) SET \
             tenant = $t, node_id = $n, role = $role, status = $status, \
             pubkey = ($pk ?? pubkey), signed_by = $sb, \
             recovery_proof = ($rp ?? recovery_proof), \
             enroll_proof = ($ep ?? enroll_proof), \
             revoked_at = (IF $revoked THEN time::now() ELSE revoked_at END), \
             created_at = created_at ?? time::now(), updated_at = time::now()",
        )
        .bind(("id", id))
        .bind(("t", tenant.to_string()))
        .bind(("n", node_id.to_string()))
        .bind(("role", role.to_string()))
        .bind(("status", status.to_string()))
        .bind(("pk", pubkey.map(|s| s.to_string())))
        .bind(("sb", signed_by.to_string()))
        .bind(("rp", recovery_proof.map(|s| s.to_string())))
        .bind(("ep", enroll_proof.map(|s| s.to_string())))
        .bind(("revoked", revoked))
        .await
        .and_then(|r| r.check())
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ── request bodies (field names are the frozen wire contract, §2.1) ──────────

#[derive(Deserialize)]
pub struct FetchShareRequest {
    pub node_id: String,
    pub kind: String,
    /// Client boot cache-buster; accepted for wire compatibility, unused server-side.
    #[serde(default)]
    #[allow(dead_code)]
    pub boot_nonce: Option<String>,
    #[serde(default)]
    pub proof: Option<String>,
}

#[derive(Deserialize)]
pub struct PutShareRequest {
    pub node_id: String,
    pub kind: String,
    pub share_index: u8,
    pub share: String,
    pub blob_len: u32,
    pub version: u32,
    #[serde(default)]
    pub expires_in_secs: Option<u64>,
    pub signer_node_id: String,
    #[serde(default)]
    pub signer_pubkey: Option<String>,
    pub ts: i64,
    pub sig: String,
    /// Requested role; accepted for wire compatibility. Bootstrap/recovery force
    /// "home"; enroll-completion keeps the pending row's role — so unused here.
    #[serde(default)]
    #[allow(dead_code)]
    pub role: Option<String>,
    #[serde(default)]
    pub recovery_proof: Option<String>,
    /// Enroll-completion only: the enroll proof preimage the newcomer presents to
    /// prove code possession; compared (SHA-256) against the pending row's stored
    /// hash before its pubkey is bound.
    #[serde(default)]
    pub enroll_proof: Option<String>,
}

#[derive(Deserialize)]
pub struct NodeActionRequest {
    pub node_id: String,
    pub action: String,
    #[serde(default)]
    pub role: Option<String>,
    pub signer_node_id: String,
    pub ts: i64,
    pub sig: String,
    /// `register` only: the enroll proof preimage; stored as its SHA-256 hash on
    /// the pending row (see [`sha256_hex`]).
    #[serde(default)]
    pub enroll_proof: Option<String>,
}

fn kind_valid(kind: &str) -> bool {
    matches!(kind, "keys" | "recovery" | "enroll")
}

// ── POST /api/vault/share — fetch one share ──────────────────────────────────

pub async fn fetch_share(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<FetchShareRequest>,
) -> Response {
    if !served() {
        return not_served();
    }
    let tenant = match tenant_from_headers(&headers) {
        Ok(t) => t,
        Err(r) => return r,
    };
    if !rate_ok(&tenant) {
        audit(&state, &tenant, "share.fetch", "vault share fetch rate-limited", json!({"code":429})).await;
        return err(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
    }
    if !kind_valid(&body.kind) {
        audit(&state, &tenant, "share.fetch", "vault share fetch bad kind", json!({"code":404,"kind":body.kind})).await;
        return not_served();
    }

    // Authorisation to receive the share, per kind.
    match body.kind.as_str() {
        "keys" => {
            let node = match load_node(&state, &tenant, &body.node_id).await {
                Ok(n) => n,
                Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
            };
            let active = node.as_ref().and_then(|n| field_str(n, "status")) == Some("active");
            if !active {
                audit(&state, &tenant, "share.fetch",
                    "vault keys-share fetch denied (node not active)",
                    json!({"code":404,"node_id":body.node_id,"kind":"keys"})).await;
                return not_served();
            }
        }
        "enroll" => {
            // Require the enroll proof preimage matching the pending row's stored
            // hash — proves the caller holds the one-time enroll code.
            let node = match load_node(&state, &tenant, &body.node_id).await {
                Ok(n) => n,
                Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
            };
            let stored = node.as_ref().and_then(|n| field_str(n, "enroll_proof"));
            let presented = body.proof.clone().unwrap_or_default();
            let proof_ok = !presented.is_empty()
                && stored.is_some_and(|s| ct_eq(sha256_hex(&presented).as_bytes(), s.as_bytes()));
            if !proof_ok {
                audit(&state, &tenant, "share.fetch",
                    "vault enroll-share fetch denied (bad proof)",
                    json!({"code":404,"node_id":body.node_id,"kind":"enroll"})).await;
                return not_served();
            }
            let share = match load_share(&state, &tenant, &body.node_id, "enroll").await {
                Ok(s) => s,
                Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
            };
            let now = chrono::Utc::now().timestamp();
            let fresh = share.as_ref().is_some_and(|s| {
                s.get("expires_at").and_then(|v| v.as_i64()).map(|exp| exp > now).unwrap_or(false)
            });
            if !fresh {
                audit(&state, &tenant, "share.fetch",
                    "vault enroll-share fetch denied (missing/expired)",
                    json!({"code":404,"node_id":body.node_id,"kind":"enroll"})).await;
                return not_served();
            }
        }
        "recovery" => {
            let proof = body.proof.clone().unwrap_or_default();
            if proof.is_empty() || !recovery_proof_matches(&state, &tenant, &proof).await {
                audit(&state, &tenant, "share.fetch",
                    "vault recovery-share fetch denied (bad proof)",
                    json!({"code":404,"node_id":body.node_id,"kind":"recovery"})).await;
                return not_served();
            }
        }
        _ => return not_served(),
    }

    // Serve the stored share.
    let share = match load_share(&state, &tenant, &body.node_id, &body.kind).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            audit(&state, &tenant, "share.fetch", "vault share fetch not found",
                json!({"code":404,"node_id":body.node_id,"kind":body.kind})).await;
            return not_served();
        }
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
    };
    audit(&state, &tenant, "share.fetch", "vault share served",
        json!({"node_id":body.node_id,"kind":body.kind})).await;
    ok(json!({
        "share_index": share.get("share_index").and_then(|v| v.as_u64()).unwrap_or(0),
        "share": field_str(&share, "share").unwrap_or_default(),
        "blob_len": share.get("blob_len").and_then(|v| v.as_u64()).unwrap_or(0),
        "version": share.get("version").and_then(|v| v.as_u64()).unwrap_or(0),
    }))
}

/// `true` iff `proof` equals (constant-time) the stored `recovery_proof` of ANY
/// of the tenant's home rows.
async fn recovery_proof_matches(state: &Arc<AppState>, tenant: &str, proof: &str) -> bool {
    let rows: Vec<Value> = match state
        .db
        .query("SELECT recovery_proof FROM vault_node WHERE tenant = $t AND role = 'home' AND recovery_proof != NONE")
        .bind(("t", tenant.to_string()))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(r) => r,
        Err(_) => return false,
    };
    // The row stores sha256(preimage); the client presents the preimage. Hash it
    // and compare constant-time, so a DB read never yields a usable capability.
    let presented_hash = sha256_hex(proof);
    rows.iter()
        .filter_map(|r| field_str(r, "recovery_proof"))
        .any(|stored| ct_eq(stored.as_bytes(), presented_hash.as_bytes()))
}

// ── PUT /api/vault/share — store one share (signed) ──────────────────────────

pub async fn put_share(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<PutShareRequest>,
) -> Response {
    if !served() {
        return not_served();
    }
    let tenant = match tenant_from_headers(&headers) {
        Ok(t) => t,
        Err(r) => return r,
    };
    if !rate_ok(&tenant) {
        audit(&state, &tenant, "share.put", "vault share put rate-limited", json!({"code":429})).await;
        return err(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
    }
    let now = chrono::Utc::now().timestamp();
    if !ts_in_window(now, body.ts) {
        audit(&state, &tenant, "share.put", "vault share put stale timestamp",
            json!({"code":403,"ts":body.ts,"now":now})).await;
        return err(StatusCode::FORBIDDEN, "timestamp outside window");
    }
    if !kind_valid(&body.kind) {
        return err(StatusCode::BAD_REQUEST, "invalid kind");
    }

    let msg = canonical_msg(
        &tenant,
        ACTION_PUT_SHARE,
        &body.node_id,
        &body.kind,
        &body.share_index.to_string(),
        &body.version.to_string(),
        body.ts,
    );

    let has_nodes = match tenant_has_nodes(&state, &tenant).await {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
    };

    // Load the target node's current row (may be None).
    let node = match load_node(&state, &tenant, &body.node_id).await {
        Ok(n) => n,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
    };
    let node_status = node.as_ref().and_then(|n| field_str(n, "status"));

    // Determine the form (contract §2.1 order).
    if !has_nodes {
        // ── Bootstrap: first node of a brand-new tenant. ──
        if body.kind != "keys" || body.signer_node_id != body.node_id {
            return deny_put(&state, &tenant, &body, "bootstrap requires kind=keys and signer==node").await;
        }
        let Some(pk) = body.signer_pubkey.as_deref() else {
            return deny_put(&state, &tenant, &body, "bootstrap requires signer_pubkey").await;
        };
        if !sig_ok(pk, &msg, &body.sig) {
            return deny_put(&state, &tenant, &body, "bootstrap signature invalid").await;
        }
        let rp_hash = body.recovery_proof.as_deref().map(sha256_hex);
        if let Err(e) = upsert_node(&state, &tenant, &body.node_id, "home", "active",
            Some(pk), &body.signer_node_id, rp_hash.as_deref(), None, false).await
        {
            return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}"));
        }
        return finish_put(&state, &tenant, &body, "bootstrap").await;
    }

    if node_status == Some("pending") {
        // ── Enroll completion: bind the newcomer's pubkey, activate. ──
        if body.kind != "keys" || body.signer_node_id != body.node_id {
            return deny_put(&state, &tenant, &body, "enroll-completion requires kind=keys and signer==node").await;
        }
        let Some(pk) = body.signer_pubkey.as_deref() else {
            return deny_put(&state, &tenant, &body, "enroll-completion requires signer_pubkey").await;
        };
        if !sig_ok(pk, &msg, &body.sig) {
            return deny_put(&state, &tenant, &body, "enroll-completion signature invalid").await;
        }
        // Prove possession of the one-time enroll code: the presented proof
        // preimage must hash to the stored enroll_proof on the pending row. A
        // self-signature alone must NOT be able to bind a pubkey to this slot.
        let stored_ep = node.as_ref().and_then(|n| field_str(n, "enroll_proof"));
        let presented_ep = body.enroll_proof.clone().unwrap_or_default();
        let ep_ok = !presented_ep.is_empty()
            && stored_ep.is_some_and(|s| ct_eq(sha256_hex(&presented_ep).as_bytes(), s.as_bytes()));
        if !ep_ok {
            return deny_put(&state, &tenant, &body, "enroll-completion proof invalid").await;
        }
        if let Err(e) = upsert_node(&state, &tenant, &body.node_id,
            node.as_ref().and_then(|n| field_str(n, "role")).unwrap_or("operator"),
            "active", Some(pk), &body.signer_node_id, None, None, false).await
        {
            return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}"));
        }
        // Store the keys share, then drop the now-consumed enroll share.
        if let Err(e) = store_put_share(&state, &tenant, &body, now).await {
            return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}"));
        }
        let _ = delete_share(&state, &tenant, &body.node_id, "enroll").await;
        audit(&state, &tenant, "share.put", "vault enroll-completion stored",
            json!({"node_id":body.node_id,"kind":body.kind})).await;
        return ok(json!({"ok": true, "form": "enroll-completion"}));
    }

    if let Some(rp) = body.recovery_proof.as_deref() {
        if !rp.is_empty() && recovery_proof_matches(&state, &tenant, rp).await {
            // ── Recovery re-key: a NEW home node re-seals under its own KEK. ──
            let Some(pk) = body.signer_pubkey.as_deref() else {
                return deny_put(&state, &tenant, &body, "recovery requires signer_pubkey").await;
            };
            if body.signer_node_id != body.node_id || !sig_ok(pk, &msg, &body.sig) {
                return deny_put(&state, &tenant, &body, "recovery signature invalid").await;
            }
            if let Err(e) = upsert_node(&state, &tenant, &body.node_id, "home", "active",
                Some(pk), &body.signer_node_id, Some(&sha256_hex(rp)), None, false).await
            {
                return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}"));
            }
            return finish_put(&state, &tenant, &body, "recovery").await;
        }
    }

    // ── Otherwise: an existing active home/operator writes a share. ──
    let signer = match load_node(&state, &tenant, &body.signer_node_id).await {
        Ok(s) => s,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
    };
    let Some(signer) = signer else {
        return deny_put(&state, &tenant, &body, "signer is not a registered node").await;
    };
    let signer_active = field_str(&signer, "status") == Some("active");
    let signer_role_ok = matches!(field_str(&signer, "role"), Some("home") | Some("operator"));
    let Some(signer_pk) = field_str(&signer, "pubkey") else {
        return deny_put(&state, &tenant, &body, "signer has no bound pubkey").await;
    };
    if !signer_active || !signer_role_ok {
        return deny_put(&state, &tenant, &body, "signer is not an active home/operator").await;
    }
    if !sig_ok(signer_pk, &msg, &body.sig) {
        return deny_put(&state, &tenant, &body, "signature invalid").await;
    }
    match store_put_share(&state, &tenant, &body, now).await {
        Ok(()) => {
            audit(&state, &tenant, "share.put", "vault share stored (delegated)",
                json!({"node_id":body.node_id,"kind":body.kind})).await;
            ok(json!({"ok": true, "form": "delegated"}))
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
    }
}

/// Store the share carried by a PUT, computing `expires_at` for enroll shares.
async fn store_put_share(state: &Arc<AppState>, tenant: &str, body: &PutShareRequest, now: i64) -> Result<(), String> {
    let expires_at = if body.kind == "enroll" {
        let secs = body.expires_in_secs.unwrap_or(900).clamp(60, 3600) as i64;
        Some(now + secs)
    } else {
        None
    };
    upsert_share(state, tenant, &body.node_id, &body.kind, body.share_index,
        &body.share, body.blob_len, body.version, expires_at).await
}

/// Bootstrap/recovery tail: both create/activate the node row (done by the
/// caller) then store the keys share.
async fn finish_put(state: &Arc<AppState>, tenant: &str, body: &PutShareRequest, form: &str) -> Response {
    let now = chrono::Utc::now().timestamp();
    if let Err(e) = store_put_share(state, tenant, body, now).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}"));
    }
    audit(state, tenant, "share.put", &format!("vault {form} share stored"),
        json!({"node_id":body.node_id,"kind":body.kind,"form":form})).await;
    ok(json!({"ok": true, "form": form}))
}

async fn deny_put(state: &Arc<AppState>, tenant: &str, body: &PutShareRequest, why: &str) -> Response {
    audit(state, tenant, "share.put", &format!("vault share put denied: {why}"),
        json!({"code":403,"node_id":body.node_id,"kind":body.kind,"why":why})).await;
    err(StatusCode::FORBIDDEN, why)
}

fn sig_ok(pubkey_b64: &str, msg: &str, sig_b64: &str) -> bool {
    eck_core::utils::identity::verify_signature(pubkey_b64, msg, sig_b64).unwrap_or(false)
}

// ── POST /api/vault/node — register / revoke (signed) ────────────────────────

pub async fn node_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<NodeActionRequest>,
) -> Response {
    if !served() {
        return not_served();
    }
    let tenant = match tenant_from_headers(&headers) {
        Ok(t) => t,
        Err(r) => return r,
    };
    if !rate_ok(&tenant) {
        audit(&state, &tenant, "node.action", "vault node action rate-limited", json!({"code":429})).await;
        return err(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
    }
    let now = chrono::Utc::now().timestamp();
    if !ts_in_window(now, body.ts) {
        audit(&state, &tenant, "node.action", "vault node action stale timestamp",
            json!({"code":403,"ts":body.ts})).await;
        return err(StatusCode::FORBIDDEN, "timestamp outside window");
    }
    if body.action != "register" && body.action != "revoke" && body.action != ACTION_REKEY {
        return err(StatusCode::BAD_REQUEST, "invalid action");
    }

    let msg = canonical_msg(&tenant, &body.action, &body.node_id, "", "", "", body.ts);

    // The signer must be a registered node with a bound pubkey.
    let signer = match load_node(&state, &tenant, &body.signer_node_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return deny_node(&state, &tenant, &body, "signer is not a registered node").await,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
    };
    let Some(signer_pk) = field_str(&signer, "pubkey") else {
        return deny_node(&state, &tenant, &body, "signer has no bound pubkey").await;
    };
    if !sig_ok(signer_pk, &msg, &body.sig) {
        return deny_node(&state, &tenant, &body, "signature invalid").await;
    }

    // Authorisation: an active home/operator may act; a node may always revoke
    // itself (self-revoke) even if not an active home/operator.
    let signer_active = field_str(&signer, "status") == Some("active");
    let signer_role_ok = matches!(field_str(&signer, "role"), Some("home") | Some("operator"));
    let self_revoke = body.action == "revoke" && body.signer_node_id == body.node_id;
    if !((signer_active && signer_role_ok) || self_revoke) {
        return deny_node(&state, &tenant, &body, "signer not authorised").await;
    }

    if body.action == ACTION_REKEY {
        // ── Re-key (P0.7 rotation): arm an ALREADY-ACTIVE node to pick up a new
        // blob. The home node has rotated the tenant keys and re-sealed the new
        // payload under a one-time code for this node; it needs the matching
        // `enroll_proof` on the row so the node may fetch that enroll share.
        //
        // Unlike `register` this keeps status/role/pubkey exactly as they are:
        // flipping an active operator to `pending` would cut it off from its own
        // keys share the moment it restarted, and a failed re-key would strand
        // it. Only an ACTIVE HOME may arm a re-key, and only for a node that is
        // already active — this can neither create a node nor resurrect a
        // revoked one (which is the whole point of revocation).
        if field_str(&signer, "role") != Some("home") {
            return deny_node(&state, &tenant, &body, "only an active home may re-key a node").await;
        }
        let target = match load_node(&state, &tenant, &body.node_id).await {
            Ok(Some(n)) => n,
            Ok(None) => return deny_node(&state, &tenant, &body, "re-key target is not registered").await,
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
        };
        if field_str(&target, "status") != Some("active") {
            return deny_node(&state, &tenant, &body, "re-key target is not active").await;
        }
        let Some(ep) = body.enroll_proof.as_deref().filter(|s| !s.is_empty()) else {
            return err(StatusCode::BAD_REQUEST, "re-key requires enroll_proof");
        };
        let role = field_str(&target, "role").unwrap_or("operator").to_string();
        if let Err(e) = upsert_node(&state, &tenant, &body.node_id, &role, "active",
            None, &body.signer_node_id, None, Some(&sha256_hex(ep)), false).await
        {
            return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}"));
        }
        audit(&state, &tenant, "node.action", "vault node armed for re-key",
            json!({"node_id":body.node_id,"role":role})).await;
        return ok(json!({"ok": true, "action": ACTION_REKEY, "status": "active"}));
    }

    if body.action == "register" {
        let role = body.role.as_deref().unwrap_or("operator");
        if role != "home" && role != "operator" && role != "cache" {
            return err(StatusCode::BAD_REQUEST, "invalid role");
        }
        // Store the enroll proof as its SHA-256 hash so the newcomer must present
        // the preimage (derived from the enroll code) to complete the enrolment.
        let ep_hash = body.enroll_proof.as_deref().map(sha256_hex);
        if let Err(e) = upsert_node(&state, &tenant, &body.node_id, role, "pending",
            None, &body.signer_node_id, None, ep_hash.as_deref(), false).await
        {
            return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}"));
        }
        audit(&state, &tenant, "node.action", "vault node registered (pending)",
            json!({"node_id":body.node_id,"role":role})).await;
        ok(json!({"ok": true, "action": "register", "status": "pending"}))
    } else {
        // revoke: keep the existing role, flip status, drop the keys share here.
        let existing_role = load_node(&state, &tenant, &body.node_id)
            .await
            .ok()
            .flatten()
            .and_then(|n| field_str(&n, "role").map(|s| s.to_string()))
            .unwrap_or_else(|| "operator".to_string());
        if let Err(e) = upsert_node(&state, &tenant, &body.node_id, &existing_role, "revoked",
            None, &body.signer_node_id, None, None, true).await
        {
            return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}"));
        }
        let _ = delete_share(&state, &tenant, &body.node_id, "keys").await;
        audit(&state, &tenant, "node.action", "vault node revoked",
            json!({"node_id":body.node_id})).await;
        ok(json!({"ok": true, "action": "revoke", "status": "revoked"}))
    }
}

async fn deny_node(state: &Arc<AppState>, tenant: &str, body: &NodeActionRequest, why: &str) -> Response {
    audit(state, tenant, "node.action", &format!("vault node action denied: {why}"),
        json!({"code":403,"node_id":body.node_id,"action":body.action,"why":why})).await;
    err(StatusCode::FORBIDDEN, why)
}

// ── GET /api/vault/nodes — public roster (never shares, never proofs) ────────

pub async fn list_nodes(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !served() {
        return not_served();
    }
    let tenant = match tenant_from_headers(&headers) {
        Ok(t) => t,
        Err(r) => return r,
    };
    if !rate_ok(&tenant) {
        return err(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
    }
    let rows: Vec<Value> = match state
        .db
        .query("SELECT node_id, role, status, pubkey, created_at, revoked_at FROM vault_node WHERE tenant = $t")
        .bind(("t", tenant.to_string()))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db: {e}")),
    };
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "node_id": field_str(r, "node_id").unwrap_or_default(),
                "role": field_str(r, "role").unwrap_or_default(),
                "status": field_str(r, "status").unwrap_or_default(),
                "has_pubkey": r.get("pubkey").map(|v| !v.is_null()).unwrap_or(false),
                "created_at": r.get("created_at").cloned().unwrap_or(Value::Null),
                "revoked_at": r.get("revoked_at").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    ok(Value::Array(out))
}

// ── GET /api/vault/health — unauthenticated deploy probe ─────────────────────

pub async fn health(State(state): State<Arc<AppState>>) -> Response {
    let shares: i64 = state
        .db
        .query("SELECT count() AS c FROM vault_share GROUP ALL")
        .await
        .and_then(|mut r| r.take::<Option<Value>>(0))
        .ok()
        .flatten()
        .and_then(|v| v.get("c").and_then(|c| c.as_i64()))
        .unwrap_or(0);
    ok(json!({ "served": served(), "shares": shares }))
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use ed25519_dalek::SigningKey;

    fn keypair() -> (String, String) {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let pk = sk.verifying_key();
        (
            B64.encode(sk.to_bytes()),
            B64.encode(pk.to_bytes()),
        )
    }

    #[test]
    fn canonical_msg_is_exact_and_ordered() {
        let m = canonical_msg("tenantA", "put_share", "nodeX", "keys", "1", "3", 1725660000);
        assert_eq!(
            m,
            "9eck-vault-v1\ntenantA\nput_share\nnodeX\nkeys\n1\n3\n1725660000"
        );
    }

    #[test]
    fn canonical_msg_empty_fields_for_node_action() {
        let m = canonical_msg("t", "revoke", "n", "", "", "", 42);
        assert_eq!(m, "9eck-vault-v1\nt\nrevoke\nn\n\n\n\n42");
    }

    #[test]
    fn signed_canonical_msg_round_trips() {
        let (priv_b64, pub_b64) = keypair();
        let msg = canonical_msg("t1", "put_share", "n1", "keys", "2", "1", 1000);
        let sig = eck_core::utils::identity::sign_message(&priv_b64, &msg).unwrap();
        assert!(sig_ok(&pub_b64, &msg, &sig), "valid signature must verify");
        // A different message under the same key must fail.
        let other = canonical_msg("t1", "put_share", "n1", "keys", "2", "2", 1000);
        assert!(!sig_ok(&pub_b64, &other, &sig), "tampered message must not verify");
        // Wrong key must fail.
        let (_p2, pub2) = keypair();
        assert!(!sig_ok(&pub2, &msg, &sig), "wrong pubkey must not verify");
    }

    #[test]
    fn ct_eq_matches_and_rejects() {
        assert!(ct_eq(b"abcd", b"abcd"));
        assert!(!ct_eq(b"abcd", b"abce"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn ts_window_bounds() {
        assert!(ts_in_window(1000, 1000));
        assert!(ts_in_window(1000, 1000 + TS_WINDOW_SECS));
        assert!(ts_in_window(1000, 1000 - TS_WINDOW_SECS));
        assert!(!ts_in_window(1000, 1000 + TS_WINDOW_SECS + 1));
        assert!(!ts_in_window(1000, 1000 - TS_WINDOW_SECS - 1));
    }

    #[test]
    fn row_ids_are_deterministic_and_scoped() {
        assert_eq!(share_row_id("t", "n", "keys"), share_row_id("t", "n", "keys"));
        assert_ne!(share_row_id("t", "n", "keys"), share_row_id("t", "n", "recovery"));
        assert_ne!(share_row_id("t", "n", "keys"), share_row_id("t2", "n", "keys"));
        assert_eq!(share_row_id("t", "n", "keys").len(), 32);
        assert_eq!(node_row_id("t", "n").len(), 32);
    }

    #[test]
    fn kind_validation() {
        assert!(kind_valid("keys"));
        assert!(kind_valid("recovery"));
        assert!(kind_valid("enroll"));
        assert!(!kind_valid("other"));
        assert!(!kind_valid(""));
    }

    #[test]
    fn rate_limiter_caps_at_60() {
        // Use a dedicated tenant key so the process-global map is isolated.
        let t = "rate-test-tenant-unique-xyz";
        for i in 0..RATE_LIMIT {
            assert!(rate_ok(t), "request {i} within limit must pass");
        }
        assert!(!rate_ok(t), "the 61st request in the window must be denied");
    }
}
