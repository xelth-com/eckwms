//! Admin / operator side of the PII vault (`.eck/PII_VAULT.md` §2/§6-P0.5,
//! contract §3). These handlers run on a FULL node (home / operator) and drive
//! the safe on eck1/2/3 through [`eck_core::vault::client`]:
//!
//! * `POST /admin/vault/bootstrap` — first node of a tenant: mint the TMK, import
//!   the legacy epoch-0 pepper / MDK, seal the blob under this node's KEK, split
//!   into Shamir shares, and PUT them + a recovery blob to the safe. Returns the
//!   recovery key ONCE.
//! * `POST /admin/vault/enroll {node_id, role}` — mint a one-time enroll code for
//!   a newcomer (re-seal the live payload under the code, PUT an enroll share,
//!   register the node pending).
//! * `POST /admin/vault/join {code}` — run on the NEWCOMER: fetch the enroll
//!   share, open it with the code, re-seal under this node's own KEK, PUT the keys
//!   share (enroll-completion), and install the ring live. Also invoked at boot
//!   from `ECK_VAULT_ENROLL_CODE` via [`join_at_boot`].
//! * `POST /admin/vault/revoke {node_id}` — signed node revocation.
//! * `POST /admin/vault/recover {recovery_key}` — re-key a lost home node from the
//!   recovery blob.
//! * `GET  /admin/vault/status` — keyring state + KEK presence + safe roster.
//!
//! Secrets (recovery key, enroll code, KEK bytes, TMK) are NEVER logged or
//! persisted; the recovery key / enroll code are returned in the response body
//! exactly once. Every write to the safe is Ed25519-signed by this node's
//! identity on the seam shared with the server (see `handlers::vault_safe`).

use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use eck_core::utils::identity;
use eck_core::vault::{
    blob,
    client::{self, PutSharesRequest, SafeClient},
    kek, shamir,
    NodeActionRequest,
};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{error, info, warn};
use zeroize::Zeroizing;

// ── small helpers ────────────────────────────────────────────────────────────

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn okj(val: Value) -> Response {
    (StatusCode::OK, Json(val)).into_response()
}

/// 32 fresh random bytes (TMK, recovery secret, enroll code).
fn rand32() -> [u8; 32] {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b
}

/// `hex(sha256("9eck-vault-proof-v1" || secret))` — the recovery proof preimage.
/// The client presents it; the safe stores its SHA-256 hash (contract §2/§3).
fn recovery_proof_hex(secret: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"9eck-vault-proof-v1");
    h.update(secret);
    hex::encode(h.finalize())
}

/// `hex(sha256("9eck-vault-enroll-v1" || code))` — the enroll proof preimage.
/// The enroller sends it at register time (safe stores its hash); the newcomer
/// re-derives the same value from the code to prove possession at join.
fn enroll_proof_hex(code: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"9eck-vault-enroll-v1");
    h.update(code);
    hex::encode(h.finalize())
}

/// Verify the node's own paid license and return its tenant (`sub`). `Err` string
/// on missing / invalid / unpaid — the handler wrappers turn it into a 412.
fn license_tenant() -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    let token = std::env::var("ECK_LICENSE_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "no paid license configured (ECK_LICENSE_TOKEN unset)".to_string())?;
    let claims = eck_core::licensing::verify(
        &eck_core::licensing::node_issuer_pubkey(),
        &token,
        now,
        eck_core::licensing::DEFAULT_GRACE_SECS,
    )
    .map_err(|e| format!("license invalid: {e}"))?;
    if !claims.is_paid() {
        return Err(format!("license tier '{}' is not paid", claims.tier));
    }
    Ok(claims.sub)
}

/// Tenant or a 412 response.
fn verified_tenant() -> Result<String, Response> {
    license_tenant().map_err(|e| err(StatusCode::PRECONDITION_FAILED, &e))
}

/// Build a safe client with this node's signing identity attached.
fn safe_client(self_id: &str, priv_b64: &str, tenant: &str) -> Result<SafeClient, String> {
    SafeClient::from_env(reqwest::Client::new())
        .map(|c| c.with_signer(self_id, priv_b64, tenant))
        .ok_or_else(|| "safe not configured (ECK_VAULT_NODES / ECK_LICENSE_TOKEN)".to_string())
}

/// Sign a `register` / `revoke` node action with this node's key.
#[allow(clippy::too_many_arguments)]
fn signed_node_action(
    tenant: &str,
    self_id: &str,
    priv_b64: &str,
    target_node: &str,
    action: &str,
    role: Option<String>,
    enroll_proof: Option<String>,
) -> Result<NodeActionRequest, String> {
    let ts = chrono::Utc::now().timestamp();
    let msg = client::signing_message(tenant, action, target_node, "", "", "", ts);
    let sig = identity::sign_message(priv_b64, &msg).map_err(|e| format!("sign: {e}"))?;
    Ok(NodeActionRequest {
        node_id: target_node.to_string(),
        action: action.to_string(),
        role,
        signer_node_id: self_id.to_string(),
        ts,
        sig,
        enroll_proof,
    })
}

