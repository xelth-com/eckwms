//! chat-pii-1001 P0 — test-only capture harness for the dashboard chat.
//!
//! This module exists to PROVE where personal data leaks on the dashboard chat
//! path today, and to turn those proofs green as the masking steps (P1-P3) land.
//! It implements NO masking of its own. It gives the capture tests
//! (`chat_capture_tests`) four things:
//!
//! 1. A minimal [`AppState`] fixture on an in-memory SurrealDB (`mem://`, the
//!    `kv-mem` feature is already on) with the tables the chat path touches, plus
//!    seed helpers for synthetic support tickets and one attachment whose stored
//!    OCR extract carries an AcroForm `--- Formularfelder ---` block. ALL data is
//!    invented (see [`synthetic`]).
//! 2. An in-process mock model provider ([`MockProvider`]) — an `axum` server on
//!    `127.0.0.1:0` that speaks BOTH wire formats (OpenAI-compatible "OR" route
//!    and the Gemini `generateContent` route), drives the brain tool loop from a
//!    [`MockReply`] script, and records every request body it receives.
//! 3. A `tracing` capture layer so a test can assert over the log lines the run
//!    emitted (scoped to the current thread via `set_default`).
//! 4. [`run_chat`] — the driver that pushes a scripted chat through the REAL
//!    entry (`orchestrator::answer_chat`), collects the SSE `BrainEvent`s, writes
//!    the `chat_session` / `chat_turn` telemetry through the real `chat_log`
//!    writer, and returns a [`Capture`] with every sink for the assertions.
//!
//! ## Routing the mock (no production seam needed for the OR route)
//! The single provider chokepoint is `eck_core::ai::AiAuth::generate_content_raw`.
//! The OR route's BYOK URL honours `ECK_OR_BASE_URL`, so setting that to the mock
//! address + a non-empty `OPENROUTER_API_KEY` points the whole brain loop (and,
//! with `ECK_AI_TASK_ROUTES=chat_compact=…`, the compaction call) at the mock.
//! The Gemini (`Studio`/`Vertex`) route builds a HARD-CODED googleapis.com host
//! with no env override — see [`GEMINI_BASE_URL_SEAM`]; scenario (g) needs that
//! seam and self-skips until it lands.
//!
//! ## Later modes (P0 parameterisation)
//! [`HarnessOptions::masker`] carries the three design-v2 modes (masker available
//! / masker failing / feature gate off). They do NOTHING yet — P1/P2 will read
//! them. The seam is here so the capture tests can be re-pointed at each mode
//! without reshaping the harness.
//!
//! ## Env hygiene
//! The route is selected through process-global env vars; tests MUST hold
//! [`env_lock`] for the whole body so parallel tests don't clobber each other.

#![cfg(test)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::{extract::State, Json, Router};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::ai::orchestrator::{self, BrainEngine, BrainEvent, HedgeMode};
use crate::mcp::McpTier;
use crate::AppState;

/// The env var the Gemini-route seam WOULD read (requested of the supervisor; see
/// the module docs and `P0-STATE.md`). Until the one-line change lands in
/// `core::ai::AiAuth::generate_url` (Studio arm), setting this has no effect and
/// scenario (g) self-skips.
pub const GEMINI_BASE_URL_SEAM: &str = "ECK_GEMINI_BASE_URL";

/// Process-wide lock serialising every env-dependent capture test.
fn env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Acquire the process-wide env lock for the WHOLE test body (the route lives in
/// process env). POISON-RESILIENT: the capture LEAK tests panic BY DESIGN (that is
/// the red proof), which poisons a plain `std::Mutex`; recovering the inner guard
/// means one expected panic does not cascade a `PoisonError` into every later test.
pub fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    env_lock().lock().unwrap_or_else(|e| e.into_inner())
}

// ───────────────────────── synthetic fixtures ──────────────────────────────

