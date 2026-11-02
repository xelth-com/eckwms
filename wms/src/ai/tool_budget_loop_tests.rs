//! chat-improve-1001 wave 3e — loop-level tool-budget / denial tests (Astra RC1).
//!
//! These drive the REAL brain loop (`orchestrator::answer_chat` → `drive_oneshot_
//! native`) against the capture mock (`test_support`), OR route, and assert the
//! tool-output size contract from the CAPTURED model request bodies — not from
//! cap arithmetic. They close F19-1 (the unknown-tool branch must obey the run
//! budget) and F19-2 (every terminal denial part stays within the ceiling for any
//! ACCEPTED name).
//!
//! The appended `functionResponse` parts are reconstructed from the last captured
//! OR request (whose `messages` carry the whole cumulative conversation): each
//! `role:"tool"` message is paired by `tool_call_id` to the name in the preceding
//! assistant `tool_calls`, and the Gemini-shape part the orchestrator charges —
//! `{"functionResponse":{"name":…,"response":…}}` — is rebuilt and measured.

#![cfg(test)]

use super::test_support::{lock_env, run_chat, synthetic, HarnessOptions, MockReply};
use crate::ai::orchestrator::DENIAL_PART_MAX_CHARS;
use serde_json::{json, Value};

/// The run cap the budget scenarios pin (`ECK_AI_TOOL_RUN_MAX_CHARS`), matching the
/// reviewer's RC1 (and the floor, so it is not raised).
const RUN_CAP: usize = 512;
/// MAX_TURNS in `drive_oneshot_native` — a run that ends earlier did so via a denial
/// wrap-up, not by exhausting its turns.
const MAX_TURNS: usize = 6;
/// The default per-turn call-count abort ceiling (`ECK_AI_TOOL_CALLS_PER_TURN_ABORT`),
/// which these tests do not override. A turn carrying more calls than this aborts the
/// run; so the number of appended parts per turn is <= this, and the whole-run size is
/// the CONSTANT bound `RUN_CAP + ABORT_CEILING * DENIAL_PART_MAX_CHARS` (F24-1/F24-3).
const ABORT_CEILING: usize = 32;
/// The fixed denial text (mirrors `TOOL_DENIAL_TEXT`): a tool response carrying it is
/// a budget denial, which is how a test tells a denial from an ordinary result.
const DENIAL_TEXT: &str = "budget hit - answer from what you have";

/// The whole-run ceiling the appended-part total must never exceed, computed from
/// CONSTANTS (not from the batch the mock emits — F24-3).
fn run_ceiling_bound() -> usize {
    RUN_CAP + ABORT_CEILING * DENIAL_PART_MAX_CHARS
}

/// Assert the number of appended `functionResponse` parts PER TURN (the growth of the
/// tool-message count between consecutive captured requests) never exceeds the abort
/// ceiling — i.e. the model cannot make one turn append an unbounded number of parts.
fn assert_per_turn_part_count_bounded(cap: &super::test_support::Capture) {
    let counts: Vec<usize> = cap.outbound.iter().map(|b| tool_parts(b).len()).collect();
    for w in counts.windows(2) {
        let appended = w[1].saturating_sub(w[0]);
        assert!(
            appended <= ABORT_CEILING,
            "a model turn appended {appended} parts > ceiling {ABORT_CEILING}"
        );
    }
}

/// Serialized char length of the exact `functionResponse` part the orchestrator
/// appends and charges for one tool call — the true size unit, rebuilt here.
fn part_chars(name: &str, response: &Value) -> usize {
    json!({ "functionResponse": { "name": name, "response": response } })
        .to_string()
        .chars()
        .count()
}

/// Reconstruct every appended tool result `(name, response)` from one captured OR
/// request body, pairing `role:"tool"` messages to their call name by id.
fn tool_parts(body: &Value) -> Vec<(String, Value)> {
    let msgs = body
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let mut id_to_name: std::collections::HashMap<String, String> = Default::default();
    for m in &msgs {
        if m.get("role").and_then(|r| r.as_str()) == Some("assistant") {
            if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    let name = tc
                        .pointer("/function/name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    id_to_name.insert(id, name);
                }
            }
        }
    }
    let mut out = Vec::new();
    for m in &msgs {
        if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
            let id = m.get("tool_call_id").and_then(|i| i.as_str()).unwrap_or("");
            let name = id_to_name.get(id).cloned().unwrap_or_default();
            let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let response: Value = serde_json::from_str(content).unwrap_or(Value::Null);
            out.push((name, response));
        }
    }
    out
}

fn is_denial(response: &Value) -> bool {
    response.get("error").and_then(|e| e.as_str()) == Some(DENIAL_TEXT)
}

