//! Vision wave (`.eck/OPENROUTER_TIER.md` §13, gated by `VISION_BENCH.md`):
//! background image summaries for stored photo attachments.
//!
//! Pipeline per attachment (all bench-derived):
//!   1. Source the byte-owning row via the CAS `replaced_by` chain
//!      ([`crate::ai::attachments::resolve_file_full`]) — originals come back
//!      AVIF-optimized, decoded in pure Rust by [`crate::ai::avif`] (no C `dav1d`;
//!      Linux-safe — see the AVIF-decode note below).
//!   2. Normalize: long side 1024 (never upscale), lossy **JPEG q75** (see the
//!      WebP note), data-URL'd into the vision shim (`inline_data` → `image_url`,
//!      §3.4).
//!   3. Pass 1: German repair-archive description + exact transcription, ending in
//!      a machine-parsable `UNLESBAR_JSON:` line (normalized 0–1000 bboxes).
//!   4. Two-pass agentic zoom: for each valid bbox crop the ORIGINAL full-res
//!      image (≤3 crops, min 64 px, clamped) and make ONE follow-up call that
//!      transcribes the crops; merge under a `Details:` line. Hard caps: 1 round,
//!      3 crops, 2 calls per image.
//!   5. Route by `ECK_AI_IMAGE_POLICY` (§13): `open` → all images through the
//!      vision route (glm); `standard` (default) → device/photo-like via the
//!      vision route, document-like images stay on the Gemini path; `strict` →
//!      Gemini only.
//!   6. Persist into the attachment's `file_resource` meta with the exact
//!      `ai_summary`/`doc_class` mesh discipline (hash-included field + author
//!      `_vclock` bump; the summary_sync_fix pattern) — content-hash guarded so
//!      re-runs skip done work.
//!
//! ## AVIF-decode decision
//! RESOLVED — no open question. `crate::ai::avif::decode_avif` already decodes
//! AVIF in pure safe Rust (`avif-parse` + `rav1d-safe`), no C `dav1d`, and is the
//! decoder the OCR/vision ladder already uses. Works on Linux (kiosk) and Windows
//! (dev). No shell-out, no `image` avif-decoder feature needed.
//!
//! ## WebP note (deviation from the bench's "WebP q75", flagged)
//! `image` 0.25's WebP support is DECODE-only for our purposes — lossy WebP
//! encoding needs the C `libwebp` (`webp` crate), which the whole stack
//! deliberately avoids (see `avif.rs`). A *lossless* WebP of a photo would also
//! balloon the data-URL and, since glm bills image tokens per pixel/byte, inflate
//! the exact cost lever the bench optimizes. So the normalizer emits **JPEG q75**
//! instead. Z.ai/OpenAI vision accept JPEG identically (VISION_BENCH anomaly #1:
//! JPEG was the documented, working fallback), and the whole existing vision path
//! (`attachments::downscale_jpeg`) already normalizes to JPEG. The tier
//! (1024 long side) is unchanged.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use eck_core::ai::AiAuth;
use eck_core::db::SurrealDb;
use image::DynamicImage;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::ai::attachments::{crop_relative, resolve_file_full};
use crate::ai::doc_classify::DocType;
use crate::AppState;

type HttpClient = reqwest::Client;

// ── Tuning constants (bench-derived) ────────────────────────────────────────

/// Pipeline revision stamped onto every persisted summary
/// (`file_resource.image_summary_ver`). BUMP to re-arm re-summarization
/// fleet-wide: the worker re-picks any live image row whose stored ver is below
/// this (or whose source bytes changed — see the content-hash guard).
pub(crate) const IMAGE_SUMMARY_VER: i64 = 1;

/// Normalization long side for pass 1 (VISION_BENCH knee: quality/cost sweet spot
/// for glm; 512 both loses legibility and fabricates, 2048 doubles token cost).
const NORMALIZE_LONG_SIDE: u32 = 1024;
/// Long side for a zoom crop sent in pass 2 — cropped from the full-res original,
/// then bounded so a big plate doesn't blow up the follow-up token bill.
const CROP_LONG_SIDE: u32 = 768;
/// Lossy JPEG quality for the normalized frames (bench tier = q75).
const JPEG_QUALITY: u8 = 75;
/// Hard caps (loop-guard lessons): one detail round, ≤3 crops, ≤2 calls total.
const MAX_CROPS: usize = 3;
const MAX_CALLS: u8 = 2;
/// A zoom crop smaller than this (in ORIGINAL pixels, per side) is expanded to it
/// (clamped) — a 12 px sliver is useless to transcribe.
const MIN_CROP_PX: u32 = 64;

