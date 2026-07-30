use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use eck_core::db::SurrealDb;
use eck_core::utils::anonymizer::{mask_person_names_heuristic, obfuscate_pii, scrub_pii_regex};
use eck_core::utils::filestore::FileStore;
use serde_json::{json, Value};
use std::io::Cursor;
use tracing::debug;

/// Compute a 128-bit MurmurHash3 of the given bytes, returned as 32-char hex string.
pub fn murmur3_hex(data: &[u8]) -> String {
    let hash = murmur3::murmur3_x64_128(&mut Cursor::new(data), 0)
        .expect("murmur3 should not fail on in-memory data");
    format!("{:032x}", hash)
}

/// Strip Zoho's per-fetch URL-signing parameters before change-detection
/// hashing. Inline-image links inside thread HTML carry `?et=<expiry-hex>` +
/// `ha=<hmac-hex>`, RE-SIGNED ON EVERY FETCH — so byte-hashing raw content
/// makes every re-fetch look like an edit. That was the root of the
/// re-summarization burn (measured 2026-07-25: one partial incremental
/// "changed" 122 stored threads, only 5 of which carried new mail — each false
/// flip re-armed the parent ticket's summary and bought a pointless Gemini
/// call; same churn also bumped the synced thread shell's vclock every run).
///
/// Applied to the SERIALIZED hash seed only — stored payloads keep the fresh
/// signed URLs so inline images still render. Deliberately narrow: only the
/// two known volatile params, in plain (`&`) or HTML-escaped (`&amp;`) form.
/// A stable-but-weird `et=`/`ha=` occurrence elsewhere is harmless — hashing
/// cares about volatility, not fidelity.
pub fn strip_volatile_sig_params(s: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?:\?|&amp;|&)(?:et|ha)=[0-9a-fA-F]+").expect("static regex")
    });
    re.replace_all(s, "").into_owned()
}

/// `murmur3_hex` over a JSON seed with volatile Zoho signing params removed.
/// EVERY change-detection hash in this module must go through here — a call
/// site that hashes the raw serialization reintroduces the false-change churn.
fn stable_seed_hash(seed: &Value) -> String {
    let s = serde_json::to_string(seed).unwrap_or_default();
    murmur3_hex(strip_volatile_sig_params(&s).as_bytes())
}

/// The change-detection hash of ONE thread payload. Hash only the stable
/// content fields — Zoho includes volatile delivery metadata in thread
/// payloads, and `content` embeds re-signed image URLs (stripped by
/// `stable_seed_hash`). Shared by `import_thread` and the one-shot
/// `restamp-thread-hashes` admin backfill, which must agree byte-for-byte or
/// the restamp is useless.
pub fn thread_source_hash(thread: &Value) -> String {
    let hash_seed = json!({
        "content": thread.get("content").and_then(|v| v.as_str()).unwrap_or(""),
        "plainText": thread.get("plainText").and_then(|v| v.as_str()).unwrap_or(""),
        "fromEmailAddress": thread.get("fromEmailAddress").and_then(|v| v.as_str()).unwrap_or(""),
        "to": thread.get("to").and_then(|v| v.as_str()).unwrap_or(""),
        "summary": thread.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
    });
    stable_seed_hash(&hash_seed)
}

/// Split a German-style address "Street 42, 12345 City" into (zip, city).
/// Returns (None, None) when the 5-digit zip boundary cannot be located.
///
/// Shared between the ticket-metadata extractor, the geocoder (so it queries
/// Nominatim with just zip+city — never the street), and the embedding-text
/// builder (so the street gets PII-masked while city/zip stay in the clear).
pub fn parse_zip_city(addr: &str) -> (Option<String>, Option<String>) {
    let bytes = addr.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 5 <= n {
        let is_digit = |b: u8| b.is_ascii_digit();
        let boundary_before = i == 0 || !is_digit(bytes[i - 1]);
        let window_digits = (0..5).all(|k| is_digit(bytes[i + k]));
        let boundary_after = i + 5 == n || !is_digit(bytes[i + 5]);
        if boundary_before && window_digits && boundary_after {
            let zip = addr[i..i + 5].to_string();
            let tail = addr[i + 5..].trim();
            let city = tail
                .trim_start_matches(|c: char| c.is_whitespace() || c == ',')
                .trim_end_matches(|c: char| c.is_whitespace() || c == ',')
                .to_string();
            return (Some(zip), if city.is_empty() { None } else { Some(city) });
        }
        i += 1;
    }
    (None, None)
}

/// Result of an import operation.
pub struct ImportResult {
    pub changed: bool,
    pub id: String,
}

/// Domain suffixes of the operating company's own support senders, from
/// `ECK_INTERNAL_SENDER_DOMAINS` (comma-separated, '@' prepended when missing,
/// lowercased). Empty when unset — no internal-domain match. Cached per process.
fn internal_sender_domains() -> &'static Vec<String> {
    static V: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        std::env::var("ECK_INTERNAL_SENDER_DOMAINS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().trim_start_matches('@').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .map(|s| format!("@{s}"))
            .collect()
    })
}

/// Lowercased Zoho custom-field labels that carry the DEVICE MODEL, from
/// `ECK_MODEL_CF_KEYS` (comma-separated). The labels are whatever the
/// deployment's Zoho admin named the fields — deployment data, not code.
/// Defaults to generic names when unset. Cached per process.
fn model_cf_keys() -> &'static Vec<String> {
    static V: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let configured: Vec<String> = std::env::var("ECK_MODEL_CF_KEYS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if configured.is_empty() {
            vec!["device model".into(), "device_model".into(), "model".into()]
        } else {
            configured
        }
    })
}