/// Invented identities — never real customer data. +49 555 is a reserved-range
/// style number; example.org is the RFC-2606 documentation domain.
pub mod synthetic {
    /// A full person name that NER must catch.
    pub const FULL_NAME: &str = "Tessa Probandt";
    /// A surname fragment an operator would type to look a person up (the
    /// person-lookup case the design must keep working).
    pub const NAME_FRAGMENT: &str = "Probandt";
    pub const PHONE: &str = "+49 555 0172 4461";
    pub const EMAIL: &str = "tessa.probandt@example.org";
    pub const STREET: &str = "Lindenpfad 14b";
    /// A town + postal code — these stay CLEAR by design (already open next to
    /// address tokens), so a test can assert they are NOT masked.
    pub const CITY: &str = "Beispielstadt";
    pub const PLZ: &str = "78462";
    pub const COMPANY: &str = "Mustermann Diagnostik GmbH";
    /// A harmless marker used as the positive control: it carries no PII, so it
    /// MUST survive to the outbound body — proving the capture sees the traffic.
    pub const MARKER: &str = "ZZMARKERZZ";
    /// The seeded attachment's CAS id.
    pub const ATTACHMENT_ID: &str = "cas-tessa-form-0001";
}

/// Seed a few synthetic support tickets into `document` (the table `customer_360`
/// / `ticket_search` read). Shapes match what those tools expect:
/// `type='support_ticket'`, `meta.{customer,email,phone,address,company,...}`.
pub async fn seed_tickets(state: &AppState) {
    let docs = vec![
        json!({
            "type": "support_ticket",
            "meta": {
                "ticket_number": "GER2610-001",
                "customer": synthetic::FULL_NAME,
                "company": synthetic::COMPANY,
                "email": synthetic::EMAIL,
                "phone": synthetic::PHONE,
                "address": format!("{}, {} {}", synthetic::STREET, synthetic::PLZ, synthetic::CITY),
                "subject": "Device 770 shows error E-14 after firmware update",
                "status": "Open",
                "device_model": "770",
            }
        }),
        json!({
            "type": "support_ticket",
            "meta": {
                "ticket_number": "GER2610-002",
                "customer": "Jordan Vester",
                "company": "Beispiel Klinik AG",
                "email": "jordan.vester@example.org",
                "phone": "+49 555 0188 2210",
                "address": "Ahornweg 3, 70173 Musterort",
                "subject": "Calibration drift on model 270",
                "status": "Closed",
                "device_model": "270",
            }
        }),
    ];
    for d in docs {
        let _ = state
            .db
            .query("CREATE document CONTENT $d")
            .bind(("d", d))
            .await
            .and_then(|mut r| r.check());
    }
}

/// Seed one `file_resource` row whose STORED OCR extract (extractor_ver =
/// current) carries an AcroForm `--- Formularfelder ---` block with synthetic
/// identity/contact/address. Because `extractor_ver == EXTRACTOR_VER`,
/// `attachments::ocr_one` returns the stored text WITHOUT touching bytes — no
/// file on disk is needed.
pub async fn seed_attachment(state: &AppState) {
    use crate::ai::attachments::EXTRACTOR_VER;
    let extracted_text = format!(
        "SERVICE FORM — RMA intake\nDevice model: 770\nError code: E-14\n\n\
         --- Formularfelder ---\n\
         Name: {name}\n\
         Strasse: {street}\n\
         PLZ: {plz}\n\
         Ort: {city}\n\
         Telefon: {phone}\n\
         E-Mail: {email}\n\
         Seriennummer: ZB770-0099\n",
        name = synthetic::FULL_NAME,
        street = synthetic::STREET,
        plz = synthetic::PLZ,
        city = synthetic::CITY,
        phone = synthetic::PHONE,
        email = synthetic::EMAIL,
    );
    let row = json!({
        "cas_uuid": synthetic::ATTACHMENT_ID,
        // Non-empty storage_path so resolve_extract_target returns this row.
        "storage_path": "data/synthetic/tessa-form.pdf",
        "original_name": "tessa-service-form.pdf",
        "mime_type": "application/pdf",
        "hash": "deadbeef",
        "extracted_text": extracted_text,
        "extract_source": "text_layer",
        "extractor_ver": EXTRACTOR_VER,
        "extracted_pages": 1,
    });
    let _ = state
        .db
        .query("CREATE file_resource CONTENT $d")
        .bind(("d", row))
        .await
        .and_then(|mut r| r.check());
}

