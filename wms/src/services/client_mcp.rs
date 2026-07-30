//! Shared admission + execution for a single **signed** client-MCP request.
//!
//! ONE gate, TWO transports:
//!   * the relay poller ([`super::client_mcp_poller`]) pulls `SignedClientMcp`
//!     payloads out of the relay's `/E/c/*` queue, and
//!   * the direct HTTP ingress (`POST /mcp/signed`, see [`crate::mcp`]) accepts
//!     the exact same `SignedClientMcp` straight from a LAN / public client.
//!
//! Both hand the raw payload to [`serve_signed`], which re-verifies the
//! `SubscriptionCert` against our own `ECK_SUB_ROOT_PUBKEY` (defense in depth on
//! the relay path; the SOLE gate on the direct path), enforces the `cmcp:`
//! nonce replay guard in the shared `xelixir_nonce` table, maps the cert's tier
//! to an [`McpTier`], and runs the JSON-RPC through the same
//! `mcp::handle_jsonrpc(.., over_relay = true)` the relay path uses.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};
use tracing::warn;

use eck_core::xelixir::envelope::DEFAULT_MAX_AGE_SECS;
use eck_core::xelixir::subscription::{
    read_sub_root_from_env, SignedClientMcp, DEFAULT_GRACE_SECS,
};

use crate::mcp::{handle_jsonrpc, McpTier};
use crate::AppState;

// Reuse the same nonce TTL policy as the xelixir poller: twice the signature
// validity window. Client nonces are namespaced (`cmcp:`) so they can't collide
// with ops-envelope nonces in the shared `xelixir_nonce` table.
pub(crate) const NONCE_TTL_SECS: u64 = (DEFAULT_MAX_AGE_SECS as u64) * 2;

/// The outcome of admitting + serving one signed client-MCP request. The
/// variants map cleanly onto BOTH transports: HTTP status codes for the direct
/// `/mcp/signed` ingress, and ack bodies for the relay poller.
#[derive(Debug)]
pub(crate) enum SignedMcpOutcome {
    /// `ECK_SUB_ROOT_PUBKEY` unset — signed ingress is disabled (fail closed).
    Disabled,
    /// Payload didn't parse as a `SignedClientMcp`.
    Malformed(String),
    /// `admit()` failed: bad signature / expired / wrong target / node not
    /// covered / feature not granted.
    Rejected(String),
    /// Nonce already seen within its TTL — a replay.
    Replay,
    /// Nonce store unavailable — we couldn't prove freshness, so we refuse.
    NonceStoreError(String),
    /// Served: the JSON-RPC response body (or an `accepted` marker for a
    /// notification, which has no response).
    Served(Value),
}