// ── POST /admin/vault/bootstrap ──────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct BootstrapRequest {
    /// Escape hatch for a genuinely new tenant with NO existing data: skip the
    /// SYNC_SECRET / MESH_DATA_KEY import gate (there is nothing to de-link).
    #[serde(default)]
    pub allow_fresh: bool,
}

pub async fn bootstrap(
    State(state): State<Arc<AppState>>,
    body: Option<Json<BootstrapRequest>>,
) -> Response {
    if eck_core::keyring::source() == eck_core::keyring::KeySource::Safe {
        return err(StatusCode::CONFLICT, "vault already initialised (ring source is safe)");
    }
    let allow_fresh = body.map(|b| b.0.allow_fresh).unwrap_or(false);
    let tenant = match verified_tenant() {
        Ok(t) => t,
        Err(r) => return r,
    };
    let self_id = state.instance_id.clone();
    let priv_b64 = state.server_identity.private_key.clone();
    let pub_b64 = state.server_identity.public_key.clone();

    // Import today's env secrets as the epoch-0 legacy values, so tokens /
    // envelopes stay byte-identical after the cut-over (contract §1.2 / §3).
    // A node that already holds data MUST import both — otherwise `derive` would
    // silently HKDF a fresh pepper/MDK and de-link every existing token/envelope.
    // Only a genuinely new tenant (no data) may bootstrap without them.
    let legacy_pp = std::env::var("SYNC_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .map(String::into_bytes);
    let legacy_mdk = std::env::var("MESH_DATA_KEY")
        .ok()
        .and_then(|raw| hex::decode(raw.trim()).ok())
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
    if !allow_fresh {
        if legacy_pp.is_none() {
            return err(StatusCode::PRECONDITION_FAILED,
                "SYNC_SECRET empty/unset: refusing bootstrap (would de-link every PPRL token). \
                 Set it, or pass {\"allow_fresh\":true} for a brand-new tenant with no data.");
        }
        if legacy_mdk.is_none() {
            return err(StatusCode::PRECONDITION_FAILED,
                "MESH_DATA_KEY missing or not 64-hex: refusing bootstrap (would de-link every \
                 mesh envelope). Fix it, or pass {\"allow_fresh\":true} for a brand-new tenant.");
        }
    }

    let client = match safe_client(&self_id, &priv_b64, &tenant) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::PRECONDITION_FAILED, &e),
    };

    let kek = match kek::load_or_create_kek() {
        Ok(k) => k,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("kek: {e}")),
    };

    let payload = blob::VaultPayload {
        v: 1,
        tmk: rand32(),
        legacy_pp,
        legacy_mdk,
        pp_epoch: 0,
        mdk_epoch: 0,
        vk_epoch: 0,
        datafs_epoch: 0,
        prev_mdk_epochs: Vec::new(),
        prev_vk_epochs: Vec::new(),
        created_at: chrono::Utc::now().timestamp(),
    };

    // Seal the keys blob under this node's KEK, split into shares.
    let aad_keys = blob::aad(&tenant, &self_id, "keys");
    let blob_keys = match blob::seal(&payload, &kek, &aad_keys) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("seal keys: {e}")),
    };
    let shares = shamir::split(&blob_keys, 2, 3, &mut rand::thread_rng());

    // Recovery secret + its proof. Zeroized on drop.
    let recovery_secret = Zeroizing::new(rand32());
    let proof = recovery_proof_hex(&*recovery_secret);

    let put_keys = PutSharesRequest {
        node_id: self_id.clone(),
        kind: "keys".into(),
        share_index: 0,
        share: String::new(),
        blob_len: blob_keys.len() as u32,
        version: 1,
        expires_in_secs: None,
        signer_node_id: self_id.clone(),
        signer_pubkey: Some(pub_b64.clone()),
        ts: 0,
        sig: String::new(),
        role: Some("home".into()),
        recovery_proof: Some(proof.clone()),
        enroll_proof: None,
    };
    let servers_ok = match client.put_shares(&put_keys, &shares).await {
        Ok(n) => n,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &format!("put keys shares: {e}")),
    };

    // Recovery blob: the SAME payload, sealed under a key derived from the
    // recovery secret, stored under node_id "recovery". This node is now an
    // active home, so the PUT rides the delegated (signed) form.
    let aad_rec = blob::aad(&tenant, "recovery", "recovery");
    let blob_rec = match blob::seal(&payload, &blob::code_key(&*recovery_secret), &aad_rec) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("seal recovery: {e}")),
    };
    let shares_rec = shamir::split(&blob_rec, 2, 3, &mut rand::thread_rng());
    let put_rec = PutSharesRequest {
        node_id: "recovery".into(),
        kind: "recovery".into(),
        share_index: 0,
        share: String::new(),
        blob_len: blob_rec.len() as u32,
        version: 1,
        expires_in_secs: None,
        signer_node_id: self_id.clone(),
        signer_pubkey: None,
        ts: 0,
        sig: String::new(),
        role: None,
        recovery_proof: None,
        enroll_proof: None,
    };
    let recovery_servers_ok = match client.put_shares(&put_rec, &shares_rec).await {
        Ok(n) => n,
        Err(e) => {
            // Keys are stored; recovery is best-effort at P0.5. Surface the count
            // (0) so the operator can re-establish recovery later. Never log the key.
            warn!("vault bootstrap: recovery share PUT failed ({e}); keys stored, recovery not");
            0
        }
    };

    // Install the live ring (retaining the payload for later enroll / recover).
    eck_core::keyring::install_payload(payload, 1);
    info!(
        "vault bootstrap complete (tenant={tenant}, node={self_id}, keys_servers={servers_ok}, recovery_servers={recovery_servers_ok})"
    );

    okj(json!({
        "ok": true,
        "tenant": tenant,
        "node_id": self_id,
        // Shown ONCE — never logged, never persisted.
        "recovery_key": blob::encode_secret(&recovery_secret),  // Zeroizing derefs to &[u8;32]
        "servers_ok": servers_ok,
        "recovery_servers_ok": recovery_servers_ok,
    }))
}