// ───────────────────────── AppState fixture ────────────────────────────────

/// Build a minimal [`AppState`] on an in-memory SurrealDB. Reusable by any wms
/// test that needs an `AppState` (there was none before this). Tables are
/// schemaless (SurrealDB creates them on first write), so no DEFINE is needed for
/// the chat path.
pub async fn build_state() -> Arc<AppState> {
    let db = surrealdb::engine::any::connect("mem://")
        .await
        .expect("connect in-mem db");
    db.use_ns("eck").use_db("main").await.expect("use ns/db");

    // Define (schemaless) the tables the chat path READS before it writes. On the
    // fresh in-mem store a `SELECT ... FROM <undefined table>` ERRORS ("table does
    // not exist") — unlike the production SurrealKV store, where it returns empty —
    // so `chat_log::ensure_session` (which SELECTs before it CREATEs) would fail.
    // Defining them up front makes the fixture behave like a warmed node.
    let _ = db
        .query(
            "DEFINE TABLE IF NOT EXISTS chat_session SCHEMALESS; \
             DEFINE TABLE IF NOT EXISTS chat_turn SCHEMALESS; \
             DEFINE TABLE IF NOT EXISTS document SCHEMALESS; \
             DEFINE TABLE IF NOT EXISTS file_resource SCHEMALESS;",
        )
        .await
        .and_then(|mut r| r.check());

    let users_db = surrealdb::engine::any::connect("mem://")
        .await
        .expect("connect in-mem users db");
    users_db
        .use_ns("eck")
        .use_db("users")
        .await
        .expect("use ns/db users");

    let instance_id = "test-instance".to_string();
    let mesh_id = "test-mesh".to_string();
    let (ws_tx, _ws_rx) = tokio::sync::broadcast::channel::<String>(16);

    let relay = eck_core::sync::relay_client::RelayClient::new_multi(&[], &instance_id, &mesh_id);
    let sync_engine = Arc::new(eck_core::sync::engine::SyncEngine::new(
        instance_id.clone(),
        mesh_id.clone(),
        relay,
        db.clone(),
        None,
        "full".to_string(),
    ));

    let agent_controller = crate::services::agent_manager::AgentController::new(
        db.clone(),
        ws_tx.clone(),
        instance_id.clone(),
        String::new(),
    );

    Arc::new(AppState {
        db,
        users_db,
        sync_engine,
        hedera: None,
        jwt_secret: "test-secret".to_string(),
        sync_secret: None,
        server_identity: eck_core::utils::identity::ServerIdentity {
            instance_id: instance_id.clone(),
            private_key: String::new(),
            public_key: String::new(),
        },
        instance_id,
        mesh_id,
        port: 0,
        setup_password: RwLock::new(None),
        ws_tx,
        agent_controller,
        node_role: "full".to_string(),
        i18n_lang_jobs: Default::default(),
        pii_reveal: Arc::new(crate::mcp::reveal::PiiRevealStore::new()),
        fuzzy_index: Arc::new(crate::ai::pii_fuzzy::FuzzyIndex::new()),
        plugins: None,
        // Added by the scan-mirroring worker to AppState (main.rs); mirrored here
        // verbatim so the bin test target compiles (see opus-worker report).
        screens: Arc::new(RwLock::new(std::collections::HashMap::new())),
        tunnel_router: Arc::new(std::sync::OnceLock::new()),
    })
}

// ───────────────────────── mock model provider ─────────────────────────────