/// Decide whether a ticket's metadata is rich enough to justify seeding an
/// `ai_task`. Returns false for replies/forwards, empty descriptions, and
/// tickets whose "customer" is an internal support agent — those cases
/// waste Gemini budget on ask_human dead-ends.
fn should_seed_ai_task(meta: &Value) -> bool {
    let description = meta.get("description").and_then(|v| v.as_str()).unwrap_or("").trim();
    if description.is_empty() {
        return false;
    }

    let subject = meta.get("subject").and_then(|v| v.as_str()).unwrap_or("").trim_start();
    let lower = subject.to_ascii_lowercase();
    if lower.starts_with("re:") || lower.starts_with("fwd:") || lower.starts_with("fw:") || lower.starts_with("aw:") {
        return false;
    }

    // Internal-sender domains: mail FROM the operating company's own support
    // addresses is agent traffic, not a customer message. Deployment-specific —
    // configured via ECK_INTERNAL_SENDER_DOMAINS (comma-separated domain
    // suffixes, with or without a leading '@'; empty = no internal-domain match).
    let email = meta.get("email").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
    if internal_sender_domains().iter().any(|d| email.ends_with(d.as_str())) {
        return false;
    }
    let customer = meta.get("customer").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
    if customer.starts_with("support_") || customer == "support" {
        return false;
    }

    true
}

/// Extract lightweight metadata from a Zoho ticket payload for the synced `document` table.
/// Tokenize the ticket contact's PII inside a free-text field (the subject).
///
/// Zoho subjects routinely carry the end-customer's name ("Reparatur Erika
/// Musterfrau…") — and `meta.subject` is meshed AND returned raw by the /mcp
/// tools, so it must be pseudonymized at distillation, not at presentation
/// (the read paths have no dictionary to mask with). Same scheme as the
/// summary masker: dictionary = this ticket's structured contact, then the
/// deterministic regex backstop for emails/phones/streets. Tokens are the
/// usual keyed-SimHash ones, so reveal and cross-result correlation keep
/// working. Names that are NOT this ticket's contact (an end-customer named
/// only in the subject of a dealer's ticket) are caught by the on-device NER
/// (`SubjectNer`, when `ECK_PII_NER=local` and the model is present) with the
/// lexicon-based `mask_person_names_heuristic` as the model-less backstop; NER
/// ORG spans are PROTECTED from every masking layer (a person-shaped brand
/// like "Emil Krause Fitness" is a company and stays clear — policy + search).
/// All `(token → span)` pairs are re-derived on the reveal side
/// (`derive_reveal_pairs` + the NER augmentation in `mcp::reveal`) so
/// reveal_file keeps resolving them.
fn mask_subject(subject: &str, customer: &str, contact: &Value, ner: Option<&SubjectNer>) -> String {
    if subject.trim().is_empty() {
        return subject.to_string();
    }
    let mut entries: Vec<(&str, String)> = Vec::new();
    if !customer.trim().is_empty() {
        entries.push(("Name", customer.trim().to_string()));
    }
    // Individual name parts too ("Frau Musterfrau" carries only the last name).
    // >2 chars — a 1-2 letter fragment would corrupt unrelated words.
    for key in ["firstName", "lastName"] {
        if let Some(v) = contact.get(key).and_then(|v| v.as_str()) {
            let v = v.trim();
            if v.len() > 2 {
                entries.push(("Name", v.to_string()));
            }
        }
    }
    if let Some(v) = contact.get("email").and_then(|v| v.as_str()) {
        if !v.trim().is_empty() {
            entries.push(("Email", v.trim().to_string()));
        }
    }
    if let Some(v) = contact.get("phone").and_then(|v| v.as_str()) {
        if v.trim().len() > 4 {
            entries.push(("Phone", v.trim().to_string()));
        }
    }
    // Longest value first so "Erika Musterfrau" wins over bare "Erika".
    entries.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));

    // NER ORG spans are protected FIRST: swap each occurrence for a
    // letter-free sentinel so neither the dictionary, the NER person pass,
    // the lexicon heuristic nor the regex backstop can touch a brand name;
    // restored verbatim at the end.
    let mut out = subject.to_string();
    let mut protected: Vec<String> = Vec::new();
    if let Some(ner) = ner {
        let mut orgs: Vec<&String> = ner.orgs.iter().collect();
        orgs.sort_by_key(|v| std::cmp::Reverse(v.len()));
        for org in orgs {
            if org.chars().count() < 2 {
                continue;
            }
            if let Ok(re) = regex::Regex::new(&format!("(?i){}", regex::escape(org))) {
                while let Some(m) = re.find(&out) {
                    let sentinel = format!("\u{E100}{}\u{E101}", protected.len());
                    protected.push(out[m.range()].to_string());
                    out.replace_range(m.range(), &sentinel);
                }
            }
        }
    }

    for (label, value) in &entries {
        if out.contains(value.as_str()) {
            out = out.replace(value.as_str(), &obfuscate_pii(value, label));
        }
    }
    // NER person entities (third parties no dictionary can know), gated by the
    // deterministic plausibility check — the model mis-tags ordinary German
    // phrases as PER on short subjects ("Messungen brechen", ticket 14371),
    // and German noun capitalization makes that systematic. The token is
    // hashed over the model's canonical span so the reveal side re-derives the
    // identical pair from the raw subject.
    if let Some(ner) = ner {
        let mut persons: Vec<&String> = ner.persons.iter().collect();
        persons.sort_by_key(|v| std::cmp::Reverse(v.len()));
        for name in persons {
            if name.chars().count() < 2 {
                continue;
            }
            if !eck_core::utils::anonymizer::plausible_person_name(name) {
                continue;
            }
            if let Ok(re) = regex::Regex::new(&format!("(?i){}", regex::escape(name))) {
                let token = obfuscate_pii(name, "Name");
                out = re.replace_all(&out, token.as_str()).into_owned();
            }
        }
    }
    // Lexicon + title heuristic — the model-less backstop; after the
    // dictionary so the contact's canonical value-derived tokens win, before
    // the regex backstop.
    let (out, _pairs) = mask_person_names_heuristic(&out);
    let (mut out, _fps) = scrub_pii_regex(&out);
    for (i, org) in protected.iter().enumerate() {
        out = out.replace(&format!("\u{E100}{i}\u{E101}"), org);
    }
    out
}

