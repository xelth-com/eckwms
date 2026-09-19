//! The PII vault — the ONE module that turns a clear customer value into a
//! PPRL token and back (`.eck/PII_VAULT.md` §3.1 / §3.3, migration phase **P1**).
//!
//! A vault row is keyed by the token itself, so the table is de-duplicated by
//! construction: the same person seen on 40 tickets is one row. The clear value
//! lives only inside `enc`, sealed under a per-row DEK that is itself wrapped
//! under the node's vault key `VK[e]`:
//!
//! ```text
//! pii_vault:{token}
//!   kind         "Name"|"Email"|"Phone"|"Address"|"Company"
//!   enc          b64( nonce24 || AEAD(DEK,  clear-JSON, aad = DOMAIN|token|kind|vk_epoch) )
//!   dek_wrapped  b64( nonce24 || AEAD(VK[e], DEK,       aad = DOMAIN|token) )
//!   vk_epoch     which VK wrapped the DEK        (rewrap target, P1b sweep)
//!   pp_epoch     which pepper minted the token   (P1b pepper rotation reads this)
//!   fuzzy        FuzzySig JSON — the search property, no clear
//!   first_seen_at / last_seen_at / home_instance_id / _vclock / updated_at
//! ```
//!
//! **AAD binds the ciphertext to `token ‖ kind ‖ vk_epoch`**, so a row cannot be
//! swapped under another token and a stale-epoch row cannot be passed off as a
//! current one — both surface as a decrypt failure, never as wrong plaintext.
//! The DEK's own AAD deliberately omits the epoch (it is what a rewrap changes).
//!
//! ## The gate: no vault key ⇒ this module does nothing
//! Every entry point short-circuits on [`enabled`] (`keyring::vault_key()` is
//! `Some`, i.e. a safe- or keyfile-sourced node whose role derives a VK). An
//! env-only dev node behaves EXACTLY as it did before this module existed:
//! [`put`] returns `None` (callers keep the clear value), [`get`] returns
//! `None` (reveal falls back to the re-hash path), the `pii_vault` table is
//! never written and never syncs. `ECK_VAULT_DISABLE=1` is a kill switch that
//! forces the same state on a node that *does* hold a VK.
//!
//! ## Transition: read paths accept BOTH
//! P1 tokenises what the import path writes from now on; the ~7000 rows already
//! in the DB keep their clear `meta.*` until the **P1b** backfill sweep. So every
//! reader goes through [`clear_or_token`] / [`customer_display`], which classify
//! a stored string by shape (`Name_8E5F3A1B00000000` is a token, anything else
//! is clear) instead of by a schema flag.
//!
//! ## What is explicitly NOT here (P1b)
//! * the backfill sweep over legacy clear rows,
//! * the pepper rotation and the both-epoch OR in queries (`pp_epoch` is
//!   written on every row precisely so that sweep is a pure re-key),
//! * wiring `fuzzy` into the live `FuzzyIndex` (Channel B still self-heals from
//!   the process mint log on the importing node).

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};
use zeroize::{Zeroize, Zeroizing};

use crate::db::SurrealDb;
use crate::keyring;
use crate::utils::anonymizer::{
    obfuscate_pii, parse_pii_token, pii_fuzzy_sig_address, pii_fuzzy_sig_kind, plausible_person_name,
    SigKind,
};

/// The vault table. Sync class `vault` — see `sync::engine::VAULT_ENTITY_TYPES`.
pub const TABLE: &str = "pii_vault";

/// Domain separation tag for every AAD this module builds.
const DOMAIN: &str = "9eck-pii-vault-v1";

/// Values shorter than this are not worth a vault row (and are exactly what the
/// reveal path already skips) — initials, stray punctuation, empty fields.
const MIN_VALUE_LEN: usize = 2;

/// Cap on a single [`get_many`] batch. Each token is one point read on the
/// embedded KV; the reveal surfaces already cap themselves well below this.
const MAX_GET_MANY: usize = 256;

/// Refresh `last_seen_at` at most this often per row — it is merkle-IGNORED
/// bookkeeping, so a write per ticket import would be pure disk churn.
const LAST_SEEN_REFRESH_SECS: i64 = 86_400;

// ─── kinds ──────────────────────────────────────────────────────────────────

/// The PII classes the vault stores. The label doubles as the `obfuscate_pii`
/// prefix, so `Kind::Name` ⇒ `Name_<hex>` — the exact tokens every other
/// surface already speaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Name,
    Email,
    Phone,
    Address,
    Company,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Name => "Name",
            Kind::Email => "Email",
            Kind::Phone => "Phone",
            Kind::Address => "Address",
            Kind::Company => "Company",
        }
    }

    pub fn from_label(s: &str) -> Option<Kind> {
        Some(match s {
            "Name" => Kind::Name,
            "Email" => Kind::Email,
            "Phone" => Kind::Phone,
            "Address" => Kind::Address,
            "Company" => Kind::Company,
            _ => return None,
        })
    }

    /// Which fuzzy signature (if any) this kind carries. Emails/phones are
    /// exact-match identifiers — a Bloom over them would leak without buying
    /// typo tolerance, so they get no sig.
    fn sig_kind(self) -> Option<SigKind> {
        match self {
            Kind::Name | Kind::Company => Some(SigKind::Name),
            Kind::Address => Some(SigKind::Address),
            Kind::Email | Kind::Phone => None,
        }
    }
}

