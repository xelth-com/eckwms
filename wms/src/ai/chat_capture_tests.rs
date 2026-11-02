//! chat-pii-1001 P0 — capture tests proving where cleartext leaks on the chat path.
//!
//! The LEAK assertions are RED today (nothing on the chat path is masked). Each
//! leak test is `#[ignore]` so the normal suite stays green; run them with
//! `--ignored` to see the red state:
//!
//! ```text
//! PROTOC=… cargo test -p wms --bin wms chat_capture -- --ignored
//! ```
//!
//! Every leak test first asserts a POSITIVE CONTROL (a harmless marker DOES reach
//! the outbound body) so a failure of the leak assertion can't be confused with a
//! blind harness. Tier is `Master` (reveal_pii) — the worst case: today nothing
//! is masked, so all sinks carry cleartext; P1/P2 must make these green for EVERY
//! tier.
//!
//! See `.eck/collab/chat-pii-1001/P0-STATE.md` for the scenario → sink → fix map.

#![cfg(test)]

use super::test_support::{
    lock_env, run_chat, synthetic, HarnessOptions, MaskerMode, MockReply, Route,
};
use crate::mcp::McpTier;
use serde_json::json;

const RED: &str = "chat-pii-1001: red until P1/P2 land";

/// Smoke test (NOT ignored): the AppState fixture builds, the mock is reached, the
/// positive-control marker survives to the outbound body, and the run produces a
/// `chat_turn`. Proves the harness is wired before any leak assertion runs.
#[tokio::test]
async fn harness_smoke_reaches_mock_and_logs_turn() {
    let _g = lock_env();
    let cap = run_chat(HarnessOptions {
        question: format!("hello {} world", synthetic::MARKER),
        script: vec![MockReply::Text("hi there".to_string())],
        ..Default::default()
    })
    .await;

    assert!(cap.mock_reached, "mock received no request");
    cap.assert_in_body(synthetic::MARKER, "control");
    assert_eq!(cap.answer, "hi there");
    if cap.chat_turns.is_empty() {
        println!("--- LOGS ---\n{}\n--- SESSION ---\n{:?}", cap.logs, cap.chat_session);
    }
    assert!(!cap.chat_turns.is_empty(), "no chat_turn written");
}

/// (a) Cleartext in the QUESTION. The operator types a full name / phone / email /
/// street; today all of it reaches the model body (and the chat_turn), unmasked.
#[tokio::test]
#[ignore = "chat-pii-1001: red until P1/P2 land"]
async fn leak_a_cleartext_in_question() {
    let _ = RED;
    let _g = lock_env();
    let question = format!(
        "Please summarise the open tickets for {name} ({phone}, {email}) at {street}. marker {marker}",
        name = synthetic::FULL_NAME,
        phone = synthetic::PHONE,
        email = synthetic::EMAIL,
        street = synthetic::STREET,
        marker = synthetic::MARKER,
    );
    let cap = run_chat(HarnessOptions {
        question,
        script: vec![MockReply::Text("Here is the summary.".to_string())],
        ..Default::default()
    })
    .await;

    cap.assert_in_body(synthetic::MARKER, "control");
    for (needle, label) in pii_needles() {
        cap.assert_absent(needle, label);
    }
}

/// (b) Cleartext in HISTORY (a prior user turn) and in the client-supplied SUMMARY.
#[tokio::test]
#[ignore = "chat-pii-1001: red until P1/P2 land"]
async fn leak_b_cleartext_in_history_and_summary() {
    let _g = lock_env();
    let history = vec![
        (
            "user".to_string(),
            format!("earlier I mentioned {} on {}", synthetic::FULL_NAME, synthetic::STREET),
        ),
        ("assistant".to_string(), "noted".to_string()),
    ];
    let summary = Some(format!(
        "So far: customer {} reachable at {} / {}.",
        synthetic::FULL_NAME, synthetic::PHONE, synthetic::EMAIL
    ));
    let cap = run_chat(HarnessOptions {
        question: format!("and now? marker {}", synthetic::MARKER),
        history,
        summary,
        script: vec![MockReply::Text("ok".to_string())],
        ..Default::default()
    })
    .await;

    cap.assert_in_body(synthetic::MARKER, "control");
    for (needle, label) in pii_needles() {
        cap.assert_absent(needle, label);
    }
}

