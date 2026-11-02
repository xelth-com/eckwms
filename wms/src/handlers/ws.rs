use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::AppState;

#[derive(Deserialize)]
struct BaseMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(rename = "deviceId", default)]
    device_id: String,
    #[serde(rename = "msgId", default)]
    msg_id: String,
}

#[derive(Serialize)]
struct AckMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(rename = "msgId")]
    msg_id: String,
    status: String,
}

/// A dashboard browser (kiosk screen) that currently holds an authenticated
/// `/E/ws` socket. Lives only in RAM (`AppState.screens`) for the lifetime of
/// the socket — it is the PDA scan-mirror target set (`GET /api/screens`), not
/// durable state. Keyed by the browser-persisted `screen_id`.
#[derive(Clone, Serialize)]
pub struct ScreenInfo {
    /// Human label, e.g. `"kiosk-11 · dimi"` (browser default, operator-editable).
    pub name: String,
    /// JWT subject (username) of the logged-in operator on that screen.
    pub user: String,
    /// When this screen first connected (RFC3339).
    pub since: String,
    /// Last time a socket for this screen_id (re)connected (RFC3339). A reconnect
    /// (the WS store retries every 3 s) refreshes this without losing `since`.
    pub last_seen: String,
}

/// Query params on the WS upgrade. `token` authenticates; `screen_id` +
/// `screen_name` register this browser as a mirror target (dashboards only).
#[derive(Deserialize)]
pub struct WsQuery {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub screen_id: Option<String>,
    #[serde(default)]
    pub screen_name: Option<String>,
}

/// GET /E/ws?token=<jwt>&screen_id=<uuid>&screen_name=<label> — WebSocket upgrade.
///
/// The broadcast stream carries operator-only events (TRIP_LIVE vehicle
/// positions, AI observer output, agent status, SCAN_EVENT mirrors), so
/// subscribing requires a valid JWT — passed as a query param because the
/// browser WebSocket API can't set an Authorization header. Tokenless
/// connections are still accepted for the legacy PDA DEVICE_IDENTIFY→ACK
/// handshake, but they get no broadcast subscription and nothing they send is
/// relayed anywhere.
///
/// A dashboard additionally passes `screen_id` (a uuid it persists in
/// localStorage) and `screen_name` so the PDA can pick it as a scan-mirror
/// target; the screen is registered for the life of the socket.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let claims = params
        .token
        .as_deref()
        .and_then(|t| eck_core::auth::validate_token(t, &state.jwt_secret).ok());
    let user = claims.as_ref().map(|c| c.sub.clone());
    let authed = claims.is_some();

    // Only register a screen when the socket is authenticated AND the browser
    // identified itself (dashboards do; PDAs don't).
    let screen = match (authed, params.screen_id.filter(|s| !s.is_empty())) {
        (true, Some(id)) => {
            let now = Utc::now().to_rfc3339();
            let name = params
                .screen_name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| id.clone());
            Some((id, name, user.unwrap_or_default(), now))
        }
        _ => None,
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, authed, screen))
}

async fn handle_socket(
    socket: WebSocket,
    state: Arc<AppState>,
    authed: bool,
    screen: Option<(String, String, String, String)>,
) {
    let client_id = format!("web_{}", uuid::Uuid::new_v4());
    info!(
        "WMS WebSocket client connected: {} (authed={})",
        client_id, authed
    );

    // Register this screen as a live mirror target. A reconnect of the same
    // screen_id keeps the original `since` and just refreshes `last_seen`.
    if let Some((id, name, user, now)) = &screen {
        let mut screens = state.screens.write().await;
        let entry = screens.entry(id.clone()).or_insert_with(|| ScreenInfo {
            name: name.clone(),
            user: user.clone(),
            since: now.clone(),
            last_seen: now.clone(),
        });
        entry.name = name.clone();
        entry.user = user.clone();
        entry.last_seen = now.clone();
    }

    if authed {
        let (mut sender, mut receiver) = socket.split();
        let mut rx = state.ws_tx.subscribe();

        let mut send_task = tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                if sender.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
        });

        // Drain incoming frames to keep the socket alive, but NEVER feed
        // client input back into the broadcast: ws_tx is a server-authored
        // event stream, and relaying arbitrary client text let any connected
        // client inject fake events (INVENTORY_UPDATED, TRIP_LIVE, …) into
        // every open dashboard.
        let mut recv_task = tokio::spawn(async move {
            while let Some(Ok(_)) = receiver.next().await {}
        });

        tokio::select! {
            _ = &mut send_task => recv_task.abort(),
            _ = &mut recv_task => send_task.abort(),
        };
    } else {
        // Unauthenticated (legacy PDA path): no broadcast subscription — the
        // stream leaks operator data. Answer DEVICE_IDENTIFY with a direct
        // ACK on this socket only.
        let (mut sender, mut receiver) = socket.split();
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(text) = msg {
                let text_str: &str = &text;
                if let Ok(base) = serde_json::from_str::<BaseMessage>(text_str) {
                    if base.msg_type == "DEVICE_IDENTIFY" && !base.device_id.is_empty() {
                        info!("Device identified: {}", base.device_id);
                        let ack = AckMessage {
                            msg_type: "ACK".to_string(),
                            msg_id: base.msg_id,
                            status: "connected".to_string(),
                        };
                        if let Ok(ack_json) = serde_json::to_string(&ack) {
                            if sender.send(Message::Text(ack_json.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    // Socket closed: drop this screen from the live set. Guarded by the same
    // screen_id so a stale reconnect race doesn't remove a fresher entry.
    if let Some((id, _, _, now)) = &screen {
        let mut screens = state.screens.write().await;
        if screens.get(id).map(|s| &s.last_seen == now).unwrap_or(false) {
            screens.remove(id);
        }
    }

    info!("WMS WebSocket client disconnected: {}", client_id);
}

/// GET /api/screens — the live set of dashboard screens a PDA can mirror scans
/// to. Any authenticated role (incl. observer). Sorted by display name.
/// Shape: `{ "screens": [ { screen_id, name, user, since, last_seen } ] }`.
pub async fn list_screens(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let screens = state.screens.read().await;
    let mut out: Vec<serde_json::Value> = screens
        .iter()
        .map(|(id, s)| {
            serde_json::json!({
                "screen_id": id,
                "name": s.name,
                "user": s.user,
                "since": s.since,
                "last_seen": s.last_seen,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase()
            .cmp(&b.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase())
    });
    Json(serde_json::json!({ "screens": out }))
}