// Worker cadence (modeled on the summarization worker).
const WORKER_INTERVAL_SECS: u64 = 45;
const WORKER_BATCH: usize = 3;
const WORKER_STARTUP_DELAY_SECS: u64 = 60;
const WORKER_ERROR_BACKOFF_SECS: u64 = 120;

// ── Policy (§13) ────────────────────────────────────────────────────────────

/// `ECK_AI_IMAGE_POLICY` = `open` | `standard` (default) | `strict`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ImagePolicy {
    /// Everything through the vision route (glm) — our kiosk default when opted in.
    Open,
    /// Class split: device/photo-like → vision route; document-like → Gemini.
    Standard,
    /// Gemini only — the vision (glm) route is never used.
    Strict,
}

pub(crate) fn image_policy() -> ImagePolicy {
    match std::env::var("ECK_AI_IMAGE_POLICY")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "open" => ImagePolicy::Open,
        "strict" => ImagePolicy::Strict,
        _ => ImagePolicy::Standard,
    }
}

/// A recognized *document* class (paperwork), as opposed to a device/repair
/// photo. In `standard` policy these stay on the Gemini vision path; everything
/// else (photo_screen, other, unclassified, or no card at all) is device-like and
/// takes the cheap vision route. `photo_screen` is explicitly device-like.
fn is_document_like(doc_type: DocType) -> bool {
    matches!(
        doc_type,
        DocType::Reparaturauftrag
            | DocType::Kostenvoranschlag
            | DocType::Invoice
            | DocType::ShippingLabel
            | DocType::Datenverarbeitung
            | DocType::QcReport
    )
}

/// Resolve the auth for one image given the policy and the attachment's class.
/// `open`/`strict` ignore the class. Fail-open matches `resolve_for_task`: a
/// vision route with no OR key/transport silently falls back to the default auth.
pub(crate) async fn resolve_image_auth(
    http: &HttpClient,
    policy: ImagePolicy,
    doc_type: Option<DocType>,
) -> Result<AiAuth> {
    match policy {
        ImagePolicy::Open => AiAuth::resolve_for_task(http, "vision").await,
        ImagePolicy::Strict => AiAuth::resolve(http).await,
        ImagePolicy::Standard => {
            if doc_type.map(is_document_like).unwrap_or(false) {
                AiAuth::resolve(http).await
            } else {
                AiAuth::resolve_for_task(http, "vision").await
            }
        }
    }
}

// ── Prompts (German, bench repair-archive, extended) ────────────────────────

const PASS1_PROMPT: &str = "Du bist ein Foto-Archivar für eine Geräte-Reparaturwerkstatt. Beschreibe dieses Foto sachlich und knapp für ein durchsuchbares Reparatur-Archiv:\n\
- Was ist zu sehen (Gerätetyp/-modell, Zustand, sichtbare Schäden oder Auffälligkeiten)?\n\
- Transkribiere JEDEN im Bild sichtbaren Text EXAKT (Aufkleber, Displays, Typenschilder, Seriennummern, Fehlermeldungen, mehrsprachige Warnhinweise). Setze Transkriptionen in Anführungszeichen.\n\
- Rate nichts, was nicht sichtbar ist. Ist etwas unlesbar oder zu klein, benenne es, statt es zu erfinden.\n\n\
Antworte auf Deutsch. Beende deine Antwort mit GENAU einer Zeile in diesem Format:\n\
UNLESBAR_JSON: [{\"bbox\":[x0,y0,x1,y1],\"was\":\"...\"}]\n\
Die Werte x0,y0,x1,y1 sind normalisierte Koordinaten von 0 bis 1000 (obere-linke bis untere-rechte Ecke) für Bildbereiche mit lesbarem Text/Seriennummer/Typenschild, die im aktuellen Bild zu klein oder unscharf zum exakten Ablesen sind. Gibt es nichts zum Nachzoomen, schreibe genau: UNLESBAR_JSON: []";

const PASS2_PROMPT: &str = "Transkribiere diese Ausschnitte exakt. Es sind vergrößerte Bildbereiche aus dem vorherigen Foto (Seriennummern, Typenschilder, Aufkleber, Displaytext). Gib pro Ausschnitt den exakt lesbaren Text zurück; rate nichts. Antworte auf Deutsch.";