/// (c) Compaction FORCED: enough rounds + a lowered keep window. The oldest rounds
/// (carrying cleartext) are sent to the summarizer — the compaction request body is
/// captured too.
#[tokio::test]
#[ignore = "chat-pii-1001: red until P1/P2 land"]
async fn leak_c_compaction_body() {
    let _g = lock_env();
    // 4 rounds of history, keep=2, chunk=2 → compaction folds the oldest 2 rounds.
    let mut history = Vec::new();
    history.push((
        "user".to_string(),
        format!("ticket for {} at {}", synthetic::FULL_NAME, synthetic::STREET),
    ));
    history.push(("assistant".to_string(), "ok 1".to_string()));
    history.push((
        "user".to_string(),
        format!("call {} or email {}", synthetic::PHONE, synthetic::EMAIL),
    ));
    history.push(("assistant".to_string(), "ok 2".to_string()));
    history.push(("user".to_string(), "third".to_string()));
    history.push(("assistant".to_string(), "ok 3".to_string()));

    let cap = run_chat(HarnessOptions {
        question: format!("continue marker {}", synthetic::MARKER),
        history,
        keep_turns: Some(2),
        compact_chunk: Some(2),
        script: vec![MockReply::Text("done".to_string())],
        ..Default::default()
    })
    .await;

    cap.assert_in_body(synthetic::MARKER, "control");
    // The compaction body must have been sent (more than one outbound request).
    assert!(
        cap.outbound.len() >= 2,
        "expected a compaction request + a brain request, got {}",
        cap.outbound.len()
    );
    for (needle, label) in pii_needles() {
        cap.assert_absent(needle, label);
    }
}

/// (d) TOOL ECHO: the scripted model calls `customer_360` with the name fragment
/// from the question. `customer_360` echoes `query` in its result, so the next
/// outbound body (the functionResponse) carries the fragment back to the model.
#[tokio::test]
#[ignore = "chat-pii-1001: red until P1/P2 land"]
async fn leak_d_tool_echo_customer_360() {
    let _g = lock_env();
    let cap = run_chat(HarnessOptions {
        question: format!("who is {}? marker {}", synthetic::NAME_FRAGMENT, synthetic::MARKER),
        script: vec![
            MockReply::Tool {
                name: "customer_360".to_string(),
                args: json!({ "query": synthetic::NAME_FRAGMENT }),
            },
            MockReply::Text("Found the customer.".to_string()),
        ],
        ..Default::default()
    })
    .await;

    cap.assert_in_body(synthetic::MARKER, "control");
    // Two brain requests expected (initial + post-tool).
    assert!(cap.outbound.len() >= 2, "tool loop did not run a second turn");
    // The fragment is echoed back in the functionResponse body (and was in the
    // question). This is the echo leak P2-4 fixes.
    cap.assert_absent(synthetic::NAME_FRAGMENT, "name-fragment");
}

/// (e) TOOL ERROR path: a failing/invalid call carrying the value. `customer_360`
/// with an empty query errors `missing required argument`; here we pass the value
/// via a bad arg so the error string / echo can carry it. The error becomes a
/// functionResponse → outbound body.
#[tokio::test]
#[ignore = "chat-pii-1001: red until P1/P2 land"]
async fn leak_e_tool_error_path() {
    let _g = lock_env();
    // search_database requires `table`; omit it so the tool returns an error that
    // echoes the query value in context.
    let cap = run_chat(HarnessOptions {
        question: format!("lookup marker {}", synthetic::MARKER),
        script: vec![
            MockReply::Tool {
                name: "customer_360".to_string(),
                // A Phone_ style free value in an arg the tool will echo/err on.
                args: json!({ "query": synthetic::PHONE }),
            },
            MockReply::Text("done".to_string()),
        ],
        ..Default::default()
    })
    .await;

    cap.assert_in_body(synthetic::MARKER, "control");
    cap.assert_absent(synthetic::PHONE, "phone-in-tool-arg");
}