// ── POST /admin/vault/enroll ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct EnrollRequest {
    pub node_id: String,
    pub role: String,
}

pub async fn enroll(State(state): State<Arc<AppState>>, Json(body): Json<EnrollRequest>) -> Response {
    if body.role != "operator" && body.role != "home" {
        return err(StatusCode::BAD_REQUEST, "role must be 'operator' or 'home'");
    }
    let tenant = match verified_tenant() {
        Ok(t) => t,
        Err(r) => return r,
    };
    let self_id = state.instance_id.clone();
    let priv_b64 = state.server_identity.private_key.clone();

    let Some(payload) = eck_core::keyring::current_payload() else {
        return err(
            StatusCode::CONFLICT,
            "vault not initialised on this node (no payload); run bootstrap or join first",
        );
    };
    let version = eck_core::keyring::version();

    let client = match safe_client(&self_id, &priv_b64, &tenant) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::PRECONDITION_FAILED, &e),
    };

    // Refuse to enroll a node that is already active — re-enrolling would downgrade
    // it to `pending` (losing its keys-share auth). Revoke first, then enroll.
    match client.list_nodes().await {
        Ok(rows) => {
            if rows.iter().any(|r| r.node_id == body.node_id && r.status == "active") {
                return err(StatusCode::CONFLICT,
                    "node is already active; revoke it before re-enrolling");
            }
        }
        Err(e) => return err(StatusCode::BAD_GATEWAY, &format!("list nodes: {e}")),
    }

    let code = Zeroizing::new(rand32());
    let enroll_proof = enroll_proof_hex(&*code);
    let aad_e = blob::aad(&tenant, &body.node_id, "enroll");
    let blob_e = match blob::seal(&payload, &blob::code_key(&*code), &aad_e) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("seal enroll: {e}")),
    };
    let shares_e = shamir::split(&blob_e, 2, 3, &mut rand::thread_rng());

    // PUT the enroll share BEFORE registering the node: once the node row is
    // `pending`, the server routes any share PUT for it through the
    // enroll-COMPLETION form (kind=keys, signer==node), which would reject this
    // delegated enroll-share write. So: share first (delegated), register after.
    let expires_in_secs: u64 = 900;
    let put_e = PutSharesRequest {
        node_id: body.node_id.clone(),
        kind: "enroll".into(),
        share_index: 0,
        share: String::new(),
        blob_len: blob_e.len() as u32,
        version,
        expires_in_secs: Some(expires_in_secs),
        signer_node_id: self_id.clone(),
        signer_pubkey: None,
        ts: 0,
        sig: String::new(),
        role: None,
        recovery_proof: None,
        enroll_proof: None,
    };
    let servers_ok = match client.put_shares(&put_e, &shares_e).await {
        Ok(n) => n,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &format!("put enroll share: {e}")),
    };

    // Register the newcomer (pending); pubkey is bound later at join-completion.
    let na = match signed_node_action(
        &tenant,
        &self_id,
        &priv_b64,
        &body.node_id,
        client::ACTION_REGISTER,
        Some(body.role.clone()),
        Some(enroll_proof),
    ) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    if let Err(e) = client.node_action(&na).await {
        return err(StatusCode::BAD_GATEWAY, &format!("register node: {e}"));
    }

    info!(
        "vault enroll issued (tenant={tenant}, node={}, role={}, servers={servers_ok})",
        body.node_id, body.role
    );
    okj(json!({
        "ok": true,
        "node_id": body.node_id,
        // Shown ONCE — never logged, never persisted.
        "code": blob::encode_secret(&code),  // Zeroizing derefs to &[u8;32]
        "expires_in_secs": expires_in_secs,
        "servers_ok": servers_ok,
    }))
}

// ── POST /admin/vault/join (run on the NEWCOMER) ─────────────────────────────