/// On-device NER verdict for one raw subject line: persons to tokenize, orgs
/// to protect. Computed ASYNC (model inference) and injected into the sync
/// extraction so `extract_ticket_metadata` stays a pure, testable function.
pub struct SubjectNer {
    pub persons: Vec<String>,
    pub orgs: Vec<String>,
}

/// Run the on-device NER over the ticket's raw subject — `Some` only when
/// `ECK_PII_NER=local`, the binary carries the `local-embed` feature and the
/// model loads. Any failure degrades to `None` (= heuristic-only masking, the
/// exact pre-NER behavior) rather than blocking the import. The heuristic
/// lexicon/stoplists are DACH-tuned; the model is what generalizes this
/// beyond the first pilot customer.
pub async fn subject_ner(ticket: &Value) -> Option<SubjectNer> {
    #[cfg(feature = "local-embed")]
    {
        if crate::ai::local_ner::ner_mode() != "local" {
            return None;
        }
        let subject = ticket.get("subject").and_then(|v| v.as_str()).unwrap_or("");
        if subject.trim().is_empty() {
            return None;
        }
        match crate::ai::local_ner::extract_entities(subject).await {
            Ok((persons, orgs)) => return Some(SubjectNer { persons, orgs }),
            Err(e) => {
                tracing::warn!("[NER] subject extraction failed ({e}) — heuristic-only masking");
                return None;
            }
        }
    }
    #[cfg(not(feature = "local-embed"))]
    {
        let _ = ticket;
        None
    }
}

pub fn extract_ticket_metadata(ticket: &Value) -> Value {
    extract_ticket_metadata_with_ner(ticket, None)
}

