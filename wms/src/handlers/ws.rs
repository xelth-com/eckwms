use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// GET /E/ws?token=<jwt> — WebSocket upgrade handler.
///
/// The broadcast stream carries operator-only events (TRIP_LIVE vehicle
/// positions, AI observer output, agent status), so subscribing requires a
/// valid JWT — passed as a query param because the browser WebSocket API
/// can't set an Authorization header. Tokenless connections are still
/// accepted for the legacy PDA DEVICE_IDENTIFY→ACK handshake, but they get
/// no broadcast subscription and nothing they send is relayed anywhere.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let authed = params
        .get("token")
        .map(|t| eck_core::auth::validate_token(t, &state.jwt_secret).is_ok())
        .unwrap_or(false);
    ws.on_upgrade(move |socket| handle_socket(socket, state, authed))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>, authed: bool) {
    let client_id = format!("web_{}", uuid::Uuid::new_v4());
    info!(
        "WMS WebSocket client connected: {} (authed={})",
        client_id, authed
    );

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

    info!("WMS WebSocket client disconnected: {}", client_id);
}