#[derive(Deserialize)]
pub struct JoinRequest {
    pub code: String,
}

pub async fn join(State(state): State<Arc<AppState>>, Json(body): Json<JoinRequest>) -> Response {
    match join_core(
        &state.instance_id,
        &state.server_identity.private_key,
        &state.server_identity.public_key,
        &body.code,
    )
    .await
    {
        Ok(v) => okj(v),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e),
    }
}

/// The join routine, factored so both the handler and the boot hook share it.
/// Fetches the enroll share, opens it with the code, re-seals under this node's
/// own KEK, PUTs the keys share (enroll-completion), and installs the ring.
async fn join_core(
    self_id: &str,
    priv_b64: &str,
    pub_b64: &str,
    code_str: &str,
) -> Result<Value, String> {
    let tenant = license_tenant()?;
    let code = Zeroizing::new(
        blob::parse_secret(code_str.trim()).map_err(|e| format!("invalid code: {e}"))?,
    );
    // Re-derive the enroll proof from the code to prove possession to the safe.
    let enroll_proof = enroll_proof_hex(&*code);

    let client = safe_client(self_id, priv_b64, &tenant)?;

    let fetched = client.fetch_shares(self_id, "enroll", Some(&enroll_proof)).await;
    // Keep only the newest-version shares (a partial PUT would combine to garbage).
    let version = fetched.iter().map(|(_, _, v)| *v).max().unwrap_or(0);
    let shares: Vec<shamir::Share> = fetched
        .into_iter()
        .filter(|(_, _, v)| *v == version)
        .map(|(_, s, _)| s)
        .collect();
    if shares.len() < 2 {
        return Err(format!(
            "safe returned {} enroll share(s) at version {version}; need >= 2 (code expired, \
             wrong code, or node not enrolled?)",
            shares.len()
        ));
    }
    let secret_len = shares.iter().map(|s| s.data.len()).max().unwrap_or(0);
    let blob_e = shamir::combine(&shares, secret_len)?;

    let aad_e = blob::aad(&tenant, self_id, "enroll");
    let payload = blob::open(&blob_e, &blob::code_key(&*code), &aad_e)
        .map_err(|e| format!("open enroll blob: {e}"))?;

    // Re-seal under THIS node's own KEK (the enroll KEK stays local forever).
    let kek = kek::load_or_create_kek()?;
    let aad_keys = blob::aad(&tenant, self_id, "keys");
    let blob_keys = blob::seal(&payload, &kek, &aad_keys).map_err(|e| format!("seal keys: {e}"))?;
    let shares_keys = shamir::split(&blob_keys, 2, 3, &mut rand::thread_rng());

    let put = PutSharesRequest {
        node_id: self_id.to_string(),
        kind: "keys".into(),
        share_index: 0,
        share: String::new(),
        blob_len: blob_keys.len() as u32,
        version,
        expires_in_secs: None,
        signer_node_id: self_id.to_string(),
        signer_pubkey: Some(pub_b64.to_string()),
        ts: 0,
        sig: String::new(),
        role: None,
        recovery_proof: None,
        enroll_proof: Some(enroll_proof),
    };
    let servers_ok = client
        .put_shares(&put, &shares_keys)
        .await
        .map_err(|e| format!("put keys share: {e}"))?;

    eck_core::keyring::install_payload(payload, version);
    info!("vault join complete (tenant={tenant}, node={self_id}, version={version}, servers={servers_ok})");
    Ok(json!({
        "ok": true,
        "node_id": self_id,
        "version": version,
        "servers_ok": servers_ok,
    }))
}

/// Boot-time enrolment: called from `main` when `ECK_VAULT_ENROLL_CODE` is set
/// and the ring is not already safe-sourced. Loads this node's identity and runs
/// the same [`join_core`]. Never panics; the code is never logged.
pub async fn join_at_boot(instance_id: &str, code: &str) {
    let identity = identity::load_or_generate_identity(instance_id);
    match join_core(instance_id, &identity.private_key, &identity.public_key, code).await {
        Ok(_) => info!("vault enroll-at-boot succeeded; you may now remove ECK_VAULT_ENROLL_CODE"),
        Err(e) => error!("vault enroll-at-boot failed ({e}); keeping current keys"),
    }
}

// ── POST /admin/vault/revoke ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RevokeRequest {
    pub node_id: String,
}

pub async fn revoke(State(state): State<Arc<AppState>>, Json(body): Json<RevokeRequest>) -> Response {
    let tenant = match verified_tenant() {
        Ok(t) => t,
        Err(r) => return r,
    };
    let self_id = state.instance_id.clone();
    let priv_b64 = state.server_identity.private_key.clone();

    let client = match safe_client(&self_id, &priv_b64, &tenant) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::PRECONDITION_FAILED, &e),
    };
    let na = match signed_node_action(
        &tenant,
        &self_id,
        &priv_b64,
        &body.node_id,
        client::ACTION_REVOKE,
        None,
        None,
    ) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    match client.node_action(&na).await {
        Ok(n) => {
            info!("vault node revoked (tenant={tenant}, node={}, servers={n})", body.node_id);
            okj(json!({ "ok": true, "node_id": body.node_id, "servers_ok": n }))
        }
        Err(e) => err(StatusCode::BAD_GATEWAY, &format!("revoke: {e}")),
    }
}