/// (f) `ocr_attachment` on the seeded attachment: the AcroForm `--- Formularfelder
/// ---` block (name / street / phone / email) must not reach the next outbound body.
#[tokio::test]
#[ignore = "chat-pii-1001: red until P1/P2 land"]
async fn leak_f_ocr_acroform_block() {
    let _g = lock_env();
    let cap = run_chat(HarnessOptions {
        question: format!("read the service form marker {}", synthetic::MARKER),
        script: vec![
            MockReply::Tool {
                name: "ocr_attachment".to_string(),
                args: json!({ "file_ids": [synthetic::ATTACHMENT_ID] }),
            },
            MockReply::Text("The form is filled in.".to_string()),
        ],
        ..Default::default()
    })
    .await;

    cap.assert_in_body(synthetic::MARKER, "control");
    assert!(cap.outbound.len() >= 2, "ocr tool loop did not run a second turn");
    // The AcroForm identity/contact/address values reached the model body.
    for (needle, label) in [
        (synthetic::FULL_NAME, "acroform-name"),
        (synthetic::STREET, "acroform-street"),
        (synthetic::PHONE, "acroform-phone"),
        (synthetic::EMAIL, "acroform-email"),
    ] {
        cap.assert_absent(needle, label);
    }
}

/// (g) The same as (a) and (d) on the SECOND route (Gemini). Needs the
/// `ECK_GEMINI_BASE_URL` seam (see `test_support::GEMINI_BASE_URL_SEAM`); until it
/// lands the Studio URL dials googleapis.com and the mock sees nothing — the test
/// then self-skips with a note rather than hitting the network.
#[tokio::test]
#[ignore = "chat-pii-1001: red until P1/P2 land (and Gemini base-URL seam)"]
async fn leak_g_gemini_route_question_and_tool_echo() {
    let _g = lock_env();
    let cap = run_chat(HarnessOptions {
        route: Route::Gemini,
        question: format!(
            "who is {} ({})? marker {}",
            synthetic::NAME_FRAGMENT, synthetic::EMAIL, synthetic::MARKER
        ),
        script: vec![
            MockReply::Tool {
                name: "customer_360".to_string(),
                args: json!({ "query": synthetic::NAME_FRAGMENT }),
            },
            MockReply::Text("Found.".to_string()),
        ],
        ..Default::default()
    })
    .await;

    if !cap.mock_reached {
        eprintln!(
            "leak_g SKIPPED: the Gemini route never reached the mock — the \
             {} seam is not present yet (requested of the supervisor). Scenario (g) \
             is blocked on that one-line change in core::ai::AiAuth::generate_url.",
            super::test_support::GEMINI_BASE_URL_SEAM
        );
        return;
    }

    cap.assert_in_body(synthetic::MARKER, "control");
    cap.assert_absent(synthetic::NAME_FRAGMENT, "name-fragment");
    cap.assert_absent(synthetic::EMAIL, "email");
}

/// The cleartext values a masked chat path must keep out of every sink. CITY / PLZ
/// are intentionally excluded — they stay clear by design (open next to address
/// tokens).
fn pii_needles() -> Vec<(&'static str, &'static str)> {
    vec![
        (synthetic::FULL_NAME, "full-name"),
        (synthetic::NAME_FRAGMENT, "name-fragment"),
        (synthetic::PHONE, "phone"),
        (synthetic::EMAIL, "email"),
        (synthetic::STREET, "street"),
    ]
}

/// A reminder that MaskerMode is a real seam the later steps read. (Keeps the
/// import meaningful and documents the intended parameterisation.)
#[test]
fn masker_modes_exist() {
    let modes = [MaskerMode::Available, MaskerMode::Failing, MaskerMode::GateOff];
    assert_eq!(modes.len(), 3);
    // Tier helper sanity: Master reveals, Agent/Observer do not.
    assert!(McpTier::Master.reveal_pii());
}