/// One scripted model turn. The mock emits these in order for the BRAIN requests
/// (requests that carry a `tools` field). Requests WITHOUT tools are treated as
/// compaction/summary calls and always get a fixed benign summary (they do not
/// advance the script).
#[derive(Clone, Debug)]
pub enum MockReply {
    /// A final text answer.
    Text(String),
    /// A tool call the brain loop will dispatch for real.
    Tool { name: String, args: Value },
    /// SEVERAL tool calls in ONE model turn (each `(name, args)`), emitted with
    /// distinct call ids. Lets a test drive a model that batches calls — e.g. to
    /// exercise the per-turn / run budget and the denial path (wave 3e).
    ToolBatch(Vec<(String, Value)>),
}

struct MockInner {
    captured: Mutex<Vec<Value>>,
    script: Vec<MockReply>,
    cursor: AtomicUsize,
    summary_reply: String,
    /// When true, a request with NO `tools` field is ALSO answered from the script
    /// (not with the fixed summary). This simulates a model that ignores
    /// `toolConfig.mode=NONE` and still returns tool calls on the last turn (the
    /// Gemini-3.x behaviour the last-turn backstop exists for). Only safe when the
    /// run triggers no compaction (compaction is the other tool-less caller); the
    /// budget loop tests use an empty history, so it never does.
    answer_toolless_from_script: bool,
}

/// An in-process mock provider. Holds the captured outbound bodies and the
/// scripted replies. Bound to `127.0.0.1:0`; [`Self::base_url`] is the OR base
/// (`…` + `/chat/completions` appended by the client) AND the Gemini host prefix.
pub struct MockProvider {
    inner: Arc<MockInner>,
    addr: std::net::SocketAddr,
}

impl MockProvider {
    /// Start a mock serving `script` for the brain turns (tool-less requests get the
    /// fixed summary — the default for the capture tests).
    pub async fn start(script: Vec<MockReply>) -> Self {
        Self::start_with(script, false).await
    }

    /// Start a mock, choosing whether tool-less requests are answered from the
    /// script (see [`MockInner::answer_toolless_from_script`]).
    pub async fn start_with(script: Vec<MockReply>, answer_toolless_from_script: bool) -> Self {
        let inner = Arc::new(MockInner {
            captured: Mutex::new(Vec::new()),
            script,
            cursor: AtomicUsize::new(0),
            summary_reply: "Rolling summary (synthetic): prior turns folded.".to_string(),
            answer_toolless_from_script,
        });
        // Catch-all so both the OR `/chat/completions` endpoint and the Gemini
        // `/v1beta/models/<model>:generateContent` endpoint hit one handler.
        let app = Router::new().fallback(handle).with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        MockProvider { inner, addr }
    }

    /// The `http://127.0.0.1:<port>` base to feed `ECK_OR_BASE_URL` /
    /// `ECK_GEMINI_BASE_URL`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request body the mock received, in arrival order.
    pub fn captured(&self) -> Vec<Value> {
        self.inner.captured.lock().unwrap().clone()
    }
}

async fn handle(State(inner): State<Arc<MockInner>>, Json(body): Json<Value>) -> Json<Value> {
    inner.captured.lock().unwrap().push(body.clone());

    let is_gemini = body.get("contents").is_some();
    let has_tools = body.get("tools").is_some();

    // No tools → normally a compaction/summary call: return a benign summary in the
    // caller's shape and do NOT advance the brain script. But when the mock is told
    // to answer tool-less requests from the script, a last-turn request (tools off
    // via toolConfig NONE) is treated as a brain turn too — modelling a model that
    // ignores the mode=NONE and keeps calling tools.
    if !has_tools && !inner.answer_toolless_from_script {
        return Json(if is_gemini {
            gemini_text(&inner.summary_reply)
        } else {
            or_text(&inner.summary_reply)
        });
    }

    let idx = inner.cursor.fetch_add(1, Ordering::SeqCst);
    let reply = inner
        .script
        .get(idx)
        .cloned()
        .unwrap_or_else(|| MockReply::Text("(mock fallback final answer)".to_string()));

    let resp = match (reply, is_gemini) {
        (MockReply::Text(t), true) => gemini_text(&t),
        (MockReply::Text(t), false) => or_text(&t),
        (MockReply::Tool { name, args }, true) => gemini_tools(&[(name, args)]),
        (MockReply::Tool { name, args }, false) => or_tools(&[(name, args)]),
        (MockReply::ToolBatch(calls), true) => gemini_tools(&calls),
        (MockReply::ToolBatch(calls), false) => or_tools(&calls),
    };
    Json(resp)
}