// ── POST /admin/vault/recover ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RecoverRequest {
    pub recovery_key: String,
}

pub async fn recover(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RecoverRequest>,
) -> Response {
    let tenant = match verified_tenant() {
        Ok(t) => t,
        Err(r) => return r,
    };
    let self_id = state.instance_id.clone();
    let priv_b64 = state.server_identity.private_key.clone();
    let pub_b64 = state.server_identity.public_key.clone();

    let secret = match blob::parse_secret(body.recovery_key.trim()) {
        Ok(s) => Zeroizing::new(s),
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("invalid recovery key: {e}")),
    };
    let proof = recovery_proof_hex(&*secret);

    let client = match safe_client(&self_id, &priv_b64, &tenant) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::PRECONDITION_FAILED, &e),
    };

    let fetched = client.fetch_shares("recovery", "recovery", Some(&proof)).await;
    let version = fetched.iter().map(|(_, _, v)| *v).max().unwrap_or(0);
    let shares: Vec<shamir::Share> = fetched
        .into_iter()
        .filter(|(_, _, v)| *v == version)
        .map(|(_, s, _)| s)
        .collect();
    if shares.len() < 2 {
        return err(
            StatusCode::BAD_GATEWAY,
            &format!(
                "safe returned {} recovery share(s) at version {version}; need >= 2 (wrong key?)",
                shares.len()
            ),
        );
    }
    let secret_len = shares.iter().map(|s| s.data.len()).max().unwrap_or(0);
    let blob_rec = match shamir::combine(&shares, secret_len) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("combine: {e}")),
    };

    let aad_rec = blob::aad(&tenant, "recovery", "recovery");
    let payload = match blob::open(&blob_rec, &blob::code_key(&*secret), &aad_rec) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("open recovery blob: {e}")),
    };

    // Re-seal under this (new home) node's KEK and PUT keys via the recovery form.
    let kek = match kek::load_or_create_kek() {
        Ok(k) => k,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("kek: {e}")),
    };
    let aad_keys = blob::aad(&tenant, &self_id, "keys");
    let blob_keys = match blob::seal(&payload, &kek, &aad_keys) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("seal keys: {e}")),
    };
    let shares_keys = shamir::split(&blob_keys, 2, 3, &mut rand::thread_rng());

    let put = PutSharesRequest {
        node_id: self_id.clone(),
        kind: "keys".into(),
        share_index: 0,
        share: String::new(),
        blob_len: blob_keys.len() as u32,
        version,
        expires_in_secs: None,
        signer_node_id: self_id.clone(),
        signer_pubkey: Some(pub_b64),
        ts: 0,
        sig: String::new(),
        role: Some("home".into()),
        recovery_proof: Some(proof),
        enroll_proof: None,
    };
    let servers_ok = match client.put_shares(&put, &shares_keys).await {
        Ok(n) => n,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &format!("put keys share: {e}")),
    };

    eck_core::keyring::install_payload(payload, version);
    info!("vault recover complete (tenant={tenant}, node={self_id}, version={version}, servers={servers_ok})");
    okj(json!({
        "ok": true,
        "node_id": self_id,
        "version": version,
        "servers_ok": servers_ok,
    }))
}

// ── POST /admin/vault/rotate ─────────────────────────────────────────────────

/// `{"mdk": true, "vk": true}` — bump those epochs; `{"pp": true}` — refused
/// (409, needs P1); `{"finalize": true}` — retire the previous epochs.
#[derive(Deserialize, Default)]
pub struct RotateRequest {
    /// Rotate the mesh data key: new envelopes are sealed under MDK[e+1] and the
    /// retired epoch stays readable until `finalize`.
    #[serde(default)]
    pub mdk: bool,
    /// Rotate the vault key (P1 DEK wrapping). Trivial today — no DEKs exist.
    #[serde(default)]
    pub vk: bool,
    /// Rotate the PPRL pepper — always refused here, see the handler.
    #[serde(default)]
    pub pp: bool,
    /// Retire every previous epoch instead of rotating. Requires a complete
    /// re-envelope sweep (or `force`).
    #[serde(default)]
    pub finalize: bool,
    /// `finalize` only: proceed even though the sweep has not reported 100 %.
    /// Every envelope still at an old epoch becomes permanently unreadable.
    #[serde(default)]
    pub force: bool,
}