// ─── the plaintext ──────────────────────────────────────────────────────────

/// The decrypted contents of one vault row. `value` is the canonical string the
/// token was minted from; `extra` carries the per-kind structure of §3.1
/// (`given`/`family` for a Name, `e164` for a Phone, `street`/`house`/`zip`/
/// `city`/`country`/`geo` for an Address).
///
/// Wiped on drop and `[redacted]` in `Debug` — a `Clear` must never reach a log
/// line by accident.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Clear {
    pub value: String,
    // `flatten` (not a nested object) so the stored JSON is exactly the §3.1
    // per-kind shape: `{"value":…,"zip":…,"city":…}`. serde forbids
    // `skip_serializing_if` on a flattened field — an empty map simply adds
    // no keys, which is the same result.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, Value>,
}

impl Clear {
    pub fn new(value: impl Into<String>) -> Self {
        Clear {
            value: value.into(),
            extra: serde_json::Map::new(),
        }
    }

    /// Builder for the structured sidecar fields (`zip`, `city`, `e164`, …).
    pub fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        let v = value.into();
        if !v.trim().is_empty() {
            self.extra.insert(key.to_string(), json!(v));
        }
        self
    }

    fn str_extra(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
    }
}

impl Drop for Clear {
    fn drop(&mut self) {
        self.value.zeroize();
        self.extra.clear();
    }
}

impl std::fmt::Debug for Clear {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clear([redacted])")
    }
}

// ─── the gate ───────────────────────────────────────────────────────────────

/// True when this node may hold clear PII in the vault — i.e. it has a vault
/// key (`home` / `operator` role) and the kill switch is off.
///
/// Everything in this module is a no-op when this is false, which is what keeps
/// an env-only dev node byte-identical to its pre-vault behaviour.
pub fn enabled() -> bool {
    if std::env::var("ECK_VAULT_DISABLE").map(|v| v == "1").unwrap_or(false) {
        return false;
    }
    keyring::vault_key().is_some()
}

/// The current VK epoch, read out of the keyring's public status (the ring's
/// fields are private and `keyring.rs` is owned by the rotation work — compose,
/// don't extend).
pub fn vk_epoch() -> u32 {
    keyring::status_json()
        .get("vk_epoch")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32
}

/// The current pepper epoch — stamped on every row so the P1b pepper rotation
/// knows which rows it still has to re-mint.
pub fn pp_epoch() -> u32 {
    keyring::status_json()
        .get("pepper_epoch")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32
}

/// Current VK plus every retired epoch, newest first — the unwrap order for a
/// DEK that has not been rewrapped yet.
fn unwrap_keys() -> Vec<(u32, [u8; 32])> {
    let mut keys = Vec::new();
    if let Some(k) = keyring::vault_key() {
        keys.push((vk_epoch(), k));
    }
    keys.extend(keyring::prev_vault_keys());
    keys
}

// ─── the envelope ───────────────────────────────────────────────────────────

/// A sealed value: the ciphertext and the DEK that opens it, both base64
/// (SurrealDB's `bytes` type does not round-trip through `serde_json::Value` —
/// `.eck/SURREALDB_AI_RULES.md` Rule 8).
#[derive(Clone, Debug, PartialEq)]
pub struct Sealed {
    pub enc: String,
    pub dek_wrapped: String,
    pub vk_epoch: u32,
}

fn aad_enc(token: &str, kind: &str, epoch: u32) -> String {
    format!("{DOMAIN}|{token}|{kind}|{epoch}")
}

fn aad_dek(token: &str) -> String {
    format!("{DOMAIN}|{token}")
}

