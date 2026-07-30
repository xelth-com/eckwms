//! PII reveal: the ephemeral token→value reverse map + the master-only
//! `reveal_tokens` JSON-RPC method.
//!
//! The MCP tools return customer PII as STABLE pseudonym tokens
//! (`obfuscate_pii`, the same keyed-SimHash scheme the summaries/embeddings
//! ride — see `eck_core::utils::anonymizer`). That token is one-way: reveal is
//! "find the value that produced this token", NOT "decode it". So every time a
//! tool tokenizes a field it also records `token → value` here, and a master
//! caller's `reveal_tokens` reads it back.
//!
//! This map lives in RAM ONLY — the same lifetime as the live records
//! themselves, deliberately NO persistent plaintext vault (that would recreate
//! the exact PII-at-rest exposure the tokens exist to avoid). It is bounded
//! FIFO (drop-oldest) so a long-lived node can't grow it without limit; an
//! evicted (or foreign-node) token simply reveals as unresolved.
//!
//! `reveal_tokens` is the ONLY path that returns clear PII, and only to a
//! master cert/token. It is a METHOD, not a tool, so it never appears in
//! `tools/list` for any tier — an agent caller is refused outright.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};

use super::McpTier;

/// Default cap on the reverse map — bounds RAM; drop-oldest on overflow.
const DEFAULT_CAP: usize = 50_000;

/// Ephemeral `token → clear-value` reverse map. Tools call [`Self::record`]
/// whenever they tokenize a PII field; [`reveal_tokens_method`] reads it back
/// for a master caller. Bounded FIFO by first-seen insertion.
pub(crate) struct PiiRevealStore {
    inner: RwLock<Inner>,
    cap: usize,
}

struct Inner {
    map: HashMap<String, String>,
    /// First-seen insertion order, for drop-oldest eviction. Each token appears
    /// here at most once (we only push on a genuinely new key).
    order: VecDeque<String>,
}

impl PiiRevealStore {
    pub(crate) fn new() -> Self {
        Self::with_cap(DEFAULT_CAP)
    }

    pub(crate) fn with_cap(cap: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
            cap: cap.max(1),
        }
    }

    /// Remember `token → value`. Idempotent for a repeated token: `obfuscate_pii`
    /// is deterministic, so the same value always maps to the same token and a
    /// re-record is a no-op for ordering. Empty inputs are ignored.
    pub(crate) fn record(&self, token: String, value: String) {
        if token.is_empty() || value.is_empty() {
            return;
        }
        let mut g = self.inner.write().unwrap();
        if g.map.contains_key(&token) {
            // Keep it; refresh the value (harmless — values are deterministic).
            g.map.insert(token, value);
            return;
        }
        if g.map.len() >= self.cap {
            if let Some(old) = g.order.pop_front() {
                g.map.remove(&old);
            }
        }
        g.order.push_back(token.clone());
        g.map.insert(token, value);
    }

    /// Resolve a token to its clear value, if known on THIS node.
    pub(crate) fn resolve(&self, token: &str) -> Option<String> {
        self.inner.read().unwrap().map.get(token).cloned()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.read().unwrap().map.len()
    }
}

impl Default for PiiRevealStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The `reveal_tokens` method: master-ONLY. Returns `{revealed: {token: value},
/// unresolved: [token]}`. Foreign/evicted/unknown tokens land in `unresolved`.
///
/// Pure over `(store, tier, params)` so the tier gate and resolution are unit
/// testable without an `AppState`. `Err((code, msg))` becomes a JSON-RPC error;
/// the agent-tier refusal is exactly that path.
pub(crate) fn reveal_tokens_method(
    store: &PiiRevealStore,
    tier: McpTier,
    params: &Value,
) -> Result<Value, (i32, String)> {
    if tier != McpTier::Master {
        return Err((
            -32001,
            "reveal_tokens requires the master subscription tier".to_string(),
        ));
    }
    let tokens: Vec<String> = params
        .get("tokens")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let mut revealed = serde_json::Map::new();
    let mut unresolved: Vec<String> = Vec::new();
    for t in tokens {
        match store.resolve(&t) {
            Some(v) => {
                revealed.insert(t, Value::String(v));
            }
            None => unresolved.push(t),
        }
    }
    Ok(json!({ "revealed": revealed, "unresolved": unresolved }))
}