/// Rotate the tenant's derived keys to the next epoch (`.eck/PII_VAULT.md` §2
/// "Rotation", P0.7).
///
/// The TMK never changes: `MDK[e] = HKDF(TMK,"mesh",e)`, so a rotation is a
/// re-seal of the same blob with bumped epoch fields plus a note of which
/// epochs must still open stored ciphertext. What it buys is concrete: epoch 0
/// IS the old `MESH_DATA_KEY`, which sat for months in a world-readable `.env`
/// on every node and in every backup. After the rotation and the sweep, whoever
/// copied that file can no longer open what the cache nodes hold.
///
/// The sequence this handler performs:
/// 1. seal the new payload under this node's KEK, split, PUT to the safe with
///    `version + 1` — refusing unless **>= 2 servers ack** (`put_shares`);
/// 2. install the new ring locally, keeping the retired epochs for decrypt;
/// 3. arm every OTHER active full node with a one-time re-key code (the new
///    payload sealed under that code, stored as its `enroll` share). Each of
///    those nodes adopts it by running `POST /admin/vault/join {code}` — they
///    cannot be re-sealed remotely, since their blob is wrapped under a KEK that
///    never leaves them. The codes are returned ONCE, here.
///
/// Until a node has joined it keeps encrypting at the old epoch, and keeps
/// reading everything (the old epoch is still current for it). Nodes that HAVE
/// rotated read its old envelopes through `prev_mdk`. So the fleet is correct
/// throughout the window — it is simply not finished until every node has joined
/// and the sweep has run.
pub async fn rotate(State(state): State<Arc<AppState>>, body: Option<Json<RotateRequest>>) -> Response {
    let req = body.map(|b| b.0).unwrap_or_default();

    // The pepper is the one key that cannot move yet: rotating it re-mints every
    // PPRL token, and rewriting `ai_summary` / `meta.subject` / `pii_fingerprints`
    // / `pii_fuzzy` / order tokens from old to new requires holding the CLEAR
    // values — which is exactly what the P1 `pii_vault` provides and this build
    // does not have. See PII_VAULT.md §6 P1b.
    if req.pp {
        return err(
            StatusCode::CONFLICT,
            "pepper (pp) rotation needs the P1 vault: a new pepper re-mints every PPRL token, and \
             rewriting old->new across ai_summary / meta.subject / pii_fingerprints / pii_fuzzy / \
             order tokens requires the clear values that only pii_vault holds. Scheduled as P1b \
             (.eck/PII_VAULT.md §6). Rotate {\"mdk\":true} now; pp stays at epoch 0.",
        );
    }
    if !req.finalize && !req.mdk && !req.vk {
        return err(
            StatusCode::BAD_REQUEST,
            "nothing to do: pass {\"mdk\":true} and/or {\"vk\":true}, or {\"finalize\":true}",
        );
    }
    if eck_core::keyring::source() != eck_core::keyring::KeySource::Safe {
        return err(
            StatusCode::CONFLICT,
            "keys are not safe-sourced on this node (ring source is env/keyfile) — rotation would \
             have nowhere to publish the new blob",
        );
    }
    let Some(payload) = eck_core::keyring::current_payload() else {
        return err(
            StatusCode::CONFLICT,
            "vault not initialised on this node (no payload in RAM); run bootstrap or join first",
        );
    };
    let tenant = match verified_tenant() {
        Ok(t) => t,
        Err(r) => return r,
    };
    let self_id = state.instance_id.clone();
    let priv_b64 = state.server_identity.private_key.clone();
    let pub_b64 = state.server_identity.public_key.clone();

    // Finalize retires the decrypt fallback — refuse while anything might still
    // be sitting at an old epoch.
    if req.finalize {
        let sweep_ok = crate::services::vault_sweep::is_complete()
            && crate::services::vault_sweep::completed_epoch() == payload.mdk_epoch;
        if !sweep_ok && !req.force {
            return err(
                StatusCode::CONFLICT,
                "re-envelope sweep has not reported 100% at the current mdk epoch — finalizing now \
                 would make every remaining old-epoch envelope permanently unreadable. Run \
                 POST /admin/vault/sweep, check GET /admin/vault/status, or pass {\"force\":true} \
                 if you accept the loss.",
            );
        }
    }

    let next = if req.finalize {
        blob::finalize(&payload)
    } else {
        blob::rotate(&payload, req.mdk, req.vk)
    };
    let version = eck_core::keyring::version().saturating_add(1);

    let client = match safe_client(&self_id, &priv_b64, &tenant) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::PRECONDITION_FAILED, &e),
    };
    let kek = match kek::load_or_create_kek() {
        Ok(k) => k,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("kek: {e}")),
    };
    let aad_keys = blob::aad(&tenant, &self_id, "keys");
    let blob_keys = match blob::seal(&next, &kek, &aad_keys) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("seal keys: {e}")),
    };
    let shares = shamir::split(&blob_keys, 2, 3, &mut rand::thread_rng());
    let put = PutSharesRequest {
        node_id: self_id.clone(),
        kind: "keys".into(),
        share_index: 0,
        share: String::new(),
        blob_len: blob_keys.len() as u32,
        version,
        expires_in_secs: None,
        signer_node_id: self_id.clone(),
        signer_pubkey: Some(pub_b64.clone()),
        ts: 0,
        sig: String::new(),
        role: None,
        recovery_proof: None,
        enroll_proof: None,
    };
    // `put_shares` is the >= 2-server gate: it returns Err unless two servers
    // accepted, and the OLD version stays readable on all three either way.
    let servers_ok = match client.put_shares(&put, &shares).await {
        Ok(n) => n,
        Err(e) => {
            return err(
                StatusCode::BAD_GATEWAY,
                &format!("rotation aborted, nothing changed locally: {e}"),
            )
        }
    };

    // Only now install locally — a ring the safe did not accept would be a node
    // that cannot re-fetch its own keys on the next boot.
    eck_core::keyring::install_payload(next.clone(), version);

    // Arm the other active full nodes.
    let (rekeyed, rekey_errors) =
        arm_rekeys(&client, &tenant, &self_id, &priv_b64, &next, version, &state).await;

    let action = if req.finalize { "vault-rotate-finalize" } else { "vault-rotate" };
    eck_core::audit::append_soft(
        &state.db,
        &state.server_identity,
        &eck_core::audit::wms_chain(&state.instance_id),
        "admin",
        action,
        eck_core::audit::class::VAULT,
        &format!(
            "vault {} (mdk_epoch={}, vk_epoch={}, pp_epoch={}, version={version})",
            if req.finalize { "epochs finalized" } else { "keys rotated" },
            next.mdk_epoch, next.vk_epoch, next.pp_epoch
        ),
        json!({
            "finalize": req.finalize,
            "forced": req.finalize && req.force,
            "mdk_epoch": next.mdk_epoch,
            "vk_epoch": next.vk_epoch,
            "pp_epoch": next.pp_epoch,
            "prev_mdk_epochs": next.prev_mdk_epochs,
            "prev_vk_epochs": next.prev_vk_epochs,
            "version": version,
            "servers_ok": servers_ok,
            "rekeyed_nodes": rekeyed.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            "rekey_errors": rekey_errors,
        }),
    )
    .await;
    info!(
        "vault {} (tenant={tenant}, node={self_id}, mdk_epoch={}, vk_epoch={}, version={version}, servers={servers_ok}, rekeyed={})",
        if req.finalize { "finalize" } else { "rotate" },
        next.mdk_epoch, next.vk_epoch, rekeyed.len()
    );

    okj(json!({
        "ok": true,
        "finalize": req.finalize,
        "mdk_epoch": next.mdk_epoch,
        "vk_epoch": next.vk_epoch,
        "pp_epoch": next.pp_epoch,
        "prev_mdk_epochs": next.prev_mdk_epochs,
        "prev_vk_epochs": next.prev_vk_epochs,
        "version": version,
        "servers_ok": servers_ok,
        // Shown ONCE — never logged, never persisted. Run
        // `POST /admin/vault/join {"code": …}` on each of these nodes.
        "rekey_codes": rekeyed.into_iter().collect::<serde_json::Map<_, _>>(),
        "rekey_errors": rekey_errors,
        "next_step": if req.finalize {
            "done — prev epochs retired"
        } else {
            "1) run join on each re-keyed node, 2) POST /admin/vault/sweep, 3) rotate {\"finalize\":true}"
        },
    }))
}

