//! Voice command resolution (movFast PDA — Voice Commands P2).
//!
//! The PDA resolves spoken commands locally first (on-device STT + a per-mode
//! keyword registry). Only on a LOCAL MISS does it fall back here, sending the
//! recognized text + the mode's available commands. Gemini picks the single best
//! matching `action` (or none). If the STT text looks unreliable AND no audio
//! was attached, we tell the device to re-send WITH the retained raw audio, and
//! Gemini transcribes it multimodally itself.
//!
//! Cost control: this is a user-triggered (push-to-talk), JWT-gated endpoint —
//! bounded per utterance. The device sends audio at most once per utterance
//! (one escalation, no loops). `maxOutputTokens` is tiny and the command list is
//! capped. The returned action is validated against the supplied command list so
//! the model can never invent an action that isn't on screen.

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

#[derive(Deserialize)]
pub struct VoiceCommandDto {
    pub action: String,
    pub description: String,
}

#[derive(Deserialize)]
pub struct VoiceResolveReq {
    pub mode: String,
    pub text: String,
    pub commands: Vec<VoiceCommandDto>,
    /// Base64 WAV (PCM16 mono). Sent only on the second-pass escalation when the
    /// first response flagged `needs_audio`. Absent/empty on the first pass.
    #[serde(default)]
    pub audio_wav_base64: Option<String>,
}

#[derive(Serialize)]
pub struct VoiceResolveResp {
    /// The chosen grid action (guaranteed to be one of the supplied commands), or
    /// null if nothing matched.
    pub action: Option<String>,
    /// True → the device should re-POST WITH `audio_wav_base64` for a multimodal
    /// re-listen. Always false once audio was already supplied.
    pub needs_audio: bool,
    /// Short machine/debug reason ("matched", "no_match", "ai_disabled", …).
    pub reason: String,
    /// "gemini" when the model was consulted, "off" when AI is unavailable.
    pub source: &'static str,
}

fn off(reason: &str) -> Json<VoiceResolveResp> {
    Json(VoiceResolveResp { action: None, needs_audio: false, reason: reason.to_string(), source: "off" })
}

/// POST /api/voice/resolve — Gemini fallback for an unmatched spoken command.
pub async fn resolve_voice(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<VoiceResolveReq>,
) -> ApiResult<Json<VoiceResolveResp>> {
    // AI gating — mirror gemini_match_tickets in handlers/trips.rs.
    if !eck_core::ai::AiAuth::is_enabled_in_env() {
        return Ok(off("ai_disabled"));
    }
    let model = match std::env::var("GEMINI_GENERATION_MODEL") {
        Ok(m) if !m.is_empty() => m,
        _ => return Ok(off("no_model")),
    };
    if req.text.trim().is_empty() || req.commands.is_empty() {
        return Ok(off("no_input"));
    }

    let http = match reqwest::Client::builder().timeout(Duration::from_secs(20)).build() {
        Ok(c) => c,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let auth = match eck_core::ai::AiAuth::resolve(&http).await {
        Ok(a) if a.is_configured() => a,
        _ => return Ok(off("ai_unconfigured")),
    };

    // Cap the command list so the prompt stays tiny.
    let commands: Vec<&VoiceCommandDto> = req.commands.iter().take(24).collect();
    let mut list = String::new();
    for c in &commands {
        list.push_str(&format!("- \"{}\" => {}\n", c.description, c.action));
    }

    let has_audio = req.audio_wav_base64.as_deref().map(|s| !s.is_empty()).unwrap_or(false);

    let system = format!(
        "Du bist ein Sprachbefehl-Router für eine Lager-/Werkstatt-App (Modus '{mode}'). \
         Gegeben ein per Spracherkennung (STT) erkannter Text (evtl. verhört/vertippt) und eine \
         Liste verfügbarer Befehle im Format \"Beschreibung\" => action, wähle die EINE am besten \
         passende action. Antworte AUSSCHLIESSLICH als JSON: \
         {{\"action\": <string|null>, \"needs_audio\": <bool>, \"reason\": <string>}}. \
         Regeln: action MUSS exakt einer der gelisteten actions entsprechen, sonst null. \
         Wenn der STT-Text unplausibel/unverständlich für die Befehle wirkt und KEIN Audio \
         beiliegt, setze needs_audio=true (action=null). Wenn Audio beiliegt, transkribiere es \
         selbst und stütze dich darauf statt auf den STT-Text; needs_audio bleibt dann false.",
        mode = req.mode
    );

    let mut parts: Vec<Value> = vec![json!({
        "text": format!("STT-Text: \"{}\"\n\nVerfügbare Befehle:\n{}", req.text, list)
    })];
    if has_audio {
        parts.push(json!({
            "inline_data": { "mime_type": "audio/wav", "data": req.audio_wav_base64.clone().unwrap() }
        }));
    }

    let payload = json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": [{ "parts": parts }],
        "generationConfig": {
            "temperature": 0.0,
            "maxOutputTokens": 128,
            "responseMimeType": "application/json"
        }
    });

    let text = match auth.generate_content(&http, &model, payload).await {
        Ok((t, _usage)) => t,
        Err(e) => {
            warn!("[voice] gemini resolve error: {e}");
            return Ok(Json(VoiceResolveResp {
                action: None, needs_audio: false, reason: "gemini_error".into(), source: "gemini",
            }));
        }
    };

    let parsed = extract_json(&text);
    // Validate the action against the supplied list — never trust the model to
    // invent an action that isn't actually on screen.
    let action = parsed
        .get("action")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "null")
        .map(|s| s.to_string())
        .filter(|a| commands.iter().any(|c| &c.action == a));

    // Only allow an audio escalation if we don't already have audio and nothing matched.
    let needs_audio = !has_audio
        && action.is_none()
        && parsed.get("needs_audio").and_then(|v| v.as_bool()).unwrap_or(false);

    let reason = if action.is_some() {
        "matched".to_string()
    } else if needs_audio {
        "needs_audio".to_string()
    } else {
        parsed.get("reason").and_then(|v| v.as_str()).unwrap_or("no_match").to_string()
    };

    Ok(Json(VoiceResolveResp { action, needs_audio, reason, source: "gemini" }))
}

/// Pull the first JSON object out of an LLM response (tolerates code fences /
/// surrounding prose).
fn extract_json(text: &str) -> Value {
    if let (Some(s), Some(e)) = (text.find('{'), text.rfind('}')) {
        if e > s {
            if let Ok(v) = serde_json::from_str::<Value>(&text[s..=e]) {
                return v;
            }
        }
    }
    Value::Null
}