impl SignedMcpOutcome {
    /// HTTP status for the direct `/mcp/signed` ingress. **Pure** — this is the
    /// decision logic the unit test exercises without standing up an
    /// (expensive) `AppState`.
    pub(crate) fn http_status(&self) -> StatusCode {
        match self {
            SignedMcpOutcome::Served(_) => StatusCode::OK,
            SignedMcpOutcome::Disabled => StatusCode::SERVICE_UNAVAILABLE,
            SignedMcpOutcome::Malformed(_) => StatusCode::BAD_REQUEST,
            // Bad cert and replay both deny with 403; the message distinguishes.
            SignedMcpOutcome::Rejected(_) | SignedMcpOutcome::Replay => StatusCode::FORBIDDEN,
            SignedMcpOutcome::NonceStoreError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The ack body the relay poller returns for this outcome (an MCP result on
    /// success, an `{"error": …}` marker on any rejection). Keeps the poller and
    /// the direct handler deriving their responses from the SAME outcome.
    pub(crate) fn into_ack_body(self) -> Value {
        match self {
            SignedMcpOutcome::Served(body) => body,
            SignedMcpOutcome::Disabled => json!({"error": "signed MCP ingress disabled"}),
            SignedMcpOutcome::Malformed(e) => json!({"error": format!("invalid payload: {e}")}),
            SignedMcpOutcome::Rejected(e) => json!({"error": e}),
            SignedMcpOutcome::Replay => json!({"error": "duplicate nonce"}),
            SignedMcpOutcome::NonceStoreError(e) => {
                json!({"error": format!("nonce store unavailable: {e}")})
            }
        }
    }
}

/// Admit and serve ONE signed client-MCP request. Shared by the relay poller
/// and the direct `POST /mcp/signed` ingress so both run the identical gate.
///
/// `over_relay` is passed **TRUE** to `handle_jsonrpc` on BOTH transports on
/// purpose: capability follows the *credential* (the `SubscriptionCert`), not
/// the wire it arrived on. A signed request must have `surrealql_read` refused
/// AND hidden whether it came via the relay or straight to `/mcp/signed`.
pub(crate) async fn serve_signed(state: &Arc<AppState>, payload: Value) -> SignedMcpOutcome {
    // Fail closed: no configured subscription root ⇒ ingress is off, and no cert
    // could verify anyway. Never silently downgrade to an open pipe.
    let Some(sub_root) = read_sub_root_from_env() else {
        return SignedMcpOutcome::Disabled;
    };

    let signed: SignedClientMcp = match serde_json::from_value(payload) {
        Ok(s) => s,
        Err(e) => return SignedMcpOutcome::Malformed(e.to_string()),
    };

    // Full admission against our OWN root. On the relay path this is defense in
    // depth (a compromised relay can neither forge access nor escalate the
    // tier); on the direct path it is the only gate.
    let now = chrono::Utc::now().timestamp();
    let cert = match signed.admit(
        &sub_root,
        &state.instance_id,
        now,
        DEFAULT_MAX_AGE_SECS,
        DEFAULT_GRACE_SECS,
    ) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "[client_mcp] reject signed request signer={}: {}",
                signed.signer_pubkey, e
            );
            return SignedMcpOutcome::Rejected(e.to_string());
        }
    };

    // Replay guard — namespaced (`cmcp:`) so it can't collide with ops nonces.
    let nonce_key = format!("cmcp:{}", signed.request.nonce);
    match nonce_seen_and_record(&state.db, &nonce_key, NONCE_TTL_SECS).await {
        Ok(true) => return SignedMcpOutcome::Replay,
        Ok(false) => {}
        Err(e) => return SignedMcpOutcome::NonceStoreError(e),
    }

    let tier = if cert.grants_master() {
        McpTier::Master
    } else {
        McpTier::Agent
    };

    // over_relay = TRUE even on the direct ingress — see the doc comment above:
    // capability follows the credential, not the transport.
    match handle_jsonrpc(state, tier, &signed.request.mcp, true).await {
        // Notifications produce no response; return an explicit accepted marker.
        None => SignedMcpOutcome::Served(json!({"jsonrpc": "2.0", "accepted": true})),
        Some(resp) => SignedMcpOutcome::Served(resp),
    }
}