// ── UNLESBAR_JSON parsing (lenient) ─────────────────────────────────────────

/// One flagged unreadable region: bbox in 0..1 relative coords + what it is.
#[derive(Debug, Clone, PartialEq)]
struct UnlesbarBox {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    was: String,
}

/// Extract the LAST `UNLESBAR_JSON:` line's array and parse it leniently:
///   - tolerate a trailing period / prose after the array,
///   - accept bboxes as 0–1000 ints/floats, drop malformed or degenerate ones,
///   - clamp to the frame, require x1>x0 and y1>y0.
/// Returns the summary text with the `UNLESBAR_JSON:` line stripped, plus the
/// parsed (valid) boxes.
fn parse_unlesbar(summary: &str) -> (String, Vec<UnlesbarBox>) {
    let marker = "UNLESBAR_JSON:";
    let Some(pos) = summary.rfind(marker) else {
        return (summary.trim_end().to_string(), Vec::new());
    };
    let head = summary[..pos].trim_end().to_string();
    let tail = &summary[pos + marker.len()..];
    // Slice from the first '[' to its matching ']' (lenient: ignores prose after).
    let boxes = extract_json_array(tail)
        .and_then(|arr| serde_json::from_str::<Value>(&arr).ok())
        .map(|v| parse_boxes(&v))
        .unwrap_or_default();
    (head, boxes)
}

