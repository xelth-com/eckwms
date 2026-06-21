//! Cross-NAT mesh task receiver.
//!
//! Polls the relay's `/E/m/poll/<self_uuid>` for tasks addressed to this WMS,
//! interprets the envelope, applies the local effect, and acks with a result
//! body the dispatcher can read. Complementary to the direct-HTTP P2P path:
//! when two peers can't dial each other (different NATs), the sender writes
//! a task to the relay queue and this poller picks it up here.
//!
//! Envelope shape (constructed by `RelayClient::mesh_dispatch`):
//! ```json
//! { "envelope": {
//!     "target_uuid": "<my_uuid>",
//!     "sender_uuid": "<peer_uuid>",
//!     "kind": "pull_request" | "push",
//!     "payload": { ...kind-specific... }
//!   }
//! }
//! ```
//!
//! Currently handled kinds:
//! - `pull_request`    — `{entity_type, ids: [String]}` → ack `{entities: [...]}`
//! - `push`            — `{entity_type, entities: [...], source_instance}`
//!                       → ack `{applied: N}`
//! - `device_register` — `{deviceId, devicePublicKey, signature, inviteToken?}`
//!                       → ack `{status, token}` (phone pairs to a NAT'd master
//!                       through a blind relay; eckN stay pure relays)

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::AppState;

/// Polling cadence. Adaptive: tightens when there's work, relaxes when idle.
/// Driven by `next_poll_in_seconds` hint from the relay (same pattern as the
/// xelixir poller).
const POLL_INTERVAL_BUSY_SECS: u64 = 3;
const POLL_INTERVAL_IDLE_SECS: u64 = 15;

pub async fn start_poller(state: Arc<AppState>) {
    info!(
        "[mesh_relay_poller] starting for instance {}",
        state.instance_id
    );

    let mut interval_secs = POLL_INTERVAL_IDLE_SECS;
    loop {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        let tasks = match state.sync_engine.relay().mesh_poll().await {
            Ok(t) => t,
            Err(e) => {
                debug!("[mesh_relay_poller] poll failed (likely relay transient): {}", e);
                interval_secs = POLL_INTERVAL_IDLE_SECS;
                continue;
            }
        };

        if tasks.is_empty() {
            interval_secs = POLL_INTERVAL_IDLE_SECS;
            continue;
        }

        interval_secs = POLL_INTERVAL_BUSY_SECS;
        for task in tasks {
            handle_task(&state, task).await;
        }
    }
}