/// `Ok(true)` = nonce already recorded within its TTL (replay); `Ok(false)` =
/// newly inserted OR a legitimately-recycled nonce whose previous TTL had
/// already lapsed.
///
/// INSERT-first, and the DATABASE (not this code) enforces uniqueness: the
/// `xelixir_nonce_unique` index means two concurrent requests bearing the same
/// nonce can't both win — exactly one INSERT commits, the rest fail the index.
/// This closes the check-then-insert race the old SELECT-then-INSERT left open
/// once the direct `/mcp/signed` HTTP ingress started calling this concurrently
/// (the relay poller was single-task, so the race was network-only before).
pub(crate) async fn nonce_seen_and_record(
    db: &eck_core::db::SurrealDb,
    nonce: &str,
    ttl_secs: u64,
) -> Result<bool, String> {
    // A unique-index violation can surface either at `.await` (Err) or as a
    // per-statement error inside an Ok(Response) that `.check()` promotes — v3
    // does both depending on path, so normalize to a single error string.
    let insert_err: String = match db
        .query(
            "INSERT INTO xelixir_nonce { \
                nonce: $n, \
                expires_at: time::now() + type::duration($ttl) \
             }",
        )
        .bind(("n", nonce.to_string()))
        .bind(("ttl", format!("{}s", ttl_secs)))
        .await
    {
        Ok(resp) => match resp.check() {
            Ok(_) => return Ok(false),
            Err(e) => e.to_string(),
        },
        Err(e) => e.to_string(),
    };

    // Was it the unique index, or a genuine DB fault? Match a STABLE substring
    // of SurrealDB's index-violation message (verified in the tests below), not
    // the full sentence, so a wording change across versions won't silently
    // reopen the race. Anything else is a real error — surface it unchanged.
    if !is_unique_violation(&insert_err) {
        return Err(insert_err);
    }

    // The nonce row already exists. Decide replay vs. legitimate reuse ENTIRELY
    // in the DB so the `time::now()` comparison is atomic with the row it reads:
    //   * live (unexpired) row  ⇒ WHERE matches nothing ⇒ empty ⇒ replay.
    //   * expired-but-not-yet-GC'd row ⇒ we reclaim it (refresh the TTL) and
    //     admit — a fresh use of a recycled nonce after its TTL is legitimate.
    // (Extreme edge: if the GC pruner deletes the expired row between our failed
    // INSERT and this UPDATE, we admit nothing and report replay — fail-closed,
    // the client simply retries with a new nonce.)
    let refreshed: Vec<Value> = db
        .query(
            "UPDATE xelixir_nonce \
                SET expires_at = time::now() + type::duration($ttl) \
                WHERE nonce = $n AND expires_at <= time::now() \
                RETURN AFTER",
        )
        .bind(("n", nonce.to_string()))
        .bind(("ttl", format!("{}s", ttl_secs)))
        .await
        .map_err(|e| e.to_string())?
        .take(0)
        .map_err(|e| e.to_string())?;

    Ok(refreshed.is_empty())
}

/// Recognize a SurrealDB unique-index violation from its error text. Kept as a
/// narrow substring match (verified empirically in the tests) rather than a
/// full-sentence comparison so version wording drift can't reopen the race.
fn is_unique_violation(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("already contains") || e.contains("index")
}

#[cfg(test)]
mod tests {
    use super::*;

    // The full serve path needs an AppState (SyncEngine, AgentController,
    // dual embedded DBs, ServerIdentity) — too heavy for a unit test. The
    // admit()/nonce/over_relay behavior is already covered by
    // `subscription.rs`'s own tests and the relay integration test. What is
    // NEW and worth pinning here is the direct-ingress STATUS MAPPING: it is
    // pure, so we test it directly.
    #[test]
    fn outcome_maps_to_expected_http_status() {
        assert_eq!(
            SignedMcpOutcome::Served(json!({"jsonrpc": "2.0", "id": 1, "result": {}}))
                .http_status(),
            StatusCode::OK,
        );
        assert_eq!(SignedMcpOutcome::Disabled.http_status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            SignedMcpOutcome::Malformed("bad json".into()).http_status(),
            StatusCode::BAD_REQUEST,
        );
        // Forged root / expired / wrong target all surface as Rejected → 403.
        assert_eq!(
            SignedMcpOutcome::Rejected("bad signature".into()).http_status(),
            StatusCode::FORBIDDEN,
        );
        // A replayed nonce is ALSO 403, but a distinct message.
        assert_eq!(SignedMcpOutcome::Replay.http_status(), StatusCode::FORBIDDEN);
        assert_eq!(
            SignedMcpOutcome::NonceStoreError("db down".into()).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    #[test]
    fn ack_body_carries_error_marker_for_rejections() {
        // Poller-side: every rejection acks an `{"error": …}` marker so the
        // relay row completes and the client sees a failure, not a hang.
        assert!(SignedMcpOutcome::Disabled.into_ack_body().get("error").is_some());
        assert!(SignedMcpOutcome::Replay.into_ack_body().get("error").is_some());
        assert!(SignedMcpOutcome::Rejected("x".into())
            .into_ack_body()
            .get("error")
            .is_some());
        // Served passes the JSON-RPC body through verbatim.
        let served = SignedMcpOutcome::Served(json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}));
        assert_eq!(served.into_ack_body()["result"]["ok"], json!(true));
    }