/// Return the substring from the first '[' to its matching ']' (bracket-balanced),
/// or `None` if there is no complete array.
fn extract_json_array(s: &str) -> Option<String> {
    let start = s.find('[')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_boxes(v: &Value) -> Vec<UnlesbarBox> {
    let Some(arr) = v.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for item in arr {
        let Some(bb) = item.get("bbox").and_then(|b| b.as_array()) else { continue };
        if bb.len() != 4 {
            continue;
        }
        let c: Vec<f32> = bb.iter().filter_map(|n| n.as_f64().map(|f| f as f32)).collect();
        if c.len() != 4 {
            continue;
        }
        // Normalize 0–1000 → 0..1, clamp, require a positive area.
        let x0 = (c[0] / 1000.0).clamp(0.0, 1.0);
        let y0 = (c[1] / 1000.0).clamp(0.0, 1.0);
        let x1 = (c[2] / 1000.0).clamp(0.0, 1.0);
        let y1 = (c[3] / 1000.0).clamp(0.0, 1.0);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let was = item.get("was").and_then(|w| w.as_str()).unwrap_or("").to_string();
        out.push(UnlesbarBox { x: x0, y: y0, w: x1 - x0, h: y1 - y0, was });
        if out.len() >= MAX_CROPS {
            break;
        }
    }
    out
}

// ── Decode (decompression-bomb guarded) ─────────────────────────────────────

/// Decode source bytes to a `DynamicImage` with a decompression-bomb guard.
/// AVIF (the CAS `replaced_by` chain leaves originals AVIF-optimized) goes
/// through the pure-Rust decoder [`crate::ai::avif::decode_avif`], which caps its
/// own RGB allocation. Every other raster goes through `image::ImageReader` with
/// EXPLICIT width/height/alloc limits — `image::load_from_memory` has none, and
/// this path decodes thousands of user-supplied photos. Ported from the
/// optimizer's protective pattern (per-side 30_000 px, 512 MiB alloc).
fn decode_guarded(bytes: &[u8], mime: &str) -> Result<DynamicImage> {
    if mime == "image/avif" || crate::ai::avif::looks_like_avif(bytes) {
        return crate::ai::avif::decode_avif(bytes);
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| anyhow!("guess image format: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(crate::ai::avif::MAX_SIDE as u32);
    limits.max_image_height = Some(crate::ai::avif::MAX_SIDE as u32);
    limits.max_alloc = Some(crate::ai::avif::MAX_DECODE_ALLOC);
    reader.limits(limits);
    reader.decode().map_err(|e| anyhow!("decode image: {e}"))
}

// ── Normalization ───────────────────────────────────────────────────────────

/// Resize so the long side ≤ `max_side` (never upscale) and JPEG-encode at q75.
fn normalize_jpeg(img: &DynamicImage, max_side: u32) -> Result<Vec<u8>> {
    let (w, h) = (img.width(), img.height());
    let long = w.max(h);
    let scaled;
    let src: &DynamicImage = if long > max_side && long > 0 {
        let ratio = max_side as f32 / long as f32;
        let nw = ((w as f32 * ratio).round() as u32).max(1);
        let nh = ((h as f32 * ratio).round() as u32).max(1);
        scaled = img.resize_exact(nw, nh, image::imageops::FilterType::Triangle);
        &scaled
    } else {
        img
    };
    let rgb = DynamicImage::ImageRgb8(src.to_rgb8());
    let mut out = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    rgb.write_with_encoder(encoder).map_err(|e| anyhow!("jpeg encode: {e}"))?;
    Ok(out.into_inner())
}

/// Crop one flagged bbox from the ORIGINAL full-res image, enforcing MIN_CROP_PX
/// per side (expanded + clamped), then normalize the crop to CROP_LONG_SIDE JPEG.
fn crop_for_zoom(orig: &DynamicImage, b: &UnlesbarBox) -> Result<Vec<u8>> {
    let (ow, oh) = (orig.width(), orig.height());
    // Relative → pixel box.
    let mut px0 = (b.x * ow as f32).round() as i64;
    let mut py0 = (b.y * oh as f32).round() as i64;
    let mut px1 = ((b.x + b.w) * ow as f32).round() as i64;
    let mut py1 = ((b.y + b.h) * oh as f32).round() as i64;
    // Enforce a minimum crop size per side, centered, then clamp to the frame.
    if px1 - px0 < MIN_CROP_PX as i64 {
        let cx = (px0 + px1) / 2;
        px0 = cx - MIN_CROP_PX as i64 / 2;
        px1 = px0 + MIN_CROP_PX as i64;
    }
    if py1 - py0 < MIN_CROP_PX as i64 {
        let cy = (py0 + py1) / 2;
        py0 = cy - MIN_CROP_PX as i64 / 2;
        py1 = py0 + MIN_CROP_PX as i64;
    }
    px0 = px0.clamp(0, ow as i64 - 1);
    py0 = py0.clamp(0, oh as i64 - 1);
    px1 = px1.clamp(px0 + 1, ow as i64);
    py1 = py1.clamp(py0 + 1, oh as i64);
    // Reuse the shared relative-crop op with the clamped pixel box.
    let rel = crop_relative(
        orig,
        px0 as f32 / ow as f32,
        py0 as f32 / oh as f32,
        (px1 - px0) as f32 / ow as f32,
        (py1 - py0) as f32 / oh as f32,
    );
    normalize_jpeg(&rel, CROP_LONG_SIDE)
}

// ── Cost estimate (ECK_OR_PRICES) ───────────────────────────────────────────

/// USD cost of one call: prefer the exact upstream `openrouterCostUsd` (OR/glm
/// returns it), else estimate from token counts via `ECK_OR_PRICES`
/// (`<model>=<in_per_1M>:<out_per_1M>,...`). Gemini has no per-call USD here → 0.
fn call_cost_usd(model: &str, usage: &Value) -> f64 {
    if let Some(c) = usage.get("openrouterCostUsd").and_then(|v| v.as_f64()) {
        return c;
    }
    let table = price_table();
    let Some((inp, outp)) = table.get(model) else { return 0.0 };
    let pt = usage.get("promptTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let ct = usage.get("candidatesTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);
    (pt.max(0) as f64 / 1_000_000.0) * inp + (ct.max(0) as f64 / 1_000_000.0) * outp
}

fn price_table() -> HashMap<String, (f64, f64)> {
    let spec = std::env::var("ECK_OR_PRICES")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "glm-5.3-flash=0.075:0.25,z-ai/glm-5.3-flash=0.075:0.25".to_string());
    let mut m = HashMap::new();
    for entry in spec.split(',') {
        let Some((model, prices)) = entry.split_once('=') else { continue };
        let Some((i, o)) = prices.split_once(':') else { continue };
        if let (Ok(i), Ok(o)) = (i.trim().parse::<f64>(), o.trim().parse::<f64>()) {
            let model = model.trim();
            if !model.is_empty() {
                m.insert(model.to_string(), (i, o));
            }
        }
    }
    m
}

// ── Pipeline ────────────────────────────────────────────────────────────────

/// The outcome of one image's two-pass pipeline (before persistence).
pub(crate) struct ImageSummaryResult {
    pub summary: String,
    pub model: String,
    pub zoom_used: bool,
    pub calls: u8,
    pub cost_usd: f64,
    pub tier_prompt_tokens: i64,
    /// sha256 of the source blob this summary was computed from (content-hash
    /// guard key). Empty when the row carried no hash.
    pub src_hash: String,
}

fn vision_model(auth: &AiAuth) -> String {
    auth.or_model().map(str::to_string).unwrap_or_else(|| {
        std::env::var("GEMINI_VISION_MODEL")
            .or_else(|_| std::env::var("GEMINI_GENERATION_MODEL"))
            .unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string())
    })
}

/// One vision call: build the Gemini-shaped multimodal body (prompt text part +
/// one `inline_data` JPEG part per image) and dispatch via `generate_content`.
/// The OR shim (§3.4) translates `inline_data` → `image_url` transparently, so
/// this identical body works on both the Gemini and glm routes.
async fn vision_call(
    http: &HttpClient,
    auth: &AiAuth,
    model: &str,
    prompt: &str,
    jpegs: &[Vec<u8>],
) -> Result<(String, Value)> {
    let mut parts = vec![json!({ "text": prompt })];
    for j in jpegs {
        parts.push(json!({ "inline_data": { "mime_type": "image/jpeg", "data": B64.encode(j) } }));
    }
    let body = json!({
        "contents": [{ "role": "user", "parts": parts }],
        "generationConfig": { "temperature": 0.2, "maxOutputTokens": 2048 }
    });
    eck_core::ai::with_ai_task("image_summary", auth.generate_content(http, model, body)).await
}

/// Full two-pass pipeline for one attachment. Does NOT persist — the worker
/// persists, the soak discards. `auth` is the already-resolved primary route;
/// the CJK-bleed guard falls back to the default Gemini auth on persistent bleed.
/// Returns `Ok(None)` when the row has no decodable image (skipped).
pub(crate) async fn run_image_pipeline(
    db: &SurrealDb,
    http: &HttpClient,
    auth: &AiAuth,
    cas_uuid: &str,
) -> Result<Option<ImageSummaryResult>> {
    // 1. Resolve the byte-owning row (follows replaced_by; hydrates from mesh).
    let resolved = match resolve_file_full(db, cas_uuid).await {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    let bytes = match tokio::fs::read(&resolved.path).await {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let mime = resolved.mime.clone();
    let src_hash = resolved.hash.clone();

    // 2. Decode (AVIF via pure-Rust decoder) + normalize pass 1, off the runtime.
    let decode_mime = mime.clone();
    let (orig, pass1_jpeg) = match tokio::task::spawn_blocking(move || -> Result<(DynamicImage, Vec<u8>)> {
        let img = decode_guarded(&bytes, &decode_mime)?;
        let jpeg = normalize_jpeg(&img, NORMALIZE_LONG_SIDE)?;
        Ok((img, jpeg))
    })
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(_)) | Err(_) => return Ok(None), // undecodable / not an image
    };

    let mut model = vision_model(auth);
    let mut calls: u8 = 0;
    let mut cost = 0.0_f64;

    // 3. Pass 1 (with the vision-scoped CJK-bleed guard).
    let (mut summary, usage) = vision_call(http, auth, &model, PASS1_PROMPT, &[pass1_jpeg.clone()]).await?;
    calls += 1;
    cost += call_cost_usd(&model, &usage);
    let tier_prompt_tokens = usage.get("promptTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);

    if auth.or_model().is_some() && crate::ai::summarization::cjk_bleed_in_image_summary(&summary) {
        warn!("[img-guard] cjk_bleed cas={cas_uuid} — retrying once on the vision route");
        if let Ok((retry, ru)) = vision_call(http, auth, &model, PASS1_PROMPT, &[pass1_jpeg.clone()]).await {
            calls += 1;
            cost += call_cost_usd(&model, &ru);
            if crate::ai::summarization::cjk_bleed_in_image_summary(&retry) {
                // Persistent bleed → fall back to the default (Gemini) auth once.
                warn!("[img-guard] cjk_bleed cas={cas_uuid} persisted — falling back to default auth");
                if let Ok(def) = AiAuth::resolve(http).await {
                    let dmodel = vision_model(&def);
                    if let Ok((fb, fu)) = vision_call(http, &def, &dmodel, PASS1_PROMPT, &[pass1_jpeg.clone()]).await {
                        calls += 1;
                        cost += call_cost_usd(&dmodel, &fu);
                        summary = fb;
                        model = dmodel;
                    } else {
                        summary = retry;
                    }
                } else {
                    summary = retry;
                }
            } else {
                summary = retry;
            }
        }
    }

    // 4. Parse UNLESBAR_JSON; strip the machine line from the stored summary.
    let (mut summary_text, boxes) = parse_unlesbar(&summary);
    let mut zoom_used = false;

    // Two-pass zoom: ONE follow-up call transcribing up to MAX_CROPS crops from
    // the ORIGINAL full-res image. Bounded by MAX_CALLS.
    if !boxes.is_empty() && calls < MAX_CALLS {
        let orig_for_crop = Arc::new(orig);
        let mut crops: Vec<Vec<u8>> = Vec::new();
        for b in boxes.iter().take(MAX_CROPS) {
            let orig = orig_for_crop.clone();
            let b = b.clone();
            match tokio::task::spawn_blocking(move || crop_for_zoom(&orig, &b)).await {
                Ok(Ok(j)) => crops.push(j),
                _ => {}
            }
        }
        if !crops.is_empty() {
            match vision_call(http, auth, &model, PASS2_PROMPT, &crops).await {
                Ok((detail, du)) => {
                    calls += 1;
                    cost += call_cost_usd(&model, &du);
                    let detail = detail.trim();
                    if !detail.is_empty() {
                        summary_text = format!("{summary_text}\n\nDetails: {detail}");
                        zoom_used = true;
                    }
                }
                Err(e) => warn!("[img-zoom] cas={cas_uuid} follow-up call failed: {e}"),
            }
        }
    }

    Ok(Some(ImageSummaryResult {
        summary: summary_text,
        model,
        zoom_used,
        calls,
        cost_usd: cost,
        tier_prompt_tokens,
        src_hash,
    }))
}

// ── Persistence (ai_summary / doc_class mesh discipline) ────────────────────

/// Persist the image summary onto the byte-owning `file_resource` row, addressed
/// by the `cas_uuid` FIELD (like `persist_class`/`persist_extract`).
///
/// Mesh correctness — the `ai_summary` reference pattern (commit d4c8559):
/// `image_summary` and its siblings are NOT in `merkle::IGNORED_FIELDS`, so the
/// live-watch bridge folds them into the content hash automatically; this write
/// only advances THIS node's `_vclock` (via `next_local_vclock`) so the summary
/// strictly dominates a peer's summary-less copy and the change propagates once
/// (no null-clock churn). `image_summary_src_hash` is the content-hash guard: a
/// re-run whose source bytes are unchanged is skipped by the worker's candidate
/// query, so this never re-writes done work. `instance_id` comes from the sync
/// engine (unset in tests ⇒ nothing to sync ⇒ write skipped, like persist_class).
async fn persist_image_summary(db: &SurrealDb, cas_uuid: &str, res: &ImageSummaryResult) {
    let Some(iid) = crate::ai::attachments::mesh_instance_id() else {
        return;
    };
    let existing_vc: Option<Value> = db
        .query("SELECT VALUE _vclock FROM file_resource WHERE cas_uuid = $cid LIMIT 1")
        .bind(("cid", cas_uuid.to_string()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();
    let next_vc = eck_core::sync::conflict::next_local_vclock(existing_vc.as_ref(), &iid);

    let sql = "UPDATE file_resource MERGE { \
            image_summary: $summary, \
            image_summary_ver: $ver, \
            image_summary_model: $model, \
            image_summary_zoom: $zoom, \
            image_summary_src_hash: $src, \
            image_summarized_at: time::now(), \
            _vclock: $vc, \
            updated_at: time::now() \
         } WHERE cas_uuid = $cid";
    let r = db
        .query(sql)
        .bind(("summary", res.summary.clone()))
        .bind(("ver", IMAGE_SUMMARY_VER))
        .bind(("model", res.model.clone()))
        .bind(("zoom", res.zoom_used))
        .bind(("src", res.src_hash.clone()))
        .bind(("vc", next_vc))
        .bind(("cid", cas_uuid.to_string()))
        .await;
    match r {
        Ok(r) => {
            if let Err(e) = r.check() {
                warn!("[img-summary] persist {cas_uuid}: {e}");
            }
        }
        Err(e) => warn!("[img-summary] persist {cas_uuid}: {e}"),
    }
}

// ── Background worker ───────────────────────────────────────────────────────

/// One candidate image row from the worker's selection query.
struct Candidate {
    cas_uuid: String,
    doc_type: Option<DocType>,
}

/// Select up to `batch` live image rows needing a summary at the current ver.
/// Guard: skip rows already summarized at `IMAGE_SUMMARY_VER` whose source bytes
/// are unchanged (`image_summary_src_hash == hash`). Only decodable raster mimes;
/// only byte-owning (live, `replaced_by IS NONE`) rows; skip optimizer failures
/// (`optimization_level` -1/-2).
async fn select_candidates(db: &SurrealDb, batch: usize) -> Result<Vec<Candidate>> {
    let sql = "SELECT cas_uuid, doc_class.doc_type AS doc_type FROM file_resource \
         WHERE replaced_by IS NONE \
         AND storage_path IS NOT NONE \
         AND mime_type IN ['image/jpeg', 'image/png', 'image/webp', 'image/avif'] \
         AND (optimization_level IS NONE OR optimization_level >= 0) \
         AND (image_summary_ver IS NONE OR image_summary_ver < $ver \
              OR image_summary_src_hash != hash) \
         LIMIT $limit";
    let rows: Vec<Value> = db
        .query(sql)
        .bind(("ver", IMAGE_SUMMARY_VER))
        .bind(("limit", batch))
        .await?
        .take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let cas_uuid = r.get("cas_uuid").and_then(|v| v.as_str())?.to_string();
            let doc_type = r
                .get("doc_type")
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_value::<DocType>(Value::String(s.to_string())).ok());
            Some(Candidate { cas_uuid, doc_type })
        })
        .collect())
}