async fn handle_task(state: &Arc<AppState>, task: Value) {
    let task_id = task
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if task_id.is_empty() {
        warn!("[mesh_relay_poller] task without id, dropping");
        return;
    }

    // The envelope was wrapped by the dispatcher; unpack.
    let envelope = task.get("payload").and_then(|p| p.get("envelope"));
    let kind = envelope
        .and_then(|e| e.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let sender = envelope
        .and_then(|e| e.get("sender_uuid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let payload = envelope
        .and_then(|e| e.get("payload"))
        .cloned()
        .unwrap_or(Value::Null);

    let ack_body = match kind.as_str() {
        "pull_request" => handle_pull_request(state, &payload).await,
        "push" => handle_push(state, &payload, &sender).await,
        "device_register" => handle_device_register(state, &payload).await,
        other => {
            warn!(
                "[mesh_relay_poller] task={} unknown kind '{}', acking with error",
                task_id, other
            );
            json!({"ok": false, "error": format!("unknown kind: {other}")})
        }
    };

    if let Err(e) = state.sync_engine.relay().mesh_ack(&task_id, ack_body).await {
        warn!("[mesh_relay_poller] ack task={} failed: {}", task_id, e);
    }
}

/// Relay-forwarded device pairing. A phone on mobile data sees only blind
/// relays (no directly-reachable full WMS), so it dispatches its registration
/// as a `device_register` mesh-task targeting this (NAT'd) master's UUID. We
/// run the exact same logic as `POST /api/internal/register-device` and ack
/// `{status, token}`. This is what lets the eckN service nodes stay pure
/// relays — the master pairs the device through the reverse-fetch queue.
///
/// Payload mirrors `DeviceRegisterRequest`:
/// `{deviceId, deviceName?, devicePublicKey, signature, inviteToken?}`.
async fn handle_device_register(state: &Arc<AppState>, payload: &Value) -> Value {
    let req: crate::handlers::device::DeviceRegisterRequest =
        match serde_json::from_value(payload.clone()) {
            Ok(r) => r,
            Err(e) => {
                return json!({"ok": false, "error": format!("bad device_register payload: {e}")})
            }
        };
    let device_id = req.device_id.clone();
    match crate::handlers::device::register_device_core(state, req).await {
        Ok(resp) => {
            info!(
                "[mesh_relay_poller] device_register {} -> status={}",
                device_id, resp.status
            );
            json!({
                "ok": true,
                "success": resp.success,
                "status": resp.status,
                "token": resp.token,
                "enc_key": resp.enc_key,
                "message": resp.message,
            })
        }
        Err((code, msg)) => {
            warn!(
                "[mesh_relay_poller] device_register {} failed ({}): {}",
                device_id, code, msg
            );
            json!({"ok": false, "error": msg, "code": code.as_u16()})
        }
    }
}

async fn handle_pull_request(state: &Arc<AppState>, payload: &Value) -> Value {
    let entity_type = payload
        .get("entity_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let ids: Vec<String> = payload
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if entity_type.is_empty() || ids.is_empty() {
        return json!({"ok": false, "error": "missing entity_type or ids", "entities": []});
    }

    let query = format!(
        "SELECT *, record::id(id) AS id FROM {} WHERE record::id(id) IN $ids",
        entity_type
    );
    let entities: Vec<Value> = match state.db.query(&query).bind(("ids", ids.clone())).await {
        Ok(mut r) => r.take(0).unwrap_or_default(),
        Err(e) => {
            warn!(
                "[mesh_relay_poller] pull_request {} ids={:?} query failed: {}",
                entity_type,
                ids.len(),
                e
            );
            return json!({"ok": false, "error": e.to_string(), "entities": []});
        }
    };

    // Blind-cache invariant (same shared logic as handlers::mesh::sync_pull):
    // an owner (holds MESH_DATA_KEY) encrypts every entity before it leaves over
    // the relay, so the relay and any cache node only ever see ciphertext; the
    // receiver decrypts on arrival if it has the key. A keyless CACHE that
    // shouldn't be fulfilling here must NOT leak plaintext — withhold any row
    // that isn't already a ciphertext envelope.
    let n = entities.len();
    let has_key = eck_core::utils::crypto::data_key();
    let is_cache = state.node_role == "cache";
    let entities = eck_core::utils::crypto::prepare_outbound(entities, has_key, is_cache);
    let withheld = n - entities.len();
    if withheld > 0 {
        warn!(
            "[mesh_relay_poller] blind cache WITHHELD {}/{} {} plaintext rows",
            withheld, n, entity_type
        );
    }

    debug!(
        "[mesh_relay_poller] pull_request served {}/{} {} entities (encrypted={}, withheld={})",
        entities.len(),
        n,
        entity_type,
        has_key.is_some(),
        withheld
    );
    json!({
        "ok": true,
        "entity_type": entity_type,
        "entities": entities,
    })
}

async fn handle_push(state: &Arc<AppState>, payload: &Value, sender: &str) -> Value {
    let entity_type = payload
        .get("entity_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let entities: Vec<Value> = payload
        .get("entities")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // Sender from envelope is authoritative; payload's `source_instance` is a
    // hint from the original caller. Use whichever is non-empty.
    let source = if !sender.is_empty() {
        sender.to_string()
    } else {
        payload
            .get("source_instance")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    };

    if entity_type.is_empty() {
        return json!({"ok": false, "error": "missing entity_type", "applied": 0});
    }

    let applied =
        crate::handlers::mesh::apply_pushed_entities(state, &entity_type, &entities, &source).await;

    json!({
        "ok": true,
        "entity_type": entity_type,
        "applied": applied,
    })
}