fn or_usage() -> Value {
    json!({ "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120 })
}
fn gemini_usage() -> Value {
    json!({ "promptTokenCount": 100, "candidatesTokenCount": 20, "totalTokenCount": 120 })
}

fn or_text(text: &str) -> Value {
    json!({
        "choices": [{ "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
        "usage": or_usage(),
    })
}
fn or_tools(calls: &[(String, Value)]) -> Value {
    let tool_calls: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            json!({
                "id": format!("call_{}", i + 1),
                "type": "function",
                "function": { "name": name, "arguments": args.to_string() }
            })
        })
        .collect();
    json!({
        "choices": [{
            "message": { "role": "assistant", "content": null, "tool_calls": tool_calls },
            "finish_reason": "tool_calls"
        }],
        "usage": or_usage(),
    })
}
fn gemini_text(text: &str) -> Value {
    json!({
        "candidates": [{ "content": { "parts": [{ "text": text }], "role": "model" }, "finishReason": "STOP" }],
        "usageMetadata": gemini_usage(),
    })
}
fn gemini_tools(calls: &[(String, Value)]) -> Value {
    let parts: Vec<Value> = calls
        .iter()
        .map(|(name, args)| json!({ "functionCall": { "name": name, "args": args } }))
        .collect();
    json!({
        "candidates": [{
            "content": { "parts": parts, "role": "model" },
            "finishReason": "STOP"
        }],
        "usageMetadata": gemini_usage(),
    })
}

// ───────────────────────── log capture ─────────────────────────────────────

#[derive(Clone)]
struct VecWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for VecWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for VecWriter {
    type Writer = VecWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

// ───────────────────────── harness options ─────────────────────────────────

/// The three design-v2 modes the harness must be parameterisable for. Stored
/// only — P1/P2 will read them. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskerMode {
    /// Trusted masker available (NER / span service). The invariant must hold.
    Available,
    /// Trusted masker down → heuristic fallback. The invariant must still hold.
    Failing,
    /// Feature gate off (today's behaviour) — expected to leak.
    GateOff,
}

/// Which provider route to drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// OpenAI-compatible ("OR") route — reachable via `ECK_OR_BASE_URL`.
    OpenAiCompatible,
    /// Gemini `generateContent` route — needs the base-URL seam (scenario g).
    Gemini,
}

/// A scripted chat run request.
pub struct HarnessOptions {
    pub route: Route,
    pub masker: MaskerMode,
    pub tier: McpTier,
    pub question: String,
    pub summary: Option<String>,
    pub history: Vec<(String, String)>,
    pub script: Vec<MockReply>,
    /// Force compaction with a short history: `ECK_CHAT_KEEP_TURNS` override.
    pub keep_turns: Option<usize>,
    pub compact_chunk: Option<usize>,
    /// Override the per-run tool-output budget (`ECK_AI_TOOL_RUN_MAX_CHARS`). `None`
    /// leaves the default (60000); the budget loop tests set 512 (wave 3e).
    pub run_max_chars: Option<usize>,
    /// Answer tool-less requests from the script too — models a model that ignores
    /// `toolConfig.mode=NONE` on the last turn. See [`MockInner`]. Only for runs with
    /// no compaction (empty history).
    pub answer_toolless_from_script: bool,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        HarnessOptions {
            route: Route::OpenAiCompatible,
            masker: MaskerMode::GateOff,
            tier: McpTier::Master,
            question: String::new(),
            summary: None,
            history: Vec::new(),
            script: vec![MockReply::Text("done".to_string())],
            keep_turns: None,
            compact_chunk: None,
            run_max_chars: None,
            answer_toolless_from_script: false,
        }
    }
}