/// Background image-summary worker. Gated by `ECK_IMAGE_SUMMARY_WORKER=1` (like
/// `ECK_EMBED_WORKER`) — off, this returns immediately (the author is the kiosk,
/// §13). Cache nodes hold no blobs → skip. Honors the budget circuit breaker.
pub async fn start_image_summary_worker(state: Arc<AppState>) {
    if !std::env::var("ECK_IMAGE_SUMMARY_WORKER").map(|v| v.trim() == "1").unwrap_or(false) {
        info!("[img-summary] disabled (set ECK_IMAGE_SUMMARY_WORKER=1 to enable)");
        return;
    }
    if state.node_role == "cache" {
        info!("[img-summary] skipped (node_role=cache — no local blobs)");
        return;
    }
    tokio::time::sleep(Duration::from_secs(WORKER_STARTUP_DELAY_SECS)).await;
    let http = match HttpClient::builder().timeout(Duration::from_secs(90)).build() {
        Ok(c) => c,
        Err(e) => {
            warn!("[img-summary] HTTP client build failed: {e}");
            return;
        }
    };
    let db = &state.db;
    let policy = image_policy();
    info!("[img-summary] worker started (policy={policy:?}, batch={WORKER_BATCH}, interval={WORKER_INTERVAL_SECS}s)");
    let mut interval = tokio::time::interval(Duration::from_secs(WORKER_INTERVAL_SECS));

    loop {
        interval.tick().await;
        if crate::ai::telemetry::current_budget_level() == crate::ai::telemetry::BudgetLevel::Halt {
            continue;
        }
        let candidates = match select_candidates(db, WORKER_BATCH).await {
            Ok(c) => c,
            Err(e) => {
                warn!("[img-summary] selection failed: {e}");
                tokio::time::sleep(Duration::from_secs(WORKER_ERROR_BACKOFF_SECS)).await;
                continue;
            }
        };
        if candidates.is_empty() {
            continue;
        }
        for cand in candidates {
            let auth = match resolve_image_auth(&http, policy, cand.doc_type).await {
                Ok(a) if a.is_configured() => a,
                _ => continue,
            };
            let started = Instant::now();
            match run_image_pipeline(db, &http, &auth, &cand.cas_uuid).await {
                Ok(Some(res)) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    persist_image_summary(db, &cand.cas_uuid, &res).await;
                    let usage = json!({
                        "promptTokenCount": res.tier_prompt_tokens,
                        "candidatesTokenCount": 0,
                        "totalTokenCount": res.tier_prompt_tokens,
                    });
                    crate::ai::telemetry::log_telemetry(db, "image_summary", &res.model, &cand.cas_uuid, &usage).await;
                    info!(
                        "[img-live] cas={} model={} tier_prompt_tokens={} zoom={} calls={} cost_usd={:.6} latency_ms={}",
                        cand.cas_uuid, res.model, res.tier_prompt_tokens, res.zoom_used, res.calls, res.cost_usd, latency_ms
                    );
                }
                Ok(None) => { /* not a decodable image — leave it (won't be re-tried each cycle only if hash stable; acceptable) */ }
                Err(e) => {
                    warn!("[img-summary] cas={} error: {e}", cand.cas_uuid);
                    tokio::time::sleep(Duration::from_secs(WORKER_ERROR_BACKOFF_SECS)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_parses_env_default_standard() {
        // Pure mapping — exercise the parser's match on representative strings.
        assert!(matches!(image_policy_from("open"), ImagePolicy::Open));
        assert!(matches!(image_policy_from("strict"), ImagePolicy::Strict));
        assert!(matches!(image_policy_from("standard"), ImagePolicy::Standard));
        assert!(matches!(image_policy_from(""), ImagePolicy::Standard));
        assert!(matches!(image_policy_from("garbage"), ImagePolicy::Standard));
    }

    // Test seam mirroring image_policy() without touching process env.
    fn image_policy_from(s: &str) -> ImagePolicy {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => ImagePolicy::Open,
            "strict" => ImagePolicy::Strict,
            _ => ImagePolicy::Standard,
        }
    }

    #[test]
    fn document_like_split() {
        assert!(is_document_like(DocType::Invoice));
        assert!(is_document_like(DocType::Reparaturauftrag));
        assert!(is_document_like(DocType::QcReport));
        // Device/photo-like → NOT document-like → takes the vision route.
        assert!(!is_document_like(DocType::PhotoScreen));
        assert!(!is_document_like(DocType::Other));
        assert!(!is_document_like(DocType::UnclassifiableLowQuality));
    }

    #[test]
    fn parse_unlesbar_strips_line_and_reads_boxes() {
        let summary = "Foto eines ZetaBody-Analysators. Typenschild sichtbar aber klein.\n\
                       UNLESBAR_JSON: [{\"bbox\":[100,200,300,260],\"was\":\"Seriennummer\"}]";
        let (text, boxes) = parse_unlesbar(summary);
        assert!(!text.contains("UNLESBAR_JSON"));
        assert!(text.contains("Typenschild"));
        assert_eq!(boxes.len(), 1);
        let b = &boxes[0];
        assert!((b.x - 0.1).abs() < 1e-4);
        assert!((b.y - 0.2).abs() < 1e-4);
        assert!((b.w - 0.2).abs() < 1e-4);
        assert!((b.h - 0.06).abs() < 1e-4);
        assert_eq!(b.was, "Seriennummer");
    }

    #[test]
    fn parse_unlesbar_empty_array_and_trailing_prose() {
        let (text, boxes) = parse_unlesbar("Reines Foto.\nUNLESBAR_JSON: [] — nichts nachzuzoomen.");
        assert!(boxes.is_empty());
        assert!(!text.contains("UNLESBAR_JSON"));
    }

    #[test]
    fn parse_unlesbar_drops_degenerate_and_caps_to_max_crops() {
        // 1 degenerate (x1<=x0) dropped; 4 valid capped to MAX_CROPS(3).
        let summary = "x\nUNLESBAR_JSON: [\
            {\"bbox\":[500,500,400,600],\"was\":\"bad\"},\
            {\"bbox\":[0,0,100,100],\"was\":\"a\"},\
            {\"bbox\":[100,100,200,200],\"was\":\"b\"},\
            {\"bbox\":[200,200,300,300],\"was\":\"c\"},\
            {\"bbox\":[300,300,400,400],\"was\":\"d\"}]";
        let (_t, boxes) = parse_unlesbar(summary);
        assert_eq!(boxes.len(), MAX_CROPS);
        assert_eq!(boxes[0].was, "a"); // the degenerate one was skipped
    }

    #[test]
    fn parse_unlesbar_no_marker_returns_whole_text() {
        let (text, boxes) = parse_unlesbar("Nur Beschreibung, kein Marker.");
        assert_eq!(text, "Nur Beschreibung, kein Marker.");
        assert!(boxes.is_empty());
    }

    #[test]
    fn cost_prefers_exact_or_cost_then_estimates() {
        // Exact upstream cost wins.
        let exact = call_cost_usd("z-ai/glm-5.3-flash", &json!({ "openrouterCostUsd": 0.0012 }));
        assert!((exact - 0.0012).abs() < 1e-9);
        // No cost field → estimate from tokens via the default price table.
        let est = call_cost_usd(
            "glm-5.3-flash",
            &json!({ "promptTokenCount": 1_000_000, "candidatesTokenCount": 1_000_000 }),
        );
        assert!((est - (0.075 + 0.25)).abs() < 1e-6);
        // Unknown model, no cost → 0.
        assert_eq!(call_cost_usd("unknown/model", &json!({ "promptTokenCount": 10 })), 0.0);
    }
}