/// `extract_ticket_metadata` with an optional pre-computed NER verdict for the
/// subject (see [`subject_ner`]). Kept sync + deterministic over its inputs:
/// same payload + same NER lists ⇒ same meta ⇒ stable `source_hash`.
pub fn extract_ticket_metadata_with_ner(ticket: &Value, ner: Option<&SubjectNer>) -> Value {
    let contact = ticket.get("contact").cloned().unwrap_or(json!({}));
    let first = contact.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
    let last = contact.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
    let mut customer = format!("{first} {last}").trim().to_string();
    if customer.is_empty() {
        customer = contact.get("fullName").and_then(|v| v.as_str()).unwrap_or("").to_string();
    }

    // Exact-match extractor for custom fields.
    //
    // Matches a field if its normalized key (lowercased, spaces/hyphens/
    // leading "cf_" stripped) equals any of the supplied keywords. Earlier
    // versions used substring matching which disastrously let short tokens
    // like "ort" bleed into `imp[ort]ed`, `sp[ort]`, `opp[ort]unity` — the
    // city field ended up holding literal "false" from `PMI Opportunity` /
    // `Imported`. Exact match + boolean-string guard prevents that.
    let normalize_key = |k: &str| -> String {
        k.trim()
            .trim_start_matches("cf_")
            .to_lowercase()
            .replace([' ', '-'], "_")
    };
    let looks_like_value = |s: &str| -> bool {
        let t = s.trim();
        !t.is_empty() && t != "null" && t != "false" && t != "true"
    };
    let find_cf = |keys: &[&str]| -> String {
        let normalized_keys: Vec<String> = keys.iter().map(|k| normalize_key(k)).collect();
        for field in ["customFields", "cf"] {
            if let Some(cfs) = ticket.get(field).and_then(|v| v.as_object()) {
                for (k, v) in cfs {
                    let kn = normalize_key(k);
                    if normalized_keys.iter().any(|nk| nk == &kn) {
                        if let Some(s) = v.as_str() {
                            if looks_like_value(s) { return s.to_string(); }
                        }
                    }
                }
            }
        }
        String::new()
    };

    // German address parser — shared helper at module level. Zoho's
    // ZetaBody-EU department mostly leaves standalone City / PLZ custom
    // fields empty; city info is embedded in the Address custom field
    // in the standard "Street 123, 12345 City" format.
    let extract_zip_city = |addr: &str| -> (String, String) {
        let (z, c) = parse_zip_city(addr);
        (z.unwrap_or_default(), c.unwrap_or_default())
    };

    // Truncate the free-text description at 2000 chars so it fits inside the
    // orchestrator's LLM context without blowing up the prompt. char-based
    // `take` is UTF-8-safe; byte-based truncate would panic mid-code-point
    // on German umlauts or multibyte symbols.
    let description_raw = ticket
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let description: String = description_raw.chars().take(2000).collect();

    let assignee = ticket.get("assignee").cloned().unwrap_or(json!({}));
    let a_first = assignee.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
    let a_last = assignee.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
    let mut assignee_name = format!("{a_first} {a_last}").trim().to_string();
    if assignee_name.is_empty() {
        assignee_name = assignee
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    let assignee_id = assignee
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            ticket
                .get("assigneeId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let address = find_cf(&["address", "adresse", "street"]);
    let mut city = find_cf(&["city", "ort", "stadt"]);
    let mut zip = find_cf(&["zip", "plz", "postcode"]);
    if zip.is_empty() || city.is_empty() {
        let (z, c) = extract_zip_city(&address);
        if zip.is_empty() { zip = z; }
        if city.is_empty() { city = c; }
    }

    json!({
        "subject": mask_subject(
            ticket.get("subject").and_then(|v| v.as_str()).unwrap_or(""),
            &customer,
            &contact,
            ner,
        ),
        "ticket_number": ticket.get("ticketNumber").and_then(|v| v.as_str()).unwrap_or(""),
        "status": ticket.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "customer": customer,
        "email": contact.get("email").and_then(|v| v.as_str()).unwrap_or(""),
        "phone": contact.get("phone").and_then(|v| v.as_str()).unwrap_or(""),
        "company": find_cf(&["company", "einrichtung"]),
        "address": address,
        "city": city,
        "zip": zip,
        "device_model": find_cf(&model_cf_keys().iter().map(String::as_str).collect::<Vec<_>>()),
        "serial_number": find_cf(&["serial", "seriennummer", "serial_number"]),
        "manufacturing_date": find_cf(&["herstellungsdatum", "manufacturing date", "manufacturing"]),
        "created_time": ticket.get("createdTime").and_then(|v| v.as_str()).unwrap_or(""),
        "description": description,
        "assignee_id": assignee_id,
        "assignee_name": assignee_name,
    })
}

/// Narrow hash over only the fields the Gemini summary actually reads.
/// `status`/assignee flip constantly on live tickets (every hourly
/// incremental returns them changed) without changing what a summary would
/// say — such a change must update meta but must NOT buy a new summary.
/// AI state is re-armed only when THIS hash moves.
///
/// Shared with `backfill-meta`: whenever a backfill rewrites meta under new
/// extraction rules (e.g. subject masking), it must ALSO restamp
/// `summary_source_hash` with this function, or the next incremental import
/// would see a hash mismatch and buy a pointless Gemini re-summary per ticket.
pub fn summary_seed_hash(meta: &Value) -> String {
    let mut summary_seed = json!({
        "subject": meta.get("subject"),
        "description": meta.get("description"),
        "customer": meta.get("customer"),
        "email": meta.get("email"),
        "phone": meta.get("phone"),
        "company": meta.get("company"),
        "address": meta.get("address"),
        "city": meta.get("city"),
        "zip": meta.get("zip"),
        "device_model": meta.get("device_model"),
        "serial_number": meta.get("serial_number"),
        "manufacturing_date": meta.get("manufacturing_date"),
    });
    // PII-policy marker: flipping a mesh to the clear LLM policy shifts this
    // hash, so every ticket re-arms a (now clear) summary on its next
    // incremental import — the accuracy upgrade the customer opted into.
    // Default (masked) contributes NOTHING so existing fleet hashes hold.
    let marker = crate::ai::pii_policy::llm_seed_marker();
    if !marker.is_empty() {
        summary_seed["pii_policy"] = json!(marker);
    }
    // stable_seed_hash: `description` is distilled from thread HTML and can
    // carry the same re-signed inline-image URLs as thread content.
    stable_seed_hash(&summary_seed)
}

/// Import or update a Zoho Desk ticket.
/// - `document` table: lightweight metadata + AI summary (synced across mesh)
/// - `document_raw` table: full Zoho payload (local only, not synced)
pub async fn import_ticket(
    db: &SurrealDb,
    ticket_id: &str,
    ticket: &Value,
    instance_id: &str,
) -> Result<ImportResult, surrealdb::Error> {
    let id_owned = ticket_id.to_string();

    // Compute hash from the distilled metadata, NOT the raw Zoho payload.
    // Zoho bumps `modifiedTime` on every fetch, which would flip the hash
    // every sync and force a full AI-state wipe even when nothing changed.
    // NER (when enabled) runs BEFORE hashing and is deterministic, so the
    // hash stays stable across incremental imports on the same node.
    let ner = subject_ner(ticket).await;
    let meta = extract_ticket_metadata_with_ner(ticket, ner.as_ref());
    // stable_seed_hash: meta.description may embed re-signed Zoho image URLs;
    // a raw hash rewrites meta + bumps the vclock on every re-fetch (pure
    // mesh churn even when the summary gate below holds).
    let new_hash = stable_seed_hash(&meta);

    let new_summary_hash = summary_seed_hash(&meta);

    // Check existing source_hash (+ vclock so a real change advances causality)
    let existing: Option<Value> = db
        .query("SELECT source_hash, summary_source_hash, _vclock FROM document WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", id_owned.clone()))
        .await?
        .take(0)?;

    let old_hash = existing
        .as_ref()
        .and_then(|v| v.get("source_hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if old_hash == new_hash {
        debug!("Ticket {} unchanged (hash={})", ticket_id, &new_hash[..8]);
        return Ok(ImportResult { changed: false, id: id_owned });
    }

    // Reaching here = synced fields (meta/source_hash) truly changed → advance
    // THIS node's vclock so the change propagates to peers (kiosk) instead of
    // being dropped as "local wins/equal". `_vclock` is hash-ignored, so this
    // does not perturb the merkle content hash. See conflict::next_local_vclock.
    let next_vclock = eck_core::sync::conflict::next_local_vclock(
        existing.as_ref().and_then(|v| v.get("_vclock")),
        instance_id,
    );

    let now = chrono::Utc::now().to_rfc3339();
    let status_str = meta["status"].as_str().unwrap_or("unknown").to_string();

    // Re-arm AI state only when summary-relevant content moved — and only to
    // 'enriching', not 'pending': the doc becomes a summarization candidate
    // when the ingest run finalizes the ticket (finalize_ticket_ingest), so
    // one run buys at most one Gemini summary per ticket. The old
    // ai_summary/embedding are deliberately KEPT serviceable until the worker
    // writes a replacement (it nulls the vector itself on the success path).
    // Wiping them here on every hash flip was the 2026-07-17 burn: the
    // cf-less list payload and the cf-carrying detail payload alternated the
    // meta hash each sync run, buying a fresh summary + re-embed per ticket
    // per run with zero real changes.
    let old_summary_hash = existing
        .as_ref()
        .and_then(|v| v.get("summary_source_hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let rearm_summary = old_summary_hash != new_summary_hash;
    let ai_fields = if rearm_summary {
        "summary_status: 'enriching', \
         summary_retries: 0, \
         summary_error: NONE, \
         summary_source_hash: $shash, "
    } else {
        ""
    };

    // UPSERT document (synced) — MERGE preserves unlisted fields. Using
    // CONTENT here would wipe *everything*, including retry counters on
    // records the Observer already marked 'failed' — breaking the circuit
    // breaker.
    let _: Option<Value> = db
        .query(format!(
            "UPSERT type::record($doc_rid) MERGE {{ \
                 type: 'support_ticket', \
                 status: $status, \
                 meta: $meta, \
                 source_hash: $hash, \
                 source_instance_id: $iid, \
                 {ai_fields}\
                 _vclock: $vclock, \
                 pii_fingerprints: NONE, \
                 updated_at: time::now() \
             }}",
        ))
        .bind(("doc_rid", format!("document:`{}`", id_owned)))
        .bind(("status", status_str))
        .bind(("meta", meta.clone()))
        .bind(("hash", new_hash.clone()))
        .bind(("shash", new_summary_hash.clone()))
        .bind(("iid", instance_id.to_string()))
        .bind(("vclock", next_vclock))
        .bind(("now", now.clone()))
        .await?
        .take(0)?;

    // UPSERT document_raw (local only) — full payload
    let _: Option<Value> = db
        .query("UPSERT type::record($raw_rid) CONTENT { type: 'support_ticket', payload: $payload, updated_at: $now }")
        .bind(("raw_rid", format!("document_raw:`{}`", id_owned)))
        .bind(("payload", ticket.clone()))
        .bind(("now", now))
        .await?
        .take(0)?;

    // Seed an `ai_task` for this ticket if one doesn't exist yet. The
    // orchestrator's LIVE SELECT on `ai_task` picks it up and dispatches
    // to the ReAct executor without any further prompting. Failures here
    // are logged but do NOT abort the import — ticket data must land even
    // if AI triage is momentarily unavailable.
    //
    // Skip noise variants that cannot produce a useful triage:
    //   - empty description (nothing for the model to reason about),
    //   - Re:/Fwd: (reply threads — real content lives in the parent,
    //     already triaged separately),
    //   - internal-sender tickets (support_* / @zetabodysupport.eu) —
    //     these are agent-side responses, not customer problems.
    // Without this, the orchestrator spends Gemini budget on tickets
    // whose only sane outcome is ask_human("give me a QC report"), which
    // deadlocks the queue and confuses operators.
    if !should_seed_ai_task(&meta) {
        debug!("Ticket {} skipped ai_task seeding (noise)", ticket_id);
    } else if let Err(e) = db
        .query(
            "LET $exists = (SELECT id FROM ai_task WHERE context.ticket_id = $tid LIMIT 1); \
             IF array::len($exists) == 0 { \
                 INSERT INTO ai_task { \
                     state: 'ready', \
                     owner_instance_id: $iid, \
                     context: { ticket_id: $tid, source: 'zoho_import', meta: $meta }, \
                     created_at: time::now(), \
                     updated_at: time::now() \
                 }; \
             };",
        )
        .bind(("tid", id_owned.clone()))
        .bind(("iid", instance_id.to_string()))
        .bind(("meta", meta))
        .await
    {
        debug!("Ticket {} ai_task seeding failed (non-fatal): {}", ticket_id, e);
    }

    debug!("Ticket {} updated (hash={})", ticket_id, &new_hash[..8]);
    Ok(ImportResult { changed: true, id: id_owned })
}

/// Import or update a Zoho Desk thread.
/// On change, marks the parent ticket for re-summarization.
pub async fn import_thread(
    db: &SurrealDb,
    thread_id: &str,
    ticket_id: &str,
    thread: &Value,
    instance_id: &str,
) -> Result<ImportResult, surrealdb::Error> {
    let new_hash = thread_source_hash(thread);
    let tid_owned = thread_id.to_string();
    let parent_owned = ticket_id.to_string();

    let existing: Option<Value> = db
        .query("SELECT source_hash, _vclock FROM document WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", tid_owned.clone()))
        .await?
        .take(0)?;

    let old_hash = existing
        .as_ref()
        .and_then(|v| v.get("source_hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if old_hash == new_hash {
        debug!("Thread {} unchanged (hash={})", thread_id, &new_hash[..8]);
        return Ok(ImportResult { changed: false, id: tid_owned });
    }

    // Synced source_hash changed → advance this node's vclock so the thread
    // document propagates (hash-ignored field; see import_ticket).
    let next_vclock = eck_core::sync::conflict::next_local_vclock(
        existing.as_ref().and_then(|v| v.get("_vclock")),
        instance_id,
    );

    let now = chrono::Utc::now().to_rfc3339();

    // UPSERT thread document (MERGE preserves any unlisted fields) and
    // conditionally mark the parent ticket 'enriching'. NOT 'pending': the
    // parent only becomes a summarization candidate when the ingest run
    // finalizes it (finalize_ticket_ingest), so N thread arrivals cost one
    // summary. The old ai_summary/embedding stay serviceable meanwhile —
    // the worker nulls the vector itself when the fresh summary lands.
    // The WHERE guards against stomping an in-flight retry cycle: only
    // terminal states (completed/failed/skipped) are re-armed. Without it,
    // every thread arrival would restart the parent's retry counter and
    // feed the infinite loop the Observer auto-mitigated.
    let _: Option<Value> = db
        .query(
             "UPSERT type::record($doc_rid) MERGE { \
                 type: 'support_thread', \
                 source_hash: $hash, \
                 ticket_id: $tid, \
                 source_instance_id: $iid, \
                 direction: $direction, \
                 created_time: $created_time, \
                 _vclock: $vclock, \
                 updated_at: time::now() \
             }; \
             UPDATE document SET \
                 summary_status = 'enriching', \
                 summary_retries = 0, \
                 summary_error = NONE, \
                 updated_at = time::now() \
             WHERE record::id(id) = $tid \
             AND summary_status IN ['completed', 'failed', 'skipped'];",
        )
        .bind(("doc_rid", format!("document:`{}`", tid_owned)))
        .bind(("hash", new_hash.clone()))
        .bind(("tid", parent_owned))
        .bind(("iid", instance_id.to_string()))
        // Lightweight, PII-free header fields carried on the SYNCED shell so
        // peers (kiosk) can render a thread list — and thus a "load" lever —
        // even though the full body (document_raw) stays scraper-local.
        .bind(("direction", thread.get("direction").and_then(|v| v.as_str()).unwrap_or("").to_string()))
        .bind(("created_time", thread.get("createdTime").and_then(|v| v.as_str()).unwrap_or("").to_string()))
        .bind(("vclock", next_vclock))
        .await?
        .take(0)?;

    // UPSERT document_raw (local only) — full thread payload. Strip attachment
    // content_base64 first: the binary already lands in the CAS filestore via
    // import_attachment, so persisting it here only bloated wms.db (a 6 MB PDF
    // became an 8 MB base64 blob per full-sync pass; main driver of the 9.5 GB
    // DB by 2026-07-23). Metadata (name/href/size) stays for the incremental
    // "download only new hrefs" plan in TECH_DEBT.
    let mut raw_payload = thread.clone();
    if let Some(atts) = raw_payload.get_mut("attachments").and_then(|v| v.as_array_mut()) {
        for att in atts.iter_mut() {
            if let Some(obj) = att.as_object_mut() {
                obj.remove("content_base64");
            }
        }
    }
    let _: Option<Value> = db
        .query("UPSERT type::record($raw_rid) CONTENT { type: 'support_thread', ticket_id: $tid, payload: $payload, updated_at: $now }")
        .bind(("raw_rid", format!("document_raw:`{}`", tid_owned)))
        .bind(("tid", ticket_id.to_string()))
        .bind(("payload", raw_payload))
        .bind(("now", now))
        .await?
        .take(0)?;

    // If this thread is outbound (agent → customer), bump the parent ticket's
    // last_outbound_at so the dashboard urgency scale measures "silence from
    // our side" instead of raw age. Only forward-monotonic updates.
    let direction = thread.get("direction").and_then(|v| v.as_str()).unwrap_or("");
    let created_time = thread.get("createdTime").and_then(|v| v.as_str()).unwrap_or("");
    if direction == "out" && !created_time.is_empty() {
        let _: Option<Value> = db
            .query(
                "UPDATE document SET meta.last_outbound_at = $t \
                 WHERE record::id(id) = $tid \
                 AND type = 'support_ticket' \
                 AND (meta.last_outbound_at IS NONE OR meta.last_outbound_at = '' OR meta.last_outbound_at < $t);"
            )
            .bind(("tid", ticket_id.to_string()))
            .bind(("t", created_time.to_string()))
            .await?
            .take(0)?;
    }

    debug!("Thread {} updated, ticket {} marked for re-summarization", thread_id, ticket_id);
    Ok(ImportResult { changed: true, id: tid_owned })
}

/// End-of-ingest gate: called once per ticket AFTER the ticket meta and all
/// its threads/attachments have landed. Flips 'enriching' → 'pending' so the
/// summarization worker sees the doc exactly once per ingest run, no matter
/// how many individual writes the run made. A no-op when nothing
/// summary-relevant changed (the doc never entered 'enriching').
///
/// Callers that can't know when a ticket is "done" (per-thread HTTP pushes)
/// simply call this after each batch — the settle window in the worker still
/// coalesces rapid successive finalizes. Docs orphaned in 'enriching' by a
/// crashed run are rescued by the worker after an hour of quiet.
pub async fn finalize_ticket_ingest(
    db: &SurrealDb,
    ticket_id: &str,
) -> Result<bool, surrealdb::Error> {
    let flipped: Vec<Value> = db
        .query(
            "UPDATE document SET summary_status = 'pending' \
             WHERE record::id(id) = $tid AND summary_status = 'enriching' \
             RETURN record::id(id) AS id",
        )
        .bind(("tid", ticket_id.to_string()))
        .await?
        .take(0)?;
    Ok(!flipped.is_empty())
}

/// Persist a Zoho thread attachment: decode base64, write to CAS filestore,
/// INSERT into file_resource (dedup by cas_uuid), RELATE document:$ticket_id
/// -> has_attachment -> file_resource.
///
/// Skips silently when `content_base64` is absent — the scraper only bundles
/// binaries when asked via `includeAttachmentContent` on full sync, so the
/// hourly incremental path naturally no-ops here.
/// Best-effort mime from a filename extension. Only the types downstream
/// consumers key on matter: the AVIF optimizer (image/jpeg, image/png) and
/// the attachment tools (application/pdf); everything else stays octet-stream.
fn mime_from_name(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

pub async fn import_attachment(
    db: &SurrealDb,
    ticket_id: &str,
    attachment: &Value,
) -> Result<(), anyhow::Error> {
    let b64 = match attachment.get("content_base64").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };
    let content = B64.decode(b64).map_err(|e| anyhow::anyhow!("base64 decode: {e}"))?;
    let name = attachment
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("attachment.bin")
        .to_string();
    // Zoho's attachment metadata carries no content type, so nearly every row
    // used to land as octet-stream — which made the AVIF optimizer skip every
    // ticket photo (it selects on mime_type IN ['image/jpeg','image/png']).
    // Fall back to the filename extension before giving up.
    let mime = attachment
        .get("content_type")
        .or_else(|| attachment.get("contentType"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| mime_from_name(&name).to_string());

    let filestore = FileStore::new(".");
    let saved = filestore
        .save(&content, &name, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("filestore.save: {e}"))?;
    let cas_id = saved.cas_uuid.to_string();

    // INSERT file_resource only if a row with the same cas_uuid doesn't
    // already exist. Without this guard, duplicate uploads (same QC report
    // on multiple tickets) would create parallel file_resource rows with
    // different SurrealDB IDs and break the RELATE dedup below.
    let avatar_b64: Option<String> = saved.avatar_data.as_ref().map(|a| B64.encode(a));
    let _: Option<Value> = db
        .query(
            "LET $exists = (SELECT id FROM file_resource WHERE cas_uuid = $cas LIMIT 1); \
             IF array::len($exists) == 0 { \
                 INSERT INTO file_resource { \
                     cas_uuid: $cas, \
                     hash: $hash, \
                     original_name: $name, \
                     mime_type: $mime, \
                     size_bytes: $size, \
                     avatar_b64: $avatar, \
                     storage_path: $path, \
                     context: 'zoho_attachment', \
                     created_at: time::now(), \
                     updated_at: time::now() \
                 }; \
             };",
        )
        .bind(("cas", cas_id.clone()))
        .bind(("hash", saved.sha256.clone()))
        .bind(("name", name.clone()))
        .bind(("mime", mime.clone()))
        .bind(("size", saved.size_bytes))
        .bind(("avatar", avatar_b64))
        .bind(("path", saved.storage_path.clone()))
        .await?
        .take(0)?;

    // RELATE document:$ticket_id -> has_attachment -> file_resource:$cas_uuid.
    // Idempotent: if the edge already exists (same ticket, same CAS), the
    // duplicate RELATE is dropped by the pre-check on has_attachment.
    let ticket_rid = format!("document:`{}`", ticket_id);
    let _: Option<Value> = db
        .query(
            "LET $fid = (SELECT id FROM file_resource WHERE cas_uuid = $cas LIMIT 1)[0].id; \
             LET $tid = type::record($trid); \
             LET $edge_exists = (SELECT id FROM has_attachment WHERE in = $tid AND out = $fid LIMIT 1); \
             IF $fid IS NOT NONE AND array::len($edge_exists) == 0 { \
                 RELATE $tid -> has_attachment -> $fid \
                     SET created_at = time::now(), label = 'zoho_attachment'; \
             };",
        )
        .bind(("cas", cas_id.clone()))
        .bind(("trid", ticket_rid))
        .await?
        .take(0)?;

    debug!(
        "Attachment saved: ticket={} cas={} size={} mime={}",
        ticket_id,
        &cas_id,
        content.len(),
        mime
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_zip_city_handles_standard_german() {
        let (z, c) = parse_zip_city("Musterstraße 42, 12345 Berlin");
        assert_eq!(z.as_deref(), Some("12345"));
        assert_eq!(c.as_deref(), Some("Berlin"));
    }

    #[test]
    fn parse_zip_city_handles_trailing_comma() {
        let (z, c) = parse_zip_city("Am Markt 1, 80331 München,");
        assert_eq!(z.as_deref(), Some("80331"));
        assert_eq!(c.as_deref(), Some("München"));
    }

    #[test]
    fn parse_zip_city_rejects_short_and_long_numbers() {
        // 4-digit and 6-digit numbers fail the boundary checks.
        let (z4, _) = parse_zip_city("Kaufpreis 1234 EUR");
        assert!(z4.is_none());
        let (z6, _) = parse_zip_city("Ref 123456 offer");
        assert!(z6.is_none());
    }

    #[test]
    fn parse_zip_city_picks_first_valid_zip() {
        let (z, c) = parse_zip_city("Street 10, 10115 Berlin ref 99999");
        assert_eq!(z.as_deref(), Some("10115"));
        assert_eq!(c.as_deref(), Some("Berlin ref 99999"));
    }

    #[test]
    fn parse_zip_city_no_match_on_plain_text() {
        let (z, c) = parse_zip_city("No address here at all");
        assert!(z.is_none());
        assert!(c.is_none());
    }

    #[test]
    fn extracted_subject_masks_contact_name() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let ticket = json!({
            "subject": "Reparatur ZetaBody 770 - Erika Musterfrau, dringend",
            "ticketNumber": "29757",
            "contact": {
                "firstName": "Erika",
                "lastName": "Musterfrau",
                "email": "erika@example.de",
            },
        });
        let meta = extract_ticket_metadata(&ticket);
        let subject = meta["subject"].as_str().unwrap();
        assert!(!subject.contains("Erika"), "first name leaked: {subject}");
        assert!(!subject.contains("Musterfrau"), "last name leaked: {subject}");
        assert!(subject.contains("Name_"), "no Name token: {subject}");
        // Non-PII words survive so subject search still works.
        assert!(subject.contains("Reparatur ZetaBody 770"), "content damaged: {subject}");
        // Deterministic: same contact in a lone-lastname subject → same token id
        // scheme, and the full-name token differs from the lastname token.
        let ticket2 = json!({
            "subject": "Rückfrage Frau Musterfrau",
            "contact": { "firstName": "Erika", "lastName": "Musterfrau" },
        });
        let meta2 = extract_ticket_metadata(&ticket2);
        let subject2 = meta2["subject"].as_str().unwrap();
        assert!(!subject2.contains("Musterfrau"), "last name leaked: {subject2}");
        assert!(subject2.starts_with("Rückfrage Frau Name_"), "unexpected shape: {subject2}");
    }

    #[test]
    fn extracted_subject_masks_third_party_name() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        // The ticket-29757 shape: the subject IS the end-customer's name while
        // the structured contact is the dealer — only the lexicon heuristic
        // can catch her. (Any lexicon first name + unknown surname works.)
        let ticket = json!({
            "subject": "Katharina Musterfrau",
            "ticketNumber": "29757",
            "contact": { "firstName": "Christian", "lastName": "Händlerkontakt" },
        });
        let meta = extract_ticket_metadata(&ticket);
        let subject = meta["subject"].as_str().unwrap();
        assert!(!subject.contains("Katharina"), "first name leaked: {subject}");
        assert!(!subject.contains("Musterfrau"), "last name leaked: {subject}");
        assert!(subject.starts_with("Name_"), "no Name token: {subject}");
    }

    #[test]
    fn extracted_subject_ner_protects_orgs_and_masks_persons() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        // NER verdict injected as a fixture: the person-shaped BRAND is an ORG
        // (protected verbatim), the end-customer — outside any lexicon — is a
        // PER (tokenized). This is the model-backed path; without a model the
        // org-marker veto in the heuristic covers the brand case only.
        let ticket = json!({
            "subject": "Analyseabbruch ZetaBody 970 - Emil Krause Fitness, Kundin Zbigniewa Kowalczyk",
            "contact": {},
        });
        let ner = SubjectNer {
            persons: vec!["Zbigniewa Kowalczyk".to_string()],
            orgs: vec!["Emil Krause Fitness".to_string()],
        };
        let meta = extract_ticket_metadata_with_ner(&ticket, Some(&ner));
        let subject = meta["subject"].as_str().unwrap();
        assert!(subject.contains("Emil Krause Fitness"), "brand damaged: {subject}");
        assert!(!subject.contains("Kowalczyk"), "person leaked: {subject}");
        assert!(subject.contains("Name_"), "no Name token: {subject}");
        assert!(subject.contains("ZetaBody 970"), "product damaged: {subject}");
        // Same input + same NER lists ⇒ byte-identical meta (hash stability).
        let meta2 = extract_ticket_metadata_with_ner(&ticket, Some(&ner));
        assert_eq!(meta, meta2);
    }

    #[test]
    fn extracted_subject_ner_phrase_false_positive_is_rejected() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        // The ticket-14371 regression: the model tags a German noun+verb pair
        // as PER; the plausibility gate must leave the subject untouched while
        // a real person in the same NER verdict is still masked.
        let ticket = json!({
            "subject": "Messungen brechen ab, Kundin Katharina Musterfrau",
            "contact": {},
        });
        let ner = SubjectNer {
            persons: vec!["Messungen brechen".to_string(), "Katharina Musterfrau".to_string()],
            orgs: vec![],
        };
        let meta = extract_ticket_metadata_with_ner(&ticket, Some(&ner));
        let subject = meta["subject"].as_str().unwrap();
        assert!(subject.starts_with("Messungen brechen ab"), "phrase masked: {subject}");
        assert!(!subject.contains("Musterfrau"), "person leaked: {subject}");
    }

    #[test]
    fn extracted_subject_brand_stays_clear_without_ner() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        // Model-less nodes: the org-marker veto alone must keep the ticket-32115
        // brand subject fully clear and searchable.
        let ticket = json!({
            "subject": "Analyseabbruch/Fehlermeldung ZetaBody 970 - Emil Krause Fitness am Schillerplatz, 1010 Wien",
            "contact": {},
        });
        let meta = extract_ticket_metadata(&ticket);
        assert_eq!(
            meta["subject"].as_str().unwrap(),
            "Analyseabbruch/Fehlermeldung ZetaBody 970 - Emil Krause Fitness am Schillerplatz, 1010 Wien"
        );
    }

    #[test]
    fn extracted_subject_scrubs_free_text_email() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        // Email in the subject that is NOT the structured contact's email —
        // the regex backstop must still tokenize it.
        let ticket = json!({
            "subject": "Bitte an office@fremdfirma.de antworten",
            "contact": {},
        });
        let meta = extract_ticket_metadata(&ticket);
        let subject = meta["subject"].as_str().unwrap();
        assert!(!subject.contains("office@fremdfirma.de"), "email leaked: {subject}");
        assert!(subject.contains("Email_"), "no Email token: {subject}");
    }

    #[test]
    fn summary_seed_hash_is_stable_for_same_meta() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let ticket = json!({
            "subject": "Reparatur - Erika Musterfrau",
            "contact": { "firstName": "Erika", "lastName": "Musterfrau" },
        });
        let h1 = summary_seed_hash(&extract_ticket_metadata(&ticket));
        let h2 = summary_seed_hash(&extract_ticket_metadata(&ticket));
        assert_eq!(h1, h2, "re-extraction must not shift the summary seed");
    }

    /// A resigned-URL ticket observed in production: Zoho re-signs inline-image
    /// URLs on every fetch, so two fetches of the SAME email differ only in
    /// `et`/`ha` query params.
    /// The thread hash must treat those as identical — every false flip
    /// re-armed the parent's summary and bought a pointless Gemini call.
    #[test]
    fn thread_hash_ignores_resigned_image_urls() {
        let fetch1 = json!({
            "content": "<img src=\"https://desk.zoho.eu/api/x/d6e345?et=19fcd71b3aa&amp;ha=78f41d6f472e5b2469b22ab50f223e10fb2a7504f8cc9081986563292f2fe19f&amp;w=1\">",
            "plainText": "", "fromEmailAddress": "k@example.de", "to": "s@example-med.de", "summary": "Fehlerbild",
        });
        let fetch2 = json!({
            "content": "<img src=\"https://desk.zoho.eu/api/x/d6e345?et=19fcd843fe8&amp;ha=77bfb838b869a10fd10b686ee9d7a6ce499c3c3ae18c6583a861ccc78d76847f&amp;w=1\">",
            "plainText": "", "fromEmailAddress": "k@example.de", "to": "s@example-med.de", "summary": "Fehlerbild",
        });
        assert_eq!(
            stable_seed_hash(&fetch1),
            stable_seed_hash(&fetch2),
            "re-signed et/ha params must not count as a content change"
        );

        // A REAL edit must still flip the hash.
        let real_change = json!({
            "content": "<img src=\"https://desk.zoho.eu/api/x/OTHERIMG?et=19fcd843fe8&amp;ha=77bf&amp;w=1\">",
            "plainText": "", "fromEmailAddress": "k@example.de", "to": "s@example-med.de", "summary": "Fehlerbild",
        });
        assert_ne!(stable_seed_hash(&fetch1), stable_seed_hash(&real_change));
    }
}