// ───────────────────────── capture result ──────────────────────────────────

/// Everything that left (or would have left) the node for one scripted chat run.
pub struct Capture {
    /// Every outbound model request body the mock received (both brain + compaction).
    pub outbound: Vec<Value>,
    /// The SSE `BrainEvent`s the run emitted, rendered as `{event, data}`.
    pub sse: Vec<Value>,
    /// The `chat_session` rows (with their turns) written by the real telemetry writer.
    pub chat_session: Option<Value>,
    /// The `chat_turn` rows for the session.
    pub chat_turns: Vec<Value>,
    /// Captured formatted log lines.
    pub logs: String,
    /// The final answer the brain returned (post handler-scrub for non-reveal tiers).
    pub answer: String,
    /// True when the mock saw at least one request (route actually reached the mock).
    pub mock_reached: bool,
}

impl Capture {
    /// Serialize the outbound bodies to one string for substring scans.
    pub fn outbound_text(&self) -> String {
        self.outbound.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n")
    }
    pub fn sse_text(&self) -> String {
        self.sse.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n")
    }
    pub fn telemetry_text(&self) -> String {
        let mut s = String::new();
        if let Some(cs) = &self.chat_session {
            s.push_str(&cs.to_string());
        }
        for t in &self.chat_turns {
            s.push('\n');
            s.push_str(&t.to_string());
        }
        s
    }

    /// The first sink (if any) in which `needle` appears, for the RED-state report.
    /// Order: body, SSE, chat_turn/session, log.
    pub fn leaked_sink(&self, needle: &str) -> Option<&'static str> {
        if self.outbound_text().contains(needle) {
            Some("body")
        } else if self.sse_text().contains(needle) {
            Some("SSE")
        } else if self.telemetry_text().contains(needle) {
            Some("chat_turn")
        } else if self.logs.contains(needle) {
            Some("log")
        } else {
            None
        }
    }

    /// Assert `needle` appears in NO sink; panic naming the sink that leaked.
    pub fn assert_absent(&self, needle: &str, label: &str) {
        if let Some(sink) = self.leaked_sink(needle) {
            panic!(
                "LEAK: {label} value {needle:?} reached the {sink} sink (expected absent)"
            );
        }
    }

    /// Positive control: `needle` MUST be in the outbound body (the mock saw traffic).
    pub fn assert_in_body(&self, needle: &str, label: &str) {
        assert!(
            self.outbound_text().contains(needle),
            "positive control failed: {label} marker {needle:?} not found in any outbound body — the harness is blind to the traffic"
        );
    }
}

// ───────────────────────── env guard ───────────────────────────────────────

/// Remembers and restores a set of env vars around a run so a route set by one
/// test never bleeds into another. The caller must still hold [`env_lock`].
struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
}
impl EnvGuard {
    fn new() -> Self {
        EnvGuard { saved: Vec::new() }
    }
    fn set(&mut self, key: &str, val: &str) {
        self.saved.push((key.to_string(), std::env::var(key).ok()));
        std::env::set_var(key, val);
    }
    fn remove(&mut self, key: &str) {
        self.saved.push((key.to_string(), std::env::var(key).ok()));
        std::env::remove_var(key);
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..).rev() {
            match v {
                Some(val) => std::env::set_var(&k, val),
                None => std::env::remove_var(&k),
            }
        }
    }
}

// ───────────────────────── driver ──────────────────────────────────────────