/// DB fallback for tokens the RAM map has never seen: the tokens BAKED into
/// stored summaries/subjects at distillation time. The map is only populated
/// when a live tool tokenizes a field, so a master reading a summary gets
/// tokens the store can't resolve — `unresolved` answers for PII the system
/// itself handed out. `pii_fingerprints` indexes exactly those baked tokens:
/// find the documents carrying the token, re-derive their full reveal
/// dictionary from meta + raw payloads (the SAME `derive_reveal_pairs` the
/// staff display path uses — no plaintext vault), and record every derived
/// pair into the RAM store so the next reveal is instant. Runs only after the
/// master tier gate in [`reveal_tokens_method`]; on a thin/cache node with no
/// raw data the token simply stays unresolved.
async fn resolve_from_db(
    state: &Arc<crate::AppState>,
    tokens: &[String],
) -> HashMap<String, String> {
    use eck_core::utils::anonymizer::parse_pii_token;

    let mut found: HashMap<String, String> = HashMap::new();
    let mut seen_docs: HashSet<String> = HashSet::new();
    for t in tokens.iter().take(64) {
        if parse_pii_token(t).is_none() || found.contains_key(t) {
            continue;
        }
        // A prior token's derivation may already have populated the store.
        if let Some(v) = state.pii_reveal.resolve(t) {
            found.insert(t.clone(), v);
            continue;
        }
        let docs: Vec<Value> = state.db
            .query("SELECT record::id(id) AS id, meta FROM document WHERE pii_fingerprints CONTAINS $t LIMIT 3")
            .bind(("t", t.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        for doc in &docs {
            let Some(id) = doc.get("id").and_then(|v| v.as_str()) else { continue };
            if !seen_docs.insert(id.to_string()) {
                continue;
            }
            let raw: Option<Value> = state.db
                .query("SELECT payload FROM document_raw WHERE record::id(id) = $id LIMIT 1")
                .bind(("id", id.to_string()))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default();
            let threads: Vec<Value> = state.db
                .query("SELECT payload FROM document_raw WHERE type = 'support_thread' AND ticket_id = $id")
                .bind(("id", id.to_string()))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default();
            let payload = raw
                .and_then(|r| r.get("payload").cloned())
                .unwrap_or_else(|| json!({}));
            let meta = doc.get("meta").cloned().unwrap_or_else(|| json!({}));
            for (tok, val) in crate::handlers::support::derive_reveal_pairs(&payload, &threads, &meta) {
                state.pii_reveal.record(tok, val);
            }
            // Subject tokens minted by the on-device NER at distillation:
            // re-run the same (deterministic) model over the RAW subject to
            // reproduce the identical (token, span) pairs. Only nodes running
            // ECK_PII_NER=local can do this — elsewhere such tokens stay
            // unresolved, same as any thin-node token.
            #[cfg(feature = "local-embed")]
            if crate::ai::local_ner::ner_mode() == "local" {
                if let Some(subj) = payload.get("subject").and_then(|v| v.as_str()) {
                    match crate::ai::local_ner::extract_entities(subj).await {
                        Ok((persons, _orgs)) => {
                            for name in persons {
                                state.pii_reveal.record(
                                    eck_core::utils::anonymizer::obfuscate_pii(&name, "Name"),
                                    name,
                                );
                            }
                        }
                        Err(e) => tracing::warn!("[NER] reveal-side subject extraction failed: {e}"),
                    }
                }
            }
        }
        if let Some(v) = state.pii_reveal.resolve(t) {
            found.insert(t.clone(), v);
        }
    }
    found
}

/// Post-pass over a successful [`reveal_tokens_method`] result: try the DB
/// fallback for whatever stayed `unresolved` and move the hits to `revealed`.
pub(crate) async fn augment_from_db(state: &Arc<crate::AppState>, mut result: Value) -> Value {
    let unresolved: Vec<String> = result
        .get("unresolved")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(String::from).collect())
        .unwrap_or_default();
    if unresolved.is_empty() {
        return result;
    }
    let found = resolve_from_db(state, &unresolved).await;
    if found.is_empty() {
        return result;
    }
    if let Some(map) = result.get_mut("revealed").and_then(|v| v.as_object_mut()) {
        for (t, v) in &found {
            map.insert(t.clone(), Value::String(v.clone()));
        }
    }
    result["unresolved"] = json!(unresolved
        .into_iter()
        .filter(|t| !found.contains_key(t))
        .collect::<Vec<_>>());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use eck_core::utils::anonymizer::obfuscate_pii;

    fn set_pepper() {
        // obfuscate_pii panics without the mesh pepper; every real node has it.
        std::env::set_var("SYNC_SECRET", "test_secret");
    }

    /// Full round-trip at the store level: a real `obfuscate_pii` token reveals
    /// back to the exact clear value, and the token itself leaks no PII.
    #[test]
    fn tokenize_then_reveal_roundtrip() {
        set_pepper();
        let store = PiiRevealStore::new();
        let name = "Anna Müller";
        let token = obfuscate_pii(name, "Name");
        store.record(token.clone(), name.to_string());

        assert!(token.starts_with("Name_"));
        assert!(!token.contains("Anna"), "token must not embed PII: {token}");
        assert_eq!(store.resolve(&token).as_deref(), Some(name));
        assert_eq!(store.resolve("Name_0000000000000000"), None);
    }

    /// Distinct entities → distinct tokens (so reveal can't confuse them).
    #[test]
    fn distinct_values_distinct_tokens() {
        set_pepper();
        let a = obfuscate_pii("Anna Müller", "Name");
        let b = obfuscate_pii("Boris Ivanov", "Name");
        assert_ne!(a, b);
        // Same value + same type is stable across calls (fleet-wide determinism).
        assert_eq!(a, obfuscate_pii("Anna Müller", "Name"));
    }

    /// Bounded FIFO: the oldest token is dropped once the cap is exceeded.
    #[test]
    fn drop_oldest_eviction() {
        let store = PiiRevealStore::with_cap(2);
        store.record("Name_0000000000000001".into(), "v1".into());
        store.record("Name_0000000000000002".into(), "v2".into());
        store.record("Name_0000000000000003".into(), "v3".into()); // evicts #1
        assert_eq!(store.len(), 2);
        assert_eq!(store.resolve("Name_0000000000000001"), None);
        assert_eq!(store.resolve("Name_0000000000000002").as_deref(), Some("v2"));
        assert_eq!(store.resolve("Name_0000000000000003").as_deref(), Some("v3"));
    }

    /// Re-recording an existing token does not grow the map or reorder eviction.
    #[test]
    fn record_is_idempotent() {
        let store = PiiRevealStore::with_cap(2);
        store.record("Email_00000000000000AA".into(), "a@x.de".into());
        store.record("Email_00000000000000AA".into(), "a@x.de".into());
        assert_eq!(store.len(), 1);
    }

    /// Agent tier is refused; master resolves known tokens and lists the rest.
    #[test]
    fn reveal_tokens_tier_gate_and_resolution() {
        let store = PiiRevealStore::new();
        store.record("Name_00000000000000AB".into(), "Real Name".into());
        let params = json!({ "tokens": ["Name_00000000000000AB", "Phone_00000000000000FF"] });

        // Agent → refused.
        let err = reveal_tokens_method(&store, McpTier::Agent, &params).unwrap_err();
        assert_eq!(err.0, -32001);
        assert!(err.1.contains("master"));

        // Master → resolves the known token, lists the unknown one.
        let ok = reveal_tokens_method(&store, McpTier::Master, &params).unwrap();
        assert_eq!(ok["revealed"]["Name_00000000000000AB"], json!("Real Name"));
        assert_eq!(ok["unresolved"], json!(["Phone_00000000000000FF"]));
        assert!(ok["revealed"].get("Phone_00000000000000FF").is_none());
    }

    /// `reveal_tokens` is a METHOD, never a tool — so it is inherently absent
    /// from `tools/list` at BOTH tiers (the "hidden from tools/list" invariant).
    #[test]
    fn reveal_tokens_never_in_tools_list() {
        for tier in [McpTier::Master, McpTier::Agent] {
            let list = super::super::tools::tools_list(tier);
            let has = list
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t.get("name").and_then(|n| n.as_str()) == Some("reveal_tokens"));
            assert!(!has, "reveal_tokens must not be a listed tool");
        }
    }
}