/// Seal the rotated payload under a fresh one-time code for every OTHER active
/// `home`/`operator` node, store it as that node's `enroll` share, and arm the
/// node row with the matching proof (`ACTION_REKEY`).
///
/// A node's blob is wrapped under a KEK that never leaves it, so this is the
/// only way to hand it a new payload without a round trip through the node
/// itself. Best-effort per node: one unreachable operator must not abort a
/// rotation that the safe has already accepted. Returns `(node → code, errors)`.
#[allow(clippy::too_many_arguments)]
async fn arm_rekeys(
    client: &SafeClient,
    tenant: &str,
    self_id: &str,
    priv_b64: &str,
    payload: &blob::VaultPayload,
    version: u32,
    _state: &Arc<AppState>,
) -> (Vec<(String, Value)>, Vec<String>) {
    let mut codes = Vec::new();
    let mut errors = Vec::new();
    let rows = match client.list_nodes().await {
        Ok(r) => r,
        Err(e) => {
            errors.push(format!("list nodes: {e}"));
            return (codes, errors);
        }
    };
    for row in rows {
        if row.node_id == self_id || row.status != "active" {
            continue;
        }
        if row.role != "home" && row.role != "operator" {
            continue; // cache / relay nodes hold no mesh data key
        }
        let code = Zeroizing::new(rand32());
        let proof = enroll_proof_hex(&*code);
        let aad_e = blob::aad(tenant, &row.node_id, "enroll");
        let blob_e = match blob::seal(payload, &blob::code_key(&*code), &aad_e) {
            Ok(b) => b,
            Err(e) => {
                errors.push(format!("{}: seal: {e}", row.node_id));
                continue;
            }
        };
        let shares = shamir::split(&blob_e, 2, 3, &mut rand::thread_rng());
        // Re-key codes live longer than an enrolment: the operator may be asleep.
        let expires_in_secs: u64 = 3600;
        let put = PutSharesRequest {
            node_id: row.node_id.clone(),
            kind: "enroll".into(),
            share_index: 0,
            share: String::new(),
            blob_len: blob_e.len() as u32,
            version,
            expires_in_secs: Some(expires_in_secs),
            signer_node_id: self_id.to_string(),
            signer_pubkey: None,
            ts: 0,
            sig: String::new(),
            role: None,
            recovery_proof: None,
            enroll_proof: None,
        };
        if let Err(e) = client.put_shares(&put, &shares).await {
            errors.push(format!("{}: put enroll share: {e}", row.node_id));
            continue;
        }
        let na = match signed_node_action(
            tenant,
            self_id,
            priv_b64,
            &row.node_id,
            client::ACTION_REKEY,
            None,
            Some(proof),
        ) {
            Ok(a) => a,
            Err(e) => {
                errors.push(format!("{}: sign rekey: {e}", row.node_id));
                continue;
            }
        };
        if let Err(e) = client.node_action(&na).await {
            errors.push(format!("{}: arm rekey: {e}", row.node_id));
            continue;
        }
        info!(
            "vault re-key armed for {} (role={}, expires_in={}s)",
            row.node_id, row.role, expires_in_secs
        );
        codes.push((row.node_id.clone(), json!(blob::encode_secret(&code))));
    }
    (codes, errors)
}