const BRAIN_MODEL: &str = "deepseek/deepseek-v4.1-flash-1017";

/// Drive one scripted chat through the real `answer_chat` entry against the mock,
/// capturing every sink. The caller MUST hold [`env_lock`] for the whole call.
pub async fn run_chat(opts: HarnessOptions) -> Capture {
    let state = build_state().await;
    seed_tickets(&state).await;
    seed_attachment(&state).await;

    let mock = MockProvider::start_with(opts.script.clone(), opts.answer_toolless_from_script).await;
    let base = mock.base_url();

    // The identity tokeniser (`obfuscate_pii`) PANICS when SYNC_SECRET is unset
    // (refuses a publicly-known default pepper). On-node tools tokenise matched
    // identities, so a tool like `customer_360` that finds a row would abort the
    // run without it. Set the SUITE-WIDE test pepper ("test_secret") and NEVER
    // restore it: the normal suite runs multi-threaded and other modules
    // (e.g. `support::reveal_tests`) also `set_var("SYNC_SECRET","test_secret")`
    // globally without restoring. Setting one agreed value unconditionally — not
    // through the restoring EnvGuard — means no concurrent test ever sees it unset
    // or divergent. This is the only env the harness leaves set on purpose.
    std::env::set_var("SYNC_SECRET", "test_secret");

    // Route env (restored after the run).
    let mut env = EnvGuard::new();
    // Disable the glm→gemini auto-hedge so a fast mock OR answer is the ONLY path.
    env.set("ECK_AI_HEDGE_SECS", "off");
    // No managed/studio creds in tests.
    env.remove("ECK_AI_MODE");
    env.remove("ECK_OR_PROXY_URL");
    env.remove("ECK_OR_SERVE_NODE");
    env.remove("ECK_OR_SLUG_API_KEY");
    env.remove("ECK_AI_MODEL_DENY");

    match opts.route {
        Route::OpenAiCompatible => {
            // BYOK OR → ECK_OR_BASE_URL. A non-empty key selects the BYOK path.
            env.set("OPENROUTER_API_KEY", "test-key");
            env.set("ECK_OR_BASE_URL", &base);
            // Route the compaction task to OR too so its body is captured.
            env.set("ECK_AI_TASK_ROUTES", &format!("chat_compact={BRAIN_MODEL}"));
        }
        Route::Gemini => {
            // The Gemini route uses the default (Studio) auth with a key present.
            env.set("GEMINI_API_KEY", "test-key");
            // The seam the Studio URL WOULD read (see GEMINI_BASE_URL_SEAM).
            env.set(GEMINI_BASE_URL_SEAM, &base);
            // Fail fast if the seam is absent (would otherwise dial googleapis.com).
            env.set("ECK_AI_BRAIN_CALL_SECS", "3");
            env.set("ECK_AI_BRAIN_RUN_SECS", "8");
        }
    }

    if let Some(k) = opts.keep_turns {
        env.set("ECK_CHAT_KEEP_TURNS", &k.to_string());
    }
    if let Some(c) = opts.compact_chunk {
        env.set("ECK_CHAT_COMPACT_CHUNK", &c.to_string());
    }
    if let Some(rm) = opts.run_max_chars {
        env.set("ECK_AI_TOOL_RUN_MAX_CHARS", &rm.to_string());
    }

    // Engine selection: OR route pins an Alt model (reaches resolve_or_route
    // directly, no ECK_AI_TASK_ROUTES needed for the brain); Gemini route forces
    // the Gemini lane.
    let engine = match opts.route {
        Route::OpenAiCompatible => BrainEngine::Alt(orchestrator::AltModel::DeepSeek),
        Route::Gemini => BrainEngine::Gemini,
    };

    // Log capture scoped to this thread (tokio::test is current-thread).
    let log_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(VecWriter(log_buf.clone()))
        .with_max_level(tracing::Level::TRACE)
        .without_time()
        .with_ansi(false)
        .finish();
    let _log_guard = tracing::subscriber::set_default(subscriber);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BrainEvent>();

    let result = orchestrator::answer_chat(
        &state,
        &opts.question,
        opts.summary.clone(),
        opts.history.clone(),
        opts.tier,
        engine,
        HedgeMode::Offer,
        Some(tx),
        None,
    )
    .await;

    // Drain SSE events (all sends were synchronous and in-task).
    let mut sse: Vec<Value> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        sse.push(render_event(&ev));
    }

    let mut answer = String::new();
    let mut chat_session = None;
    let mut chat_turns = Vec::new();

    if let Ok(run) = &result {
        // Emulate the handler: non-reveal tiers scrub the answer before telemetry.
        let mut ans = run.answer.clone();
        if !opts.tier.reveal_pii() {
            ans.answer = eck_core::utils::anonymizer::scrub_pii_regex(&ans.answer).0;
        }
        answer = ans.answer.clone();

        let tier_label = if opts.tier.reveal_pii() { "master" } else { "agent" };
        let sid = crate::ai::chat_log::ensure_session(
            &state.db,
            None,
            "dimi",
            tier_label,
            "deepseek",
            "dashboard",
            &opts.question,
        )
        .await;
        let idx = crate::ai::chat_log::next_idx(&state.db, &sid).await;
        crate::ai::chat_log::record_turn(
            &state.db,
            &sid,
            idx,
            &opts.question,
            &ans,
            1,
            run.compaction.as_ref(),
            None,
        )
        .await;
        if let Ok(Some(sess)) = crate::ai::chat_log::get_session(&state.db, &sid).await {
            chat_turns = sess
                .get("turns")
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();
            chat_session = Some(sess);
        }
    }

    // Render Done payload into SSE too (the handler emits it as the terminal
    // `done` event; it carries the answer + compaction summary — a real sink).
    if let Ok(run) = &result {
        let mut done = json!({
            "answer": answer,
            "model": run.answer.model,
        });
        if let Some(c) = &run.compaction {
            done["compaction"] = json!({ "summary": c.summary, "dropped_turns": c.dropped_turns });
        }
        sse.push(json!({ "event": "done", "data": done }));
    } else if let Err(e) = &result {
        sse.push(json!({ "event": "error", "data": { "error": e.to_string() } }));
    }

    let logs = String::from_utf8_lossy(&log_buf.lock().unwrap()).to_string();
    let outbound = mock.captured();
    let mock_reached = !outbound.is_empty();

    Capture {
        outbound,
        sse,
        chat_session,
        chat_turns,
        logs,
        answer,
        mock_reached,
    }
}