/// Minimal save/restore for one env var, scoped to a test body (the caller already
/// holds the env lock). Used only to force the Gemini fallback offline + fast when a
/// test drives the OR leg to an error.
struct EnvVarGuard {
    key: String,
    prev: Option<String>,
}
impl EnvVarGuard {
    fn clear(key: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key: key.to_string(), prev }
    }
}
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(&self.key, v),
            None => std::env::remove_var(&self.key),
        }
    }
}

/// (a) F19-1: the model returns a batch of unknown (`not_a_tool`) calls every turn.
/// Each name passes the guard but is not in the catalog, so earlier it appended an
/// error part WITHOUT consuming the budget. Now the unknown branch goes through the
/// same `per_result_cap` enforcement: the parts stay within `RUN_CAP + batch*160`,
/// a denial fires once capacity is gone, the next request has tools off, and the run
/// ends in the wrap-up text — all read from the captured bodies.
#[tokio::test]
async fn repeated_unknown_calls_obey_the_run_budget_and_wrap_up() {
    let _g = lock_env();
    const BATCH: usize = 3;
    let batch: Vec<(String, Value)> =
        (0..BATCH).map(|_| ("not_a_tool".to_string(), json!({}))).collect();
    // More batches than turns can consume — the run must stop itself via a denial.
    let script: Vec<MockReply> = (0..MAX_TURNS)
        .map(|_| MockReply::ToolBatch(batch.clone()))
        .collect();

    let cap = run_chat(HarnessOptions {
        question: format!("repeat unknown calls {}", synthetic::MARKER),
        script,
        run_max_chars: Some(RUN_CAP),
        ..Default::default()
    })
    .await;

    assert!(cap.mock_reached, "mock received no request");
    let last = cap.outbound.last().expect("at least one request");
    assert!(
        last.get("tools").is_none(),
        "the terminating request must have tools off (wrap-up)"
    );

    let parts = tool_parts(last);
    assert!(!parts.is_empty(), "no tool parts were appended");
    // F24-3: the total is bounded by a CONSTANT (run cap + ceiling * 160), NOT by the
    // batch the mock emitted, and the per-turn appended-part COUNT is bounded too.
    let total: usize = parts.iter().map(|(n, r)| part_chars(n, r)).sum();
    let bound = run_ceiling_bound();
    assert!(
        total <= bound,
        "appended functionResponse parts total {total} > {bound} (run cap + ceiling*{DENIAL_PART_MAX_CHARS})"
    );
    assert_per_turn_part_count_bounded(&cap);

    assert!(
        parts.iter().any(|(_, r)| is_denial(r)),
        "no denial appeared — the run budget was not enforced on unknown calls"
    );

    let tools_on = cap.outbound.iter().filter(|b| b.get("tools").is_some()).count();
    assert!(
        (1..MAX_TURNS).contains(&tools_on),
        "run should wrap up before MAX_TURNS (tools-on requests = {tools_on})"
    );

    // The wrap-up (a tools-off request) is answered by the mock's fixed summary.
    assert!(
        cap.answer.contains("Rolling summary"),
        "run must return the wrap-up text, got {:?}",
        cap.answer
    );
}

/// (b) F19-2: on the 6th (last) turn the model ignores `toolConfig.mode=NONE` and
/// returns a batch of calls using a 64-char unknown name (the widest the guard
/// accepts). The last-turn backstop denies every one with the fixed envelope; each
/// terminal part must stay within the 160 ceiling — which a 64-char name (158 chars)
/// exceeded under the old 120 bound.
#[tokio::test]
async fn last_turn_unknown_64char_batch_stays_within_the_denial_ceiling() {
    let _g = lock_env();
    let name64 = "a".repeat(64);
    const BATCH: usize = 3;
    let batch64: Vec<(String, Value)> =
        (0..BATCH).map(|_| (name64.clone(), json!({}))).collect();

    // Turns 1..5 each make ONE cheap unknown call — enough to keep the budget alive
    // (no early denial) so the loop reaches the 6th turn; turn 6 is the 64-char
    // batch; then the wrap-up returns the final text.
    let mut script: Vec<MockReply> = (0..(MAX_TURNS - 1))
        .map(|_| MockReply::Tool { name: "probe".to_string(), args: json!({}) })
        .collect();
    script.push(MockReply::ToolBatch(batch64));
    script.push(MockReply::Text("final answer".to_string()));

    let cap = run_chat(HarnessOptions {
        question: "drive to the last turn".to_string(),
        script,
        run_max_chars: Some(RUN_CAP),
        // Model ignores mode=NONE and keeps calling tools on the last turn.
        answer_toolless_from_script: true,
        ..Default::default()
    })
    .await;

    assert!(cap.mock_reached, "mock received no request");
    assert_eq!(cap.answer, "final answer", "run must return the wrap-up text");

    let last = cap.outbound.last().expect("at least one request");
    let parts = tool_parts(last);
    let terminal: Vec<&(String, Value)> = parts
        .iter()
        .filter(|(n, r)| n == &name64 && is_denial(r))
        .collect();
    assert_eq!(
        terminal.len(),
        BATCH,
        "every 6th-turn 64-char call must get a terminal denial"
    );
    for (n, r) in terminal {
        let c = part_chars(n, r);
        assert!(
            c <= DENIAL_PART_MAX_CHARS,
            "terminal denial part is {c} chars > {DENIAL_PART_MAX_CHARS}"
        );
        // Sanity: this is exactly the F19-2 case — a 64-char name is 158 chars, which
        // broke the old 120 ceiling and is the reason it was raised to 160.
        assert!(c > 120, "expected the 64-char denial to exceed the old 120 bound, got {c}");
    }
    // F24-1/F24-3: the last-turn denials are now charged like every other part, the
    // per-turn appended count stays within the ceiling, and the whole-run total stays
    // within the constant bound.
    assert_per_turn_part_count_bounded(&cap);
    let total: usize = parts.iter().map(|(n, r)| part_chars(n, r)).sum();
    assert!(
        total <= run_ceiling_bound(),
        "appended parts total {total} > {}",
        run_ceiling_bound()
    );
}