// ── POST /admin/vault/sweep ──────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct SweepRequest {
    /// `"start"` (default), `"stop"`, `"reset"`.
    #[serde(default)]
    pub action: Option<String>,
}

/// Drive the re-envelope sweep that makes an MDK rotation actually take effect
/// on the cache nodes (`crate::services::vault_sweep`). Idempotent, resumable,
/// rate-limited; progress lives in `GET /admin/vault/status`.
pub async fn sweep(State(state): State<Arc<AppState>>, body: Option<Json<SweepRequest>>) -> Response {
    let action = body
        .and_then(|b| b.0.action)
        .unwrap_or_else(|| "start".to_string());
    match action.as_str() {
        "stop" => {
            crate::services::vault_sweep::cancel();
            okj(json!({ "ok": true, "action": "stop" }))
        }
        "reset" => match crate::services::vault_sweep::reset() {
            Ok(()) => okj(json!({ "ok": true, "action": "reset" })),
            Err(e) => err(StatusCode::CONFLICT, &e),
        },
        "start" => match crate::services::vault_sweep::start(state.clone()) {
            Ok(epoch) => {
                eck_core::audit::append_soft(
                    &state.db,
                    &state.server_identity,
                    &eck_core::audit::wms_chain(&state.instance_id),
                    "admin",
                    "vault-reenvelope-sweep",
                    eck_core::audit::class::VAULT,
                    &format!("re-envelope sweep started (target mdk_epoch={epoch})"),
                    json!({ "target_mdk_epoch": epoch }),
                )
                .await;
                okj(json!({ "ok": true, "action": "start", "target_mdk_epoch": epoch }))
            }
            Err(e) => err(StatusCode::CONFLICT, &e),
        },
        other => err(
            StatusCode::BAD_REQUEST,
            &format!("unknown action '{other}' (start|stop|reset)"),
        ),
    }
}

// ── GET /admin/vault/status ──────────────────────────────────────────────────

pub async fn status(State(_state): State<Arc<AppState>>) -> Response {
    let nodes = match SafeClient::from_env(reqwest::Client::new()) {
        Some(client) => match client.list_nodes().await {
            Ok(rows) => json!(rows),
            Err(e) => json!({ "error": e }),
        },
        None => json!({ "error": "safe not configured (ECK_VAULT_NODES / ECK_LICENSE_TOKEN)" }),
    };
    okj(json!({
        "keyring": eck_core::keyring::status_json(),
        // P0.7 rotation: which epochs are live, which are kept for decrypt, and
        // how far the re-envelope sweep has got (`POST /admin/vault/sweep`).
        "rotation": json!({
            "mdk_epoch": eck_core::keyring::mesh_data_key_epoch(),
            "prev_mdk_epochs": eck_core::keyring::prev_mdk_epochs(),
            "prev_vk_epochs": eck_core::keyring::prev_vault_keys()
                .iter().map(|(e, _)| *e).collect::<Vec<_>>(),
            "pp_epoch_frozen_reason":
                "pepper rotation needs the P1 vault to re-mint tokens (PII_VAULT.md §6 P1b)",
            "sweep": crate::services::vault_sweep::status_json(),
        }),
        // Encrypted data-at-rest volume: {enabled, mounted, cipher_dir, mount}.
        "datafs": eck_core::vault::datafs::status_json(),
        "kek_present": kek::kek_exists(),
        "kek_path": kek::kek_path().display().to_string(),
        "safe_nodes": client::safe_nodes_from_env(),
        "nodes": nodes,
    }))
}