/// Render a `BrainEvent` the way `chat_stream` would put it on the wire.
fn render_event(ev: &BrainEvent) -> Value {
    match ev {
        BrainEvent::Turn { turn, max } => json!({ "event": "turn", "data": { "turn": turn, "max": max } }),
        BrainEvent::Tool { name, args_preview } => {
            json!({ "event": "tool", "data": { "name": name, "args_preview": args_preview } })
        }
        BrainEvent::ToolDone { name, ms, chars, truncated } => json!({
            "event": "tool_done",
            "data": { "name": name, "ms": ms, "chars": chars, "truncated": truncated }
        }),
        BrainEvent::Hedge { from, to, after_secs } => {
            json!({ "event": "hedge", "data": { "from": from, "to": to, "after_secs": after_secs } })
        }
        BrainEvent::HedgeOffer { from, to, after_secs } => {
            json!({ "event": "hedge_offer", "data": { "from": from, "to": to, "after_secs": after_secs } })
        }
        BrainEvent::Fallback { from, to, error } => {
            json!({ "event": "fallback", "data": { "from": from, "to": to, "error": error } })
        }
        BrainEvent::Done(payload) => json!({ "event": "done", "data": payload }),
        BrainEvent::Error(msg) => json!({ "event": "error", "data": { "error": msg } }),
    }
}