    // --- Nonce replay guard (DB-enforced uniqueness) -----------------------
    //
    // These exercise the real `nonce_seen_and_record` against an in-memory
    // SurrealDB (the `Any` engine, `mem://`, matching production's handle type)
    // with the same UNIQUE index `main.rs` defines at boot. No AppState needed —
    // the helper only takes a `&SurrealDb`.

    /// Fresh in-memory DB carrying ONLY the `xelixir_nonce` table + its unique
    /// index — the minimal schema the guard relies on.
    async fn mem_db_with_index() -> eck_core::db::SurrealDb {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("in-memory Any DB");
        db.use_ns("test").use_db("test").await.unwrap();
        db.query(
            "DEFINE TABLE IF NOT EXISTS xelixir_nonce SCHEMALESS; \
             DEFINE INDEX IF NOT EXISTS xelixir_nonce_unique \
                 ON xelixir_nonce FIELDS nonce UNIQUE;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        db
    }

    /// Pin the EXACT error text SurrealDB emits for a unique-index violation, so
    /// `is_unique_violation`'s substring match is grounded in observed behavior,
    /// not a guess. If a future SurrealDB reworded this, this test fails loudly
    /// instead of the guard silently reopening the race.
    #[tokio::test]
    async fn unique_violation_error_shape_is_stable() {
        let db = mem_db_with_index().await;
        db.query("INSERT INTO xelixir_nonce { nonce: 'x', expires_at: time::now() + 1h }")
            .await
            .unwrap()
            .check()
            .unwrap();
        // Same nonce again → must trip the unique index.
        let err: String = match db
            .query("INSERT INTO xelixir_nonce { nonce: 'x', expires_at: time::now() + 1h }")
            .await
        {
            Ok(r) => match r.check() {
                Ok(_) => panic!("duplicate INSERT unexpectedly succeeded"),
                Err(e) => e.to_string(),
            },
            Err(e) => e.to_string(),
        };
        println!("[nonce] duplicate-insert error text: {err}");
        assert!(
            is_unique_violation(&err),
            "unique-index violation not recognized; error was: {err}"
        );
    }

    /// The core race: 8 requests with the SAME nonce hit the guard at once.
    /// Exactly one may be admitted (Ok(false)); the rest must be rejected as
    /// replays (Ok(true)). This is the property the DB unique index guarantees
    /// and the old SELECT-then-INSERT could not.
    #[tokio::test]
    async fn concurrent_same_nonce_admits_exactly_one() {
        let db = mem_db_with_index().await;
        let nonce = "cmcp:race-me";
        let mut handles = Vec::new();
        for _ in 0..8 {
            let db = db.clone();
            handles.push(tokio::spawn(async move {
                nonce_seen_and_record(&db, nonce, 300).await
            }));
        }
        let (mut fresh, mut replay) = (0u32, 0u32);
        for h in handles {
            match h.await.unwrap() {
                Ok(false) => fresh += 1,
                Ok(true) => replay += 1,
                Err(e) => panic!("unexpected nonce store error: {e}"),
            }
        }
        assert_eq!(fresh, 1, "exactly one concurrent caller must be admitted");
        assert_eq!(replay, 7, "every other caller must be rejected as a replay");
    }

    /// A nonce whose TTL already lapsed (row not yet GC'd) is a LEGITIMATE fresh
    /// use, not a replay: the guard reclaims the row and admits it, then treats
    /// the immediately-following call as the replay.
    #[tokio::test]
    async fn expired_nonce_is_reusable() {
        let db = mem_db_with_index().await;
        let nonce = "cmcp:recycle-me";
        // Seed an ALREADY-expired row for this nonce.
        db.query("INSERT INTO xelixir_nonce { nonce: $n, expires_at: time::now() - 1h }")
            .bind(("n", nonce.to_string()))
            .await
            .unwrap()
            .check()
            .unwrap();

        // Expired ⇒ reclaimed ⇒ admitted.
        assert_eq!(
            nonce_seen_and_record(&db, nonce, 300).await,
            Ok(false),
            "an expired nonce must be reusable"
        );
        // Now live again ⇒ genuine replay.
        assert_eq!(
            nonce_seen_and_record(&db, nonce, 300).await,
            Ok(true),
            "the just-refreshed nonce must now read as a replay"
        );
    }
}