/// (c) Control: a known catalog tool under an ample budget is dispatched and the run
/// answers normally — no denial, no forced wrap-up.
#[tokio::test]
async fn known_tool_under_ample_budget_is_dispatched_normally() {
    let _g = lock_env();
    let cap = run_chat(HarnessOptions {
        question: format!("look up {}", synthetic::NAME_FRAGMENT),
        script: vec![
            MockReply::Tool {
                name: "customer_360".to_string(),
                args: json!({ "query": synthetic::NAME_FRAGMENT }),
            },
            MockReply::Text("Here is the answer.".to_string()),
        ],
        // No run_max override → the default (ample) budget.
        ..Default::default()
    })
    .await;

    assert!(cap.mock_reached, "mock received no request");
    assert_eq!(cap.answer, "Here is the answer.", "normal model text must be returned");
    // The known tool actually ran (SSE carries its tool / tool_done events).
    assert!(
        cap.sse_text().contains("customer_360"),
        "customer_360 was not dispatched: {}",
        cap.sse_text()
    );
    // Not forced into the budget wrap-up (that would return the mock summary).
    assert!(
        !cap.answer.contains("Rolling summary"),
        "control must not hit the budget wrap-up"
    );
    // The dispatched result stayed within the per-result part ceiling (ample run cap).
    let last = cap.outbound.last().expect("a request");
    for (name, response) in tool_parts(last) {
        assert!(
            !is_denial(&response),
            "no call should be denied under an ample budget (got a denial for {name})"
        );
    }
}

/// (d) F24-1: a single model turn carrying MORE calls than the abort ceiling is a
/// malfunction — the run aborts on this engine BEFORE appending anything, so no
/// unbounded batch of denial parts ever enters the context. The 64 unknown 64-char
/// calls (> the default ceiling 32) at run cap 512 are never appended.
#[tokio::test]
async fn oversized_call_batch_aborts_before_appending() {
    let _g = lock_env();
    // The OR leg aborts, so answer_chat falls through to the Gemini path — keep that
    // offline + instant (no Studio key → `AiAuth::resolve` bails before any network).
    let _gem = EnvVarGuard::clear("GEMINI_API_KEY");
    let name64 = "a".repeat(64);
    let big_batch: Vec<(String, Value)> =
        (0..64).map(|_| (name64.clone(), json!({}))).collect();

    let cap = run_chat(HarnessOptions {
        question: "flood one turn with calls".to_string(),
        script: vec![MockReply::ToolBatch(big_batch)],
        run_max_chars: Some(RUN_CAP),
        ..Default::default()
    })
    .await;

    // The mock WAS reached (the first brain request was sent) ...
    assert!(cap.mock_reached, "mock received no request");
    // ... and the OR leg aborted with the count-ceiling reason, visible in the logs
    // (the final answer_chat error is the Gemini-fallback "not configured", since no
    // Gemini creds are wired on the OR route — so there is no normal answer).
    assert!(
        cap.logs.contains("tool calls in one turn"),
        "expected the per-turn count abort in the logs; logs: {}",
        cap.logs
    );
    assert!(
        cap.answer.is_empty(),
        "an aborted run must not return a normal answer, got {:?}",
        cap.answer
    );
    // No captured request contains the oversized batch — nothing was appended.
    for body in &cap.outbound {
        let n = tool_parts(body).iter().filter(|(nm, _)| nm == &name64).count();
        assert_eq!(n, 0, "an oversized batch leaked {n} parts into a request");
    }
}