/// `nonce24 || XChaCha20-Poly1305(key, msg, aad)`.
fn aead_seal(key: &[u8; 32], msg: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg, aad })
        .map_err(|e| format!("seal failed: {e}"))?;
    let mut out = Vec::with_capacity(24 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn aead_open(key: &[u8; 32], blob: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    if blob.len() < 24 {
        return Err("ciphertext too short".into());
    }
    let (nonce, ct) = blob.split_at(24);
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map(Zeroizing::new)
        .map_err(|e| format!("open failed (wrong key/aad or tampered?): {e}"))
}

/// Seal `plaintext` under a fresh random DEK wrapped by `vk`. Pure — the key is
/// a parameter so the crypto is unit-testable without a live keyring.
pub fn seal_with(
    vk: &[u8; 32],
    epoch: u32,
    plaintext: &[u8],
    token: &str,
    kind: &str,
) -> Result<Sealed, String> {
    let mut dek = Zeroizing::new([0u8; 32]);
    rand::thread_rng().fill_bytes(&mut *dek);
    let enc = aead_seal(&dek, plaintext, aad_enc(token, kind, epoch).as_bytes())?;
    let wrapped = aead_seal(vk, &*dek, aad_dek(token).as_bytes())?;
    Ok(Sealed {
        enc: B64.encode(enc),
        dek_wrapped: B64.encode(wrapped),
        vk_epoch: epoch,
    })
}

/// Open a [`Sealed`], trying each `(epoch, key)` in order. The DEK's AAD is
/// epoch-free, so a row wrapped under a retired VK opens as soon as that key is
/// reached — the prev-epoch fallback of §2 "Rotation".
pub fn open_with(
    keys: &[(u32, [u8; 32])],
    sealed: &Sealed,
    token: &str,
    kind: &str,
) -> Result<Zeroizing<Vec<u8>>, String> {
    let wrapped = B64
        .decode(sealed.dek_wrapped.trim())
        .map_err(|e| format!("dek_wrapped base64: {e}"))?;
    let enc = B64
        .decode(sealed.enc.trim())
        .map_err(|e| format!("enc base64: {e}"))?;
    let aad_d = aad_dek(token);
    let aad_e = aad_enc(token, kind, sealed.vk_epoch);
    let mut last_err = "no vault key".to_string();
    for (_epoch, key) in keys {
        let dek_bytes = match aead_open(key, &wrapped, aad_d.as_bytes()) {
            Ok(d) => d,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        let dek = <[u8; 32]>::try_from(dek_bytes.as_slice())
            .map_err(|_| "unwrapped DEK is not 32 bytes".to_string())?;
        let mut dek = Zeroizing::new(dek);
        let out = aead_open(&dek, &enc, aad_e.as_bytes());
        dek.zeroize();
        return out;
    }
    Err(last_err)
}

/// Seal arbitrary bytes under the node's CURRENT vault key. `None` without a VK.
///
/// Shared with the `document_raw` at-rest envelope (P1 item 5): same helper,
/// same AAD discipline, `token` = the row id, `kind` = the payload class.
pub fn seal(plaintext: &[u8], token: &str, kind: &str) -> Option<Sealed> {
    let vk = keyring::vault_key()?;
    match seal_with(&vk, vk_epoch(), plaintext, token, kind) {
        Ok(s) => Some(s),
        Err(e) => {
            warn!("[pii_vault] seal of {kind} failed: {e}");
            None
        }
    }
}

/// Open a [`Sealed`] with the node's current + retired vault keys.
pub fn open(sealed: &Sealed, token: &str, kind: &str) -> Option<Zeroizing<Vec<u8>>> {
    let keys = unwrap_keys();
    if keys.is_empty() {
        return None;
    }
    match open_with(&keys, sealed, token, kind) {
        Ok(v) => Some(v),
        Err(e) => {
            warn!("[pii_vault] open of {kind} failed: {e}");
            None
        }
    }
}

// ─── clear-or-token (the transition helper) ─────────────────────────────────

/// How a stored PII field reads right now. P1 writes tokens; every row written
/// before it still holds clear — one helper decides which, by shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field<'a> {
    /// Empty / absent.
    Empty,
    /// A legacy clear value (pre-P1 row, or a node with no vault key).
    Clear(&'a str),
    /// A PPRL token — the clear lives in the vault (or nowhere, if shredded).
    Token(&'a str),
}

impl<'a> Field<'a> {
    /// The string as stored — what a NON-operator caller is allowed to see.
    /// A token is already the masked form, so this is safe by construction on
    /// vaulted rows and unchanged on legacy ones.
    pub fn as_str(self) -> &'a str {
        match self {
            Field::Empty => "",
            Field::Clear(s) | Field::Token(s) => s,
        }
    }

    pub fn token(self) -> Option<&'a str> {
        match self {
            Field::Token(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_token(self) -> bool {
        matches!(self, Field::Token(_))
    }
}

/// Classify one stored PII field. THE read-path helper of P1 item 2 — call it
/// instead of testing for tokens ad hoc, so the eventual P1b drop of the clear
/// columns is a one-line change here.
pub fn clear_or_token(field: &str) -> Field<'_> {
    let t = field.trim();
    if t.is_empty() {
        return Field::Empty;
    }
    if parse_pii_token(t).is_some() {
        Field::Token(t)
    } else {
        Field::Clear(t)
    }
}

/// True when `s` is exactly one PPRL token (so an import must not re-tokenise it).
pub fn is_token(s: &str) -> bool {
    clear_or_token(s).is_token()
}

/// What to print for `document.meta.customer` without a reveal: the value as
/// stored — clear on a legacy row, the token on a vaulted one. Operator surfaces
/// resolve the token through [`get`] / [`get_many`] behind the audited reveal
/// gate instead of calling this.
pub fn customer_display(meta: &Value) -> String {
    meta_field_display(meta, "customer")
}

/// [`customer_display`] for any `meta` PII field (`email`, `phone`, `address`, …).
pub fn meta_field_display(meta: &Value, field: &str) -> String {
    meta.get(field)
        .and_then(|v| v.as_str())
        .map(|s| clear_or_token(s).as_str().to_string())
        .unwrap_or_default()
}

/// Every PPRL token sitting in the PII fields of a `document.meta` — the input
/// to a vault-first reveal.
pub fn meta_tokens(meta: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for field in ["customer", "email", "phone", "address", "company"] {
        if let Some(t) = meta.get(field).and_then(|v| v.as_str()) {
            if let Field::Token(t) = clear_or_token(t) {
                out.push(t.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// ─── row plumbing ───────────────────────────────────────────────────────────

fn record_id(token: &str) -> String {
    // Tokens are `[A-Za-z]+_[0-9A-F]{16}` — a bare SurrealDB ident, no
    // backticks needed. `type::record($rid)` is the house pattern (Rule 19).
    format!("{TABLE}:{token}")
}

fn sealed_from_row(row: &Value) -> Option<Sealed> {
    Some(Sealed {
        enc: row.get("enc")?.as_str()?.to_string(),
        dek_wrapped: row.get("dek_wrapped")?.as_str()?.to_string(),
        vk_epoch: row.get("vk_epoch").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
    })
}

async fn read_row(db: &SurrealDb, token: &str) -> Option<Value> {
    let row: Option<Value> = db
        .query("SELECT kind, enc, dek_wrapped, vk_epoch, pp_epoch, first_seen_at, last_seen_at, _vclock FROM type::record($rid)")
        .bind(("rid", record_id(token)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or(None);
    row
}

/// The fuzzy signature stored next to a row — the search property that replaces
/// the clear scan of `meta.customer` once P1b drops the clear columns.
fn fuzzy_for(kind: Kind, clear: &Clear) -> Value {
    match kind.sig_kind() {
        Some(SigKind::Address) => {
            let mut comps: Vec<&str> = vec![clear.value.trim()];
            for k in ["zip", "city", "street"] {
                if let Some(v) = clear.str_extra(k) {
                    comps.push(v);
                }
            }
            serde_json::to_value(pii_fuzzy_sig_address(&comps)).unwrap_or(Value::Null)
        }
        Some(SigKind::Name) => {
            serde_json::to_value(pii_fuzzy_sig_kind(clear.value.trim(), SigKind::Name))
                .unwrap_or(Value::Null)
        }
        None => Value::Null,
    }
}

// ─── the API ────────────────────────────────────────────────────────────────

/// Mint the token for `value` and park the clear in the vault.
///
/// Returns the token, or `None` when the node has no vault key (the caller then
/// keeps the clear value — that is the pre-P1 behaviour, unchanged) or the value
/// is too short to be PII.
///
/// **Idempotent**: the token is a deterministic function of the value, so a
/// re-import of the same ticket hits the same row. Nothing is rewritten unless
/// the row is missing or its `vk_epoch` / `pp_epoch` no longer match the node's
/// — a ticket re-imported hourly costs zero writes.
pub async fn put(db: &SurrealDb, instance_id: &str, kind: Kind, value: &str) -> Option<String> {
    put_clear(db, instance_id, kind, &Clear::new(value)).await
}

/// [`put`] with the structured sidecar fields of §3.1.
pub async fn put_clear(
    db: &SurrealDb,
    instance_id: &str,
    kind: Kind,
    clear: &Clear,
) -> Option<String> {
    if !enabled() {
        return None;
    }
    let value = clear.value.trim();
    if value.chars().count() < MIN_VALUE_LEN || is_token(value) {
        return None;
    }
    let token = obfuscate_pii(value, kind.label());
    let (vke, ppe) = (vk_epoch(), pp_epoch());

    let existing = read_row(db, &token).await;
    if let Some(row) = &existing {
        let same_epochs = row.get("vk_epoch").and_then(|v| v.as_u64()).unwrap_or(u64::MAX) == vke as u64
            && row.get("pp_epoch").and_then(|v| v.as_u64()).unwrap_or(u64::MAX) == ppe as u64;
        let has_ct = row.get("enc").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty());
        if same_epochs && has_ct {
            touch_last_seen(db, &token, row).await;
            return Some(token);
        }
    }

    let plaintext = Zeroizing::new(serde_json::to_vec(clear).ok()?);
    let sealed = match seal(&plaintext, &token, kind.label()) {
        Some(s) => s,
        None => return None,
    };

    let first_seen = existing
        .as_ref()
        .and_then(|r| r.get("first_seen_at"))
        .cloned()
        .filter(|v| !v.is_null());
    let vclock = crate::sync::conflict::next_local_vclock(
        existing.as_ref().and_then(|r| r.get("_vclock")),
        instance_id,
    );

    let res = db
        .query(
            "UPSERT type::record($rid) CONTENT { \
                 kind: $kind, \
                 enc: $enc, \
                 dek_wrapped: $dek, \
                 vk_epoch: $vke, \
                 pp_epoch: $ppe, \
                 fuzzy: $fuzzy, \
                 first_seen_at: $first, \
                 last_seen_at: time::now(), \
                 home_instance_id: $iid, \
                 _vclock: $vclock, \
                 updated_at: time::now() \
             }",
        )
        .bind(("rid", record_id(&token)))
        .bind(("kind", kind.label().to_string()))
        .bind(("enc", sealed.enc))
        .bind(("dek", sealed.dek_wrapped))
        .bind(("vke", vke))
        .bind(("ppe", ppe))
        .bind(("fuzzy", fuzzy_for(kind, clear)))
        .bind(("first", first_seen.unwrap_or(json!(chrono::Utc::now().to_rfc3339()))))
        .bind(("iid", instance_id.to_string()))
        .bind(("vclock", vclock))
        .await
        .and_then(|mut r| r.take::<Option<Value>>(0));

    match res {
        Ok(_) => Some(token),
        Err(e) => {
            // Never let a vault write break an import: the caller keeps the
            // clear value, exactly as on a node without a VK.
            warn!("[pii_vault] put({}) failed: {e}", kind.label());
            None
        }
    }
}

/// Bump `last_seen_at` at most once a day. The field is in
/// `merkle::IGNORED_FIELDS`, so this never drives a cross-node re-sync.
async fn touch_last_seen(db: &SurrealDb, token: &str, row: &Value) {
    let stale = row
        .get("last_seen_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds() > LAST_SEEN_REFRESH_SECS)
        .unwrap_or(true);
    if !stale {
        return;
    }
    let _ = db
        .query("UPDATE type::record($rid) SET last_seen_at = time::now()")
        .bind(("rid", record_id(token)))
        .await
        .and_then(|mut r| r.take::<Option<Value>>(0));
}

/// Decrypt one token. `None` when the node cannot decrypt (no VK), the row was
/// never written (legacy clear row — the caller falls back to the re-hash
/// reveal) or the row was crypto-shredded by a GDPR erasure.
pub async fn get(db: &SurrealDb, token: &str) -> Option<Clear> {
    if !enabled() {
        return None;
    }
    let kind_label = parse_pii_token(token)?;
    let row = read_row(db, token).await?;
    // Trust the row's own `kind` (the AAD was built from it at seal time); the
    // token prefix is only the routing hint.
    let kind = row
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or(kind_label)
        .to_string();
    let sealed = sealed_from_row(&row)?;
    let plaintext = open(&sealed, token, &kind)?;
    match serde_json::from_slice::<Clear>(&plaintext) {
        Ok(c) => Some(c),
        Err(e) => {
            warn!("[pii_vault] get({kind}) plaintext is not a Clear: {e}");
            None
        }
    }
}

/// Batch [`get`] — `token -> clear value`. Tokens with no readable row are
/// simply absent from the map (the caller falls back to the re-hash dictionary).
pub async fn get_many(db: &SurrealDb, tokens: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if !enabled() {
        return out;
    }
    let mut seen: Vec<&String> = Vec::new();
    for t in tokens.iter().take(MAX_GET_MANY) {
        if seen.contains(&t) {
            continue;
        }
        seen.push(t);
        if let Some(clear) = get(db, t).await {
            let v = clear.value.trim().to_string();
            if !v.is_empty() {
                out.insert(t.clone(), v);
            }
        }
    }
    debug!("[pii_vault] get_many resolved {}/{}", out.len(), tokens.len().min(MAX_GET_MANY));
    out
}

/// Crypto-shred: delete the row. The token stays everywhere else as an
/// anonymous pseudonym that no longer resolves to a person — which is what
/// makes GDPR Art. 17 satisfiable without rewriting the business records
/// (§3.1). The delete tombstones through the normal mesh path, so a peer
/// `operator` node drops its copy too.
pub async fn shred(db: &SurrealDb, token: &str) -> bool {
    if !enabled() {
        return false;
    }
    match db
        .query("DELETE type::record($rid)")
        .bind(("rid", record_id(token)))
        .await
        .and_then(|mut r| r.take::<Vec<Value>>(0))
    {
        Ok(_) => true,
        Err(e) => {
            warn!("[pii_vault] shred failed: {e}");
            false
        }
    }
}

/// Re-wrap one row's DEK under the CURRENT vault key without touching the
/// ciphertext — the unit of the P1b VK-rotation sweep (§2 "Rotation": a VK bump
/// is a rewrap, never a re-encrypt).
///
/// `new_epoch` must be the node's live VK epoch; passing anything else is a
/// programming error and is refused rather than silently writing a row nobody
/// can open. Returns `Ok(false)` when the row is missing or already current.
pub async fn rewrap(db: &SurrealDb, token: &str, new_epoch: u32) -> Result<bool, String> {
    if !enabled() {
        return Err("no vault key on this node".into());
    }
    let live = vk_epoch();
    if new_epoch != live {
        return Err(format!("rewrap target epoch {new_epoch} != live VK epoch {live}"));
    }
    let row = match read_row(db, token).await {
        Some(r) => r,
        None => return Ok(false),
    };
    let sealed = sealed_from_row(&row).ok_or_else(|| "row has no envelope".to_string())?;
    if sealed.vk_epoch == new_epoch {
        return Ok(false);
    }
    let kind = row.get("kind").and_then(|v| v.as_str()).unwrap_or("Name").to_string();
    let plaintext = open(&sealed, token, &kind).ok_or_else(|| "cannot open row".to_string())?;
    let vk = keyring::vault_key().ok_or_else(|| "no vault key".to_string())?;
    // Re-seal rather than re-wrap the same DEK: the `enc` AAD binds vk_epoch,
    // so the ciphertext has to be re-produced under the new epoch anyway. A
    // fresh DEK per rotation is strictly better than reusing the old one.
    let resealed = seal_with(&vk, new_epoch, &plaintext, token, &kind)?;
    db.query(
        "UPDATE type::record($rid) SET enc = $enc, dek_wrapped = $dek, vk_epoch = $vke, updated_at = time::now()",
    )
    .bind(("rid", record_id(token)))
    .bind(("enc", resealed.enc))
    .bind(("dek", resealed.dek_wrapped))
    .bind(("vke", new_epoch))
    .await
    .and_then(|mut r| r.take::<Option<Value>>(0))
    .map_err(|e| e.to_string())?;
    Ok(true)
}

// ─── import-side convenience ────────────────────────────────────────────────

/// Should this `company` string be vaulted? §3.2: a company name stays clear
/// (it is business data) **unless** it is really a person's name in the company
/// field — the same heuristic the masker uses.
pub fn company_is_person(company: &str) -> bool {
    plausible_person_name(company.trim())
}

/// Tokenise one clear field in place: replace `obj[field]` with its token and
/// park the clear in the vault. No-op without a VK, on an empty field, or when
/// the field already holds a token. Returns the token when one was written.
pub async fn tokenize_field(
    db: &SurrealDb,
    instance_id: &str,
    obj: &mut Value,
    field: &str,
    kind: Kind,
) -> Option<String> {
    let clear = obj.get(field).and_then(|v| v.as_str())?.trim().to_string();
    if clear.is_empty() || is_token(&clear) {
        return None;
    }
    let token = put(db, instance_id, kind, &clear).await?;
    obj[field] = json!(token);
    Some(token)
}

// ─── document_raw at rest (P1 item 5 — primitive only, see the note) ────────

/// AEAD `kind` tag for a sealed `document_raw.payload`.
pub const RAW_KIND: &str = "document_raw";

/// Field names of the sealed-payload envelope on a `document_raw` row.
pub const RAW_ENC: &str = "payload_enc";
pub const RAW_DEK: &str = "payload_dek";
pub const RAW_EPOCH: &str = "payload_vk_epoch";

/// Opt-in for writing NEW `document_raw.payload` rows sealed at rest
/// (`ECK_RAW_AT_REST=1` **and** a vault key).
///
/// **Default OFF, and deliberately so.** `.eck/DATA_AT_REST.md` (P0.6) already
/// puts the kiosk's entire `data/` — SurrealKV internals, CAS blobs, backups —
/// on a gocryptfs volume whose key is not on the disk it opens, so a sealed
/// `payload` column buys defence in depth on *other* full nodes rather than
/// closing a live hole. What it costs is the ~20 read sites that project
/// SurrealQL sub-fields out of `document_raw.payload` (`payload.assignee`,
/// `payload.createdTime`, `payload.subject`, `payload.customFields`, …): every
/// one of them has to move to "select the row, decrypt, read the field in
/// Rust". That conversion is the SAME sweep as the P1b backfill of the
/// already-stored clear payloads, so both are done together in P1b — see
/// `.eck/TECH_DEBT.md`. Until then this flag must stay unset in production;
/// the primitives below are shipped, tested and unwired.
pub fn raw_at_rest_enabled() -> bool {
    enabled() && std::env::var("ECK_RAW_AT_REST").map(|v| v == "1").unwrap_or(false)
}

/// Seal a `document_raw.payload` for storage. `row_id` is the bare record id
/// (the Zoho ticket / thread id), which becomes the AAD binding — a sealed
/// payload cannot be moved onto another row.
pub fn seal_raw(row_id: &str, payload: &Value) -> Option<Sealed> {
    let pt = Zeroizing::new(serde_json::to_vec(payload).ok()?);
    seal(&pt, row_id, RAW_KIND)
}

/// Transparently decrypt a `document_raw` row read back from the DB: when it
/// carries the sealed envelope, replace `row["payload"]` with the plaintext and
/// drop the envelope fields. Returns `true` when a decrypt happened.
///
/// A row that was never sealed (every row today) is left exactly as it is, so
/// callers can apply this unconditionally.
pub fn open_raw(row: &mut Value, row_id: &str) -> bool {
    let sealed = {
        let enc = row.get(RAW_ENC).and_then(|v| v.as_str()).unwrap_or("");
        let dek = row.get(RAW_DEK).and_then(|v| v.as_str()).unwrap_or("");
        if enc.is_empty() || dek.is_empty() {
            return false;
        }
        Sealed {
            enc: enc.to_string(),
            dek_wrapped: dek.to_string(),
            vk_epoch: row.get(RAW_EPOCH).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        }
    };
    let Some(pt) = open(&sealed, row_id, RAW_KIND) else {
        // No key / wrong key: leave the row sealed rather than blanking it. The
        // caller sees no `payload`, which is the same shape a thin node with no
        // local raw copy presents — a degradation every reader already handles.
        return false;
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&pt) else {
        warn!("[pii_vault] sealed document_raw payload is not JSON");
        return false;
    };
    if let Some(obj) = row.as_object_mut() {
        obj.insert("payload".to_string(), payload);
        obj.remove(RAW_ENC);
        obj.remove(RAW_DEK);
        obj.remove(RAW_EPOCH);
    }
    true
}

/// The `pii_vault` table definition. Idempotent — safe to run on every boot.
pub fn schema_sql() -> &'static str {
    "DEFINE TABLE IF NOT EXISTS pii_vault SCHEMALESS;"
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn put_get_roundtrip_through_the_envelope() {
        let vk = key(1);
        let clear = Clear::new("Max Mustermann");
        let pt = serde_json::to_vec(&clear).unwrap();
        let sealed = seal_with(&vk, 3, &pt, "Name_0123456789ABCDEF", "Name").unwrap();
        assert_eq!(sealed.vk_epoch, 3);
        // The ciphertext must not carry the plaintext.
        assert!(!sealed.enc.contains("Mustermann"));

        let out = open_with(&[(3, vk)], &sealed, "Name_0123456789ABCDEF", "Name").unwrap();
        let back: Clear = serde_json::from_slice(&out).unwrap();
        assert_eq!(back.value, "Max Mustermann");
    }

    #[test]
    fn clear_extras_survive_the_roundtrip() {
        let vk = key(9);
        let clear = Clear::new("Hauptstr. 5")
            .with("zip", "12345")
            .with("city", "Berlin");
        let pt = serde_json::to_vec(&clear).unwrap();
        let sealed = seal_with(&vk, 0, &pt, "Address_00000000000000FF", "Address").unwrap();
        let out = open_with(&[(0, vk)], &sealed, "Address_00000000000000FF", "Address").unwrap();
        let back: Clear = serde_json::from_slice(&out).unwrap();
        assert_eq!(back.value, "Hauptstr. 5");
        assert_eq!(back.extra.get("zip").unwrap(), "12345");
        assert_eq!(back.extra.get("city").unwrap(), "Berlin");
    }

    #[test]
    fn aad_binds_the_row_to_token_kind_and_epoch() {
        let vk = key(2);
        let pt = b"{\"value\":\"Erika\"}";
        let sealed = seal_with(&vk, 1, pt, "Name_AAAAAAAAAAAAAAAA", "Name").unwrap();

        // Swapped under another token: the DEK unwrap already fails.
        assert!(open_with(&[(1, vk)], &sealed, "Name_BBBBBBBBBBBBBBBB", "Name").is_err());
        // Same token, wrong kind: the payload AAD fails.
        assert!(open_with(&[(1, vk)], &sealed, "Name_AAAAAAAAAAAAAAAA", "Email").is_err());
        // Same token+kind, but the stored epoch claims something else.
        let lied = Sealed { vk_epoch: 2, ..sealed.clone() };
        assert!(open_with(&[(1, vk)], &lied, "Name_AAAAAAAAAAAAAAAA", "Name").is_err());
        // Untouched: opens.
        assert!(open_with(&[(1, vk)], &sealed, "Name_AAAAAAAAAAAAAAAA", "Name").is_ok());
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        let vk = key(3);
        let sealed = seal_with(&vk, 0, b"{\"value\":\"Erika\"}", "Name_CCCCCCCCCCCCCCCC", "Name").unwrap();
        let mut raw = B64.decode(&sealed.enc).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered = Sealed { enc: B64.encode(raw), ..sealed };
        assert!(open_with(&[(0, vk)], &tampered, "Name_CCCCCCCCCCCCCCCC", "Name").is_err());
    }

    #[test]
    fn prev_epoch_key_opens_a_not_yet_rewrapped_row() {
        let old = key(4);
        let new = key(5);
        let sealed = seal_with(&old, 1, b"{\"value\":\"Erika\"}", "Name_DDDDDDDDDDDDDDDD", "Name").unwrap();
        // Current key alone cannot open it...
        assert!(open_with(&[(2, new)], &sealed, "Name_DDDDDDDDDDDDDDDD", "Name").is_err());
        // ...but the ring's (current, prev) order does.
        let out = open_with(&[(2, new), (1, old)], &sealed, "Name_DDDDDDDDDDDDDDDD", "Name").unwrap();
        assert_eq!(&*out, b"{\"value\":\"Erika\"}");
    }

    #[test]
    fn a_shredded_row_is_simply_unreadable() {
        // `shred` is a row DELETE; the crypto contract that makes it a
        // *crypto*-shred is that the ciphertext is worthless without the DEK,
        // which lives only inside the row.
        let vk = key(6);
        let sealed = seal_with(&vk, 0, b"{\"value\":\"Erika\"}", "Name_EEEEEEEEEEEEEEEE", "Name").unwrap();
        let orphan = Sealed { dek_wrapped: String::new(), ..sealed };
        assert!(open_with(&[(0, vk)], &orphan, "Name_EEEEEEEEEEEEEEEE", "Name").is_err());
    }

    #[test]
    fn nonces_are_fresh_per_seal() {
        let vk = key(7);
        let a = seal_with(&vk, 0, b"x", "Name_FFFFFFFFFFFFFFFF", "Name").unwrap();
        let b = seal_with(&vk, 0, b"x", "Name_FFFFFFFFFFFFFFFF", "Name").unwrap();
        assert_ne!(a.enc, b.enc);
        assert_ne!(a.dek_wrapped, b.dek_wrapped);
    }

    #[test]
    fn clear_or_token_classifies_by_shape() {
        assert_eq!(clear_or_token("Name_8E5F3A1B00000000"), Field::Token("Name_8E5F3A1B00000000"));
        assert_eq!(clear_or_token("Email_0123456789ABCDEF"), Field::Token("Email_0123456789ABCDEF"));
        assert_eq!(clear_or_token("  Max Mustermann "), Field::Clear("Max Mustermann"));
        assert_eq!(clear_or_token(""), Field::Empty);
        assert_eq!(clear_or_token("   "), Field::Empty);
        // A token plus prose is NOT a token field — it is free text.
        assert!(!is_token("Kunde Name_8E5F3A1B00000000 meldet"));
        // Lowercase hex / wrong width are not tokens.
        assert!(!is_token("Name_8e5f3a1b00000000"));
        assert!(!is_token("Name_8E5F"));
    }

    #[test]
    fn field_as_str_never_leaks_more_than_stored() {
        assert_eq!(clear_or_token("Name_8E5F3A1B00000000").as_str(), "Name_8E5F3A1B00000000");
        assert_eq!(clear_or_token("Max").as_str(), "Max");
        assert_eq!(clear_or_token("").as_str(), "");
    }

    #[test]
    fn meta_helpers_read_both_shapes() {
        let vaulted = json!({ "customer": "Name_8E5F3A1B00000000", "email": "Email_0123456789ABCDEF", "city": "Berlin" });
        let legacy = json!({ "customer": "Max Mustermann", "email": "max@example.com" });
        assert_eq!(customer_display(&vaulted), "Name_8E5F3A1B00000000");
        assert_eq!(customer_display(&legacy), "Max Mustermann");
        assert_eq!(
            meta_tokens(&vaulted),
            vec!["Email_0123456789ABCDEF".to_string(), "Name_8E5F3A1B00000000".to_string()]
        );
        assert!(meta_tokens(&legacy).is_empty());
    }

    #[test]
    fn kind_labels_round_trip() {
        for k in [Kind::Name, Kind::Email, Kind::Phone, Kind::Address, Kind::Company] {
            assert_eq!(Kind::from_label(k.label()), Some(k));
        }
        assert_eq!(Kind::from_label("Iban"), None);
    }

    #[test]
    fn raw_envelope_round_trips_and_is_bound_to_its_row() {
        let vk = key(8);
        let payload = json!({ "subject": "Gerät defekt", "contact": { "email": "max@example.com" } });
        let pt = serde_json::to_vec(&payload).unwrap();
        let sealed = seal_with(&vk, 0, &pt, "53451000034588248", RAW_KIND).unwrap();
        assert!(!sealed.enc.contains("max@example.com"));

        assert_eq!((RAW_ENC, RAW_DEK, RAW_EPOCH), ("payload_enc", "payload_dek", "payload_vk_epoch"));
        let mut row = json!({
            "payload_enc": sealed.enc,
            "payload_dek": sealed.dek_wrapped,
            "payload_vk_epoch": sealed.vk_epoch,
        });
        // `open_raw` goes through the live keyring, which has no VK in a unit
        // test — it must therefore leave the row untouched rather than blank
        // the payload. That degradation path is the one that matters here.
        assert!(!open_raw(&mut row, "53451000034588248"));
        assert!(row.get("payload").is_none());
        assert!(row.get(RAW_ENC).is_some());

        // The crypto itself, with the key supplied explicitly.
        let out = open_with(&[(0, vk)], &Sealed {
            enc: row[RAW_ENC].as_str().unwrap().to_string(),
            dek_wrapped: row[RAW_DEK].as_str().unwrap().to_string(),
            vk_epoch: 0,
        }, "53451000034588248", RAW_KIND).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), payload);
    }

    #[test]
    fn an_unsealed_row_passes_through_open_raw_untouched() {
        let mut row = json!({ "payload": { "subject": "x" }, "type": "support_ticket" });
        let before = row.clone();
        assert!(!open_raw(&mut row, "123"));
        assert_eq!(row, before);
    }

    #[test]
    fn disabled_without_a_vault_key() {
        // The whole point of the gate: a dev node with no VK behaves exactly
        // as it did before this module existed.
        assert!(!enabled());
    }
}
