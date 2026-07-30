//! MCP tool catalog — the WMS business-graph surface.
//!
//! Add a tool once here (one arm in [`dispatch_tool`] + one schema in
//! [`tools_list`]) and it is instantly available to every consumer of `/mcp`:
//! external Claude Code, and the server-side `claude -p` escalation once wired.
//! Tools call existing services / query the domain tables directly — they do
//! NOT re-expose raw SurrealQL (that is `/X/ops/surrealql_read`, schema-blind
//! and Zone-1-gated).

use std::sync::Arc;

use eck_core::utils::anonymizer::{
    mask_person_names_heuristic, obfuscate_pii, parse_named_addresses, parse_pii_token,
    pprl_reveal_map, scrub_pii_regex,
};
use rig::tool::Tool as _;
use serde_json::{json, Value};

use super::McpTier;
use crate::ai::tools::{
    AnalyzeQcReportArgs, AnalyzeQcReportTool, ListTicketAttachmentsArgs, ListTicketAttachmentsTool,
    SearchDatabaseArgs, SearchDatabaseTool,
};
use crate::AppState;

pub(crate) const INSTRUCTIONS: &str = "\
The 9eck WMS business-graph agent surface. Tools answer questions about \
customers, their repair/support history, devices, and warehouse state. \
Start from `customer_360` for any customer question; if it reports an \
ambiguous match, narrow with the exact email (or company) of one candidate. \
Use `device_history` for a unit's repair history by serial number, \
`ticket_search` to find tickets by subject or ticket number, \
`similar_tickets` for semantic nearest neighbors of a known ticket, and \
`search_database` for fuzzy full-text/vector lookup when exact keys fail. \
`ask_brain` delegates a whole question to the node's internal agent (metered — \
it burns mesh credit). PII (names, emails, phones, addresses) is NEVER returned \
in clear on this surface: it comes back as stable pseudonym tokens \
(`Name_<hex>`, `Email_<hex>`, `Phone_<hex>`, `Address_<hex>`) for every caller. \
The same person always yields the same token, so you can correlate across \
results — and the tokens are VALID QUERY KEYS: pass one back to `customer_360` \
(identity match) or `search_database` (exact match over every record \
referencing that person) to pivot without ever seeing the clear value. An \
authorized (master-tier) user can restore the real names/addresses \
INTO A WRITTEN FILE with the bridge's `reveal_file` tool — write your report to \
a .md file, then call `reveal_file` on its path; it resolves both live-result \
tokens and tokens baked inside stored AI summaries. Company names stay in \
clear in structured fields; inside AI-summary text a company may appear as a \
`Company_<hex>` token — treat it like any other token (reveal_file resolves \
it too).";

fn text_result(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }] })
}

fn json_result(v: &Value) -> Value {
    text_result(serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()))
}

fn error_result(msg: impl Into<String>) -> Value {
    json!({ "isError": true, "content": [{ "type": "text", "text": msg.into() }] })
}

/// The JSON-Schema tool list returned by `tools/list`, filtered to what this
/// caller may actually invoke: `surrealql_read` is Master-only.
pub(crate) fn tools_list(tier: McpTier) -> Value {
    let all = json!([
        {
            "name": "customer_360",
            "description":
                "Assemble a 360° view of one customer from their support/repair \
                 history: identity, the devices (serial numbers) they've sent, \
                 first & last contact, total number of tickets, an open/closed \
                 status breakdown, and how long open tickets have been waiting. \
                 Match by email (exact), by a name fragment (case-insensitive \
                 substring), or by a pseudonym token from any earlier result \
                 (Name_<hex>, Email_<hex>, Phone_<hex>, Address_<hex> — exact \
                 identity match). If a fragment matches several distinct \
                 customers the tool returns `ambiguous: true` with a candidate \
                 list instead of a merged view — re-query with one candidate's \
                 email token. NOTE: payment totals are not yet sourced — the \
                 `payments` field will say so until an invoice source is wired.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Customer email (exact match) or a fragment of their name/company."
                    }
                },
                "required": ["query"]
            }
        },
        {
            "name": "device_history",
            "description":
                "Full repair/support history of one device by its serial number \
                 (exact, case-insensitive): every ticket that unit appears in, \
                 chronological, with status, subject, and the customer who sent \
                 it each time — plus how many of those tickets are still open.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "serial": {
                        "type": "string",
                        "description": "Device serial number, e.g. 'F91802147'."
                    }
                },
                "required": ["serial"]
            }
        },
        {
            "name": "ticket_search",
            "description":
                "Find support tickets by ticket number (exact) or a subject \
                 fragment (case-insensitive substring), and/or filter by \
                 status: 'open', 'closed', or a literal status fragment (e.g. \
                 'escalated'). To ENUMERATE every ticket in a status, omit \
                 `query` and pass only `status` — never fake a wildcard with a \
                 single-letter query, it silently misses subjects lacking that \
                 letter. At least one of query/status is required. Returns the \
                 newest matches first. Each hit carries its `zoho_id` (the \
                 internal document id) — feed it to list_ticket_attachments to \
                 pull that ticket's files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Ticket number or a fragment of the subject line. Optional when `status` is given."
                    },
                    "status": {
                        "type": "string",
                        "description": "Optional: 'open', 'closed', or a status fragment to filter by. Alone (no query) it enumerates the whole status."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results (default 20, cap 50)."
                    }
                },
                "required": []
            }
        },
        {
            "name": "similar_tickets",
            "description":
                "Find support tickets semantically similar to a given one \
                 (nearest neighbors in embedding space via the HNSW index — \
                 works across languages, no keyword overlap needed). Use it to \
                 spot repeat issues, related cases from the same customer, or \
                 known fixes for the same symptom.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticket_number": {
                        "type": "string",
                        "description": "The source ticket number, e.g. '6219'."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max neighbors (default 5, cap 10)."
                    }
                },
                "required": ["ticket_number"]
            }
        },
        {
            "name": "search_database",
            "description":
                "Hybrid BM25 + vector search over one table when exact keys \
                 fail — the same fuzzy retriever the internal brain uses. \
                 `table`='order' finds repair/RMA records by order number, \
                 customer name, or issue description; `table`='document' finds \
                 Zoho support tickets by subject/summary content. A pseudonym \
                 token (Name_<hex>, Email_<hex>, …) as the query switches to \
                 an EXACT match — every record referencing that person. \
                 Returns up to 3 compact fuzzy matches (10 for exact token \
                 lookups) for disambiguation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Free-text fragment: order number, name, or issue description."
                    },
                    "table": {
                        "type": "string",
                        "description": "'order' or 'document'."
                    }
                },
                "required": ["query", "table"]
            }
        },
        {
            "name": "list_ticket_attachments",
            "description":
                "List files attached to a support ticket — by its Zoho ticket \
                 ID (the internal 17-digit document id / `zoho_id`) or the plain \
                 public ticket number. Returns CAS UUIDs that can be fed \
                 directly into `analyze_qc_report`, `ocr_attachment`, or \
                 `export_attachment`, each with `linked_tickets` (how many \
                 tickets share that file) and `linked_ticket_numbers` (up to 10 \
                 of their public numbers). The count alone misleads: 3 known \
                 blank templates link 180+ tickets, but a small shared count \
                 usually means one customer's document filed on several of their \
                 own tickets — check the numbers before calling a shared file a \
                 template. A classified file also carries `doc_type` \
                 (reparaturauftrag/kostenvoranschlag/invoice/shipping_label/ \
                 datenverarbeitung/qc_report/photo_screen/other) and `is_blank` \
                 (unfilled template) so you can pick the right file without \
                 OCRing every one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticket_id": {
                        "type": "string",
                        "description": "Zoho ticket ID or plain ticket number (digits only, no 'document:' prefix)."
                    }
                },
                "required": ["ticket_id"]
            }
        },
        {
            "name": "ticket_thread",
            "description":
                "Read the actual back-and-forth MESSAGE TEXT of a support ticket — \
                 what the customer and the agent literally wrote, with the quoted \
                 history of earlier messages stripped out (deterministic, no AI). \
                 Use this when the AI summary and attachments aren't enough and you \
                 need a specific reply (\"did the customer agree to the \
                 Kostenvoranschlag?\", \"what serial did they mention?\"). Accepts a \
                 public ticket number (e.g. \"31107\") or the internal document id. \
                 Returns messages oldest→newest, each with {seq, direction \
                 (in=customer→us / out=us→customer), author, from, created_time, \
                 text, method}; `method` names the extraction strategy that won. \
                 PII is TOKENIZED exactly like the other tools — `author`/`from` \
                 come back as Name_/Email_ tokens and any name/email/phone inside \
                 `text` is replaced with its stable pseudonym, so the message text \
                 is safe to read while staying correlatable and revealable. \
                 `only_new` (default true) returns just the fresh text; set it \
                 false to also get `full_text_chars` (the size of the un-stripped \
                 message) per row. The full body lives only on the node that \
                 scraped the ticket: a message whose raw payload is absent here \
                 comes back with `text: null, method: \"raw_unavailable\"` rather \
                 than failing the call. If the ticket has more than `limit` \
                 messages, the NEWEST `limit` are returned and `truncated` is set.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticket": {
                        "type": "string",
                        "description": "Public ticket number (digits) or the internal document id."
                    },
                    "only_new": {
                        "type": "boolean",
                        "description": "true (default): return only the extracted unique text. false: also return full_text_chars per message."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Max messages to return (default 20, newest kept if the thread is longer)."
                    }
                },
                "required": ["ticket"]
            }
        },
        {
            "name": "analyze_qc_report",
            "description":
                "Analyze one or more QC report files (plain-text device QC \
                 dumps) by their CAS UUIDs: extracts digital firmware, analog \
                 firmware, and serial number from each file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "CAS UUIDs of QC report files (from `list_ticket_attachments`)."
                    }
                },
                "required": ["file_ids"]
            }
        },
        {
            "name": "ocr_attachment",
            "description":
                "Extract text from ticket attachments (PDF scans / photos) by \
                 their CAS UUIDs — purely local, no AI, not metered. Tries the \
                 PDF text layer first, then decodes the page images (office- \
                 scanner CCITT G3/G4 fax, DCT JPEG, or raw bitmaps) and OCRs \
                 them. Returns per file: status \
                 ('ok'|'not_found'|'ocr_unavailable'|'encrypted'|'unsupported_codec'). \
                 'encrypted' means the file needs a real user password (owner- \
                 password PDFs are decrypted automatically and read normally); \
                 'unsupported_codec' means the page images use a codec this build \
                 cannot decode (JBIG2Decode/JPXDecode) — named in the `codec` \
                 field, bytes still intact for export_attachment. Also returns \
                 source ('text_layer'|'ocrs'|'external_cmd'), the text (capped \
                 6000 chars), a quality block (chars, lines, dictionary_ratio, \
                 garbage_ratio) to judge trustworthiness, recognized line boxes \
                 (normalized 0..1), and page count. The extract is cached on the \
                 file (with a normalized-text fingerprint that clusters re-uploaded \
                 copies of the same form) and mesh-replicated, so repeat calls are \
                 free and a node without the blob still returns the stored text. \
                 Call this BEFORE analyze_attachment_visual.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "CAS UUIDs of attachment files (from `list_ticket_attachments`)."
                    }
                },
                "required": ["file_ids"]
            }
        },
        {
            "name": "analyze_attachment_visual",
            "description":
                "Ask Gemini vision a question about ONE attachment page image. \
                 Call `ocr_attachment` FIRST — only use this when the OCR text \
                 is insufficient. A scan bypasses PII masking, so prefer the \
                 SMALLEST region that answers the question (e.g. only the \
                 signature zone) and avoid regions with names/addresses unless \
                 required. Returns {status:'ok', answer, model, tokens_in, \
                 tokens_out}; may instead return 'denied_by_policy', \
                 'not_found', or 'no_image'. Every response also carries \
                 prior_analyses: up to 5 recent earlier Q&A for this file — reuse \
                 an already-paid answer instead of asking again. METERED. Master \
                 token only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file_id": {
                        "type": "string",
                        "description": "CAS UUID of the attachment to look at."
                    },
                    "question": {
                        "type": "string",
                        "description": "A specific, self-contained question about what the image shows."
                    },
                    "page": {
                        "type": "integer",
                        "description": "Optional page index (default 0) for multi-page scan PDFs."
                    },
                    "region": {
                        "type": "object",
                        "description": "Optional relative (0..1) crop region — use the smallest that answers the question.",
                        "properties": {
                            "x": { "type": "number" },
                            "y": { "type": "number" },
                            "w": { "type": "number" },
                            "h": { "type": "number" }
                        },
                        "required": ["x", "y", "w", "h"]
                    }
                },
                "required": ["file_id", "question"]
            }
        },
        {
            "name": "export_attachment",
            "description":
                "Copy one attachment's raw bytes out of the CAS to an absolute \
                 destination path on the WMS server's disk. Master token only. \
                 ADD-ONLY: it never overwrites — a destination that already \
                 exists is an error. Follows `replaced_by` chains, so it works \
                 for files ocr_attachment cannot read (encrypted owner-password \
                 PDFs, AVIF-optimized originals) — the bytes are always intact. \
                 The sha256 is verified against the file_resource record before \
                 anything is written. `file_id` is a CAS UUID from \
                 list_ticket_attachments. Returns {status:'ok', dest_path, \
                 size_bytes, sha256, original_name, mime_type, cas_uuid}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file_id": {
                        "type": "string",
                        "description": "CAS UUID of the attachment to export (from list_ticket_attachments)."
                    },
                    "dest_path": {
                        "type": "string",
                        "description": "Absolute destination path INCLUDING the filename. Must not already exist."
                    }
                },
                "required": ["file_id", "dest_path"]
            }
        },
        {
            "name": "surrealql_read",
            "description":
                "Run a read-only SurrealQL query (SELECT / INFO / LIVE / \
                 RETURN) against the node's business database — the same \
                 governed window the ops plane uses. Zone-1 tables \
                 (credentials, PII sidecars) are refused. Master token only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "A single read-only SurrealQL statement."
                    }
                },
                "required": ["query"]
            }
        },
        {
            "name": "reveal_file_local",
            "description":
                "Server-local variant of the shim's `reveal_file` bridge tool, \
                 for when the report file lives on the WMS server's OWN disk \
                 (same machine, or a shared disk mounted there) rather than on \
                 your client machine. After writing a report to a file, call \
                 this with its ABSOLUTE server-local path to replace the \
                 pseudonym tokens (Name_…, Email_…, Phone_…, Address_…, \
                 Company_…) with the real values IN THE FILE. The real values \
                 are written to the file on the server — only counts and the \
                 output path are returned here, never the clear values. Optional \
                 `output` (must be in the SAME directory as `path`) writes a \
                 revealed copy instead of overwriting. Master token only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path, on the WMS server machine, of the file to reveal."
                    },
                    "output": {
                        "type": "string",
                        "description": "Optional: write the revealed copy here instead of overwriting `path` (must be in the same directory)."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "ask_brain",
            "description":
                "Delegate a whole question to the node's internal brain — the \
                 same Gemini agent that runs the WMS Central Brain, with its \
                 own tools (search_database, list_ticket_attachments, \
                 analyze_qc_report). Use it for multi-step questions that need \
                 reasoning across several lookups. METERED: the run is billed \
                 like any other brain work — prefer the direct tools for \
                 simple lookups.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question for the internal agent, self-contained."
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional supporting data (treated as untrusted data, max ~8k chars)."
                    }
                },
                "required": ["question"]
            }
        }
    ]);
    let tools: Vec<Value> = all
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| {
            match t.get("name").and_then(|n| n.as_str()).unwrap_or("") {
                "surrealql_read" => tier == McpTier::Master,
                "reveal_file_local" => tier == McpTier::Master,
                // Writes raw bytes to the server's disk — Master gate, like the
                // other tools that reach past the tokenized surface.
                "export_attachment" => tier == McpTier::Master,
                // Vision sends a raw scan off-node (bypasses PII masking) — same
                // Master gate as the raw-DB window.
                "analyze_attachment_visual" => tier == McpTier::Master,
                _ => true,
            }
        })
        .collect();
    json!(tools)
}

/// Route a `tools/call` to its handler. Unknown names return an MCP error result
/// (not a transport error) so the client sees a tool-shaped message.
pub(crate) async fn dispatch_tool(
    state: &Arc<AppState>,
    name: &str,
    args: &Value,
    tier: McpTier,
) -> Value {
    match name {
        "customer_360" => customer_360(state, args, tier).await,
        "device_history" => device_history(state, args, tier).await,
        "ticket_search" => ticket_search(state, args, tier).await,
        "similar_tickets" => similar_tickets(state, args, tier).await,
        "search_database" => search_database(state, args, tier).await,
        "list_ticket_attachments" => list_ticket_attachments(state, args).await,
        "ticket_thread" => ticket_thread(state, args, tier).await,
        "analyze_qc_report" => analyze_qc_report(state, args).await,
        "ocr_attachment" => ocr_attachment(state, args).await,
        "analyze_attachment_visual" => analyze_attachment_visual(state, args, tier).await,
        "export_attachment" => export_attachment(state, args, tier).await,
        "surrealql_read" => surrealql_read(state, args, tier).await,
        "reveal_file_local" => reveal_file_local(state, args, tier).await,
        // Boxed: the brain loop re-enters dispatch_tool for its tool calls
        // (ask_brain itself is whitelisted out there — no runtime recursion),
        // and the type-level cycle needs a heap indirection.
        "ask_brain" => Box::pin(ask_brain(state, args, tier)).await,
        other => error_result(format!("unknown tool '{other}'")),
    }
}

// ─── PII pseudonymization (ALL tiers) ──────────────────────────────────────

/// Replace a clear PII value with its stable pseudonym token — the SAME
/// keyed-SimHash scheme (`obfuscate_pii`, peppered with the mesh `SYNC_SECRET`)
/// the summaries and embeddings already ride, so tokens are stable fleet-wide
/// and identical inputs always collapse to one token. The reverse mapping is
/// stashed in the node's ephemeral reveal store so a master `reveal_tokens`
/// call can restore it into a file. Emitted for EVERY tier — no clear PII ever
/// leaves an MCP tool result. Empty input passes through unchanged.
fn tokenize(state: &Arc<AppState>, value: &str, pii_type: &str) -> String {
    if value.trim().is_empty() {
        return value.to_string();
    }
    let token = obfuscate_pii(value, pii_type);
    state.pii_reveal.record(token.clone(), value.to_string());
    token
}

/// Tokenize FREE TEXT (a message body) the same way the summarizer does: heuristic
/// person-name masking first, then the deterministic regex scrub for
/// emails/phones/IBANs/cards. Both passes emit the SAME `obfuscate_pii` tokens as
/// [`tokenize`], so a name in a body collapses to the identical `Name_<hex>` token
/// as the structured `author` field — correlatable and revealable. Every produced
/// (token → clear) pair is stashed in the reveal store so a master `reveal_tokens`
/// / `reveal_file` can restore it. No clear PII survives this function.
fn mask_free_text(state: &Arc<AppState>, text: &str) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    let (named, name_pairs) = mask_person_names_heuristic(text);
    for (tok, orig) in name_pairs {
        state.pii_reveal.record(tok, orig);
    }
    // Record the clear originals of the regex-detectable PII BEFORE scrubbing, so
    // the same tokens the scrub produces are resolvable at reveal time.
    for (tok, orig) in pprl_reveal_map(&named) {
        state.pii_reveal.record(tok, orig);
    }
    scrub_pii_regex(&named).0
}

/// Display name of a thread message's author (`author.name`, else
/// `firstName + lastName`). Empty when the payload carries neither.
fn payload_author_name(payload: &Value) -> String {
    let author = payload.get("author");
    author
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            let fname = author
                .and_then(|a| a.get("firstName"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let lname = author
                .and_then(|a| a.get("lastName"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("{fname} {lname}").trim().to_string()
        })
}

/// Build `(needle_lowercase, token)` pairs for every name KNOWN to take part in
/// this thread — the structured author display names plus the display names on
/// the `fromEmailAddress` headers — including their individual parts.
///
/// Why this exists: `mask_person_names_heuristic` only recognises capitalised
/// *multi-word* names, so a body that greets or signs with a BARE FIRST NAME
/// ("Hallo Phil", "-Phil") would leak it in clear — which the contract of this
/// surface forbids. Here we already know the participants from structured
/// fields, so a bare part can be masked with full precision, and it maps to the
/// token of the FULL name so correlation and reveal keep working.
///
/// Parts shorter than 3 chars are skipped (initials would match everywhere).
/// Over-masking a same-spelled common noun is accepted: for PII that is the
/// safe direction to err.
fn thread_name_needles(state: &Arc<AppState>, payloads: &[&Value]) -> Vec<(String, String)> {
    let mut names: Vec<String> = Vec::new();
    for payload in payloads {
        let n = payload_author_name(payload);
        if !n.is_empty() {
            names.push(n);
        }
        if let Some(hdr) = payload.get("fromEmailAddress").and_then(|v| v.as_str()) {
            for (disp, _) in parse_named_addresses(hdr) {
                let d = disp.trim();
                if !d.is_empty() && !d.contains('@') {
                    names.push(d.to_string());
                }
            }
        }
    }

    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for full in names {
        let token = tokenize(state, &full, "Name");
        for cand in std::iter::once(full.as_str()).chain(full.split_whitespace()) {
            let c = cand.trim().trim_matches(|ch: char| !ch.is_alphanumeric());
            if c.chars().count() < 3 {
                continue;
            }
            let key = c.to_lowercase();
            if seen.insert(key.clone()) {
                pairs.push((key, token.clone()));
            }
        }
    }
    // Longest first: "Phil Weber" must win before the bare "Phil".
    pairs.sort_by_key(|(n, _)| std::cmp::Reverse(n.chars().count()));
    pairs
}

/// Replace each known name/part with its token, matching case-insensitively on
/// WORD BOUNDARIES so "Phil" does not fire inside "Philosophie". Applied after
/// [`mask_free_text`], on text that already contains tokens — those are matched
/// as ordinary words and never re-tokenized.
fn mask_known_names(text: &str, needles: &[(String, String)]) -> String {
    if needles.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for (needle, token) in needles {
        // Single left-to-right pass: the cursor always advances PAST the text we
        // just wrote, so an inserted token is never re-scanned. (Rescanning
        // would loop forever on a name that reappears inside its own token —
        // someone literally called "Name" would regrow `Name_<hex>` each round.)
        let hay = out.to_lowercase();
        // Byte offsets are taken from the lowercased copy; bail out if case
        // folding changed the length (ẞ→ss), rather than slicing wrongly.
        if hay.len() != out.len() {
            continue;
        }
        let mut result = String::with_capacity(out.len());
        let mut cursor = 0usize;
        while let Some(pos) = find_word(&hay, needle, cursor) {
            let end = pos + needle.len();
            if !out.is_char_boundary(pos) || !out.is_char_boundary(end) {
                break;
            }
            result.push_str(&out[cursor..pos]);
            result.push_str(token);
            cursor = end;
        }
        if cursor > 0 {
            result.push_str(&out[cursor..]);
            out = result;
        }
    }
    out
}

/// First byte offset at or after `from` in `hay` (a lowercased copy) where
/// `needle` occurs as a whole word — the neighbouring characters must not be
/// alphanumeric, so "Phil" never fires inside "Philosophie".
fn find_word(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let mut from = from;
    if from > hay.len() || !hay.is_char_boundary(from) {
        return None;
    }
    while let Some(rel) = hay[from..].find(needle) {
        let pos = from + rel;
        let end = pos + needle.len();
        let before_ok = hay[..pos].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after_ok = hay[end..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return Some(pos);
        }
        from = end;
        if from >= hay.len() {
            break;
        }
    }
    None
}

// ─── PII masking (legacy; non-MCP callers only — MCP tools now tokenize) ────

/// `john.doe@acme.de` → `j***@acme.de`; short/!valid → `***`.
#[allow(dead_code)]
fn mask_email(s: &str) -> String {
    match s.split_once('@') {
        Some((local, domain)) if !local.is_empty() => {
            format!("{}***@{}", &local[..1], domain)
        }
        _ => "***".into(),
    }
}

/// Keep the first character of each whitespace-separated part: `Anna Müller`
/// → `A*** M***`.
#[allow(dead_code)]
fn mask_name(s: &str) -> String {
    let parts: Vec<String> = s
        .split_whitespace()
        .filter(|p| !p.is_empty())
        .map(|p| format!("{}***", &p.chars().next().unwrap()))
        .collect();
    if parts.is_empty() {
        "***".into()
    } else {
        parts.join(" ")
    }
}

/// Last two digits only: `+49 170 1234567` → `***67`.
#[allow(dead_code)]
fn mask_phone(s: &str) -> String {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 2 {
        format!("***{}", &digits[digits.len() - 2..])
    } else {
        "***".into()
    }
}

// ─── customer_360 ──────────────────────────────────────────────────────────

fn meta_str<'a>(meta: &'a Value, key: &str) -> &'a str {
    meta.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Which real-world customer a ticket belongs to, for grouping fragment
/// matches: email wins (stable across typo'd names), then contact name, then
/// company. Tickets with none of the three collapse into one anonymous bucket.
fn identity_key(meta: &Value) -> String {
    let email = meta_str(meta, "email").trim().to_ascii_lowercase();
    if !email.is_empty() {
        return format!("e:{email}");
    }
    let name = meta_str(meta, "customer").trim().to_ascii_lowercase();
    if !name.is_empty() {
        return format!("n:{name}");
    }
    format!("c:{}", meta_str(meta, "company").trim().to_ascii_lowercase())
}

/// Whether a ticket status counts as "done". Substring match (case-insensitive)
/// so Zoho variants like "Auto closed" / "Closed - resolved" are caught — same
/// approach as `services`/`repair_distiller`. German equivalents included for
/// the EU support desk.
fn is_closed(status: &str) -> bool {
    let s = status.to_ascii_lowercase();
    ["closed", "resolved", "geschlossen", "gelöst", "erledigt"]
        .iter()
        .any(|m| s.contains(m))
}

async fn customer_360(state: &Arc<AppState>, args: &Value, _tier: McpTier) -> Value {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return error_result("missing required argument `query`"),
    };

    // Pull the distilled meta of every support ticket matching this customer.
    // `document.meta` is the synced, distilled shell (Zone-2) — email/name are
    // stored here in clear; we mask at presentation for the Agent tier.
    //
    // A pseudonym token as the query (`Name_<hex>`, `Email_<hex>`, …) is the
    // agent tier's ONLY handle on a person — every tool tokenizes identity on
    // output — so it must round-trip as a query key. obfuscate_pii is
    // deterministic, so we re-derive each ticket's token for the matching
    // identity field in Rust and compare (no plaintext reverse map needed,
    // works across restarts). Non-token queries keep the DB-side match.
    let rows: Vec<Value> = if let Some(label) = parse_pii_token(&query) {
        let field = match label {
            "Name" => "customer",
            "Email" => "email",
            "Phone" => "phone",
            "Address" => "address",
            _ => {
                return error_result(format!(
                    "a {label}_ token is not a customer identity — query with a \
                     Name_/Email_/Phone_/Address_ token, an email, or a name/company fragment"
                ))
            }
        };
        match state
            .db
            .query("SELECT meta FROM document WHERE type = 'support_ticket'")
            .await
            .and_then(|mut r| r.take::<Vec<Value>>(0))
        {
            Ok(all) => all
                .into_iter()
                .filter(|r| {
                    r.get("meta")
                        .and_then(|m| m.get(field))
                        .and_then(|v| v.as_str())
                        .map(|v| v.trim())
                        .filter(|v| !v.is_empty())
                        .is_some_and(|v| obfuscate_pii(v, label) == query)
                })
                .collect(),
            Err(e) => return error_result(format!("query failed: {e}")),
        }
    } else {
        match state
            .db
            .query(
                // No ORDER BY: SurrealDB requires an order idiom to appear in the
                // projection, but we project the whole `meta` object. Row counts per
                // customer are small, so we sort in Rust below.
                "SELECT meta FROM document \
                 WHERE type = 'support_ticket' \
                   AND (meta.email = $q \
                     OR string::contains(string::lowercase(meta.customer ?? ''), string::lowercase($q)) \
                     OR string::contains(string::lowercase(meta.company ?? ''), string::lowercase($q)))",
            )
            .bind(("q", query.clone()))
            .await
            .and_then(|mut r| r.take(0))
        {
            Ok(v) => v,
            Err(e) => return error_result(format!("query failed: {e}")),
        }
    };

    if rows.is_empty() {
        return json_result(&json!({
            "query": query,
            "found": false,
            "message": "no support tickets match this customer",
        }));
    }

    // Sort by created_time DESC in Rust (ISO-8601 → lexical == chronological).
    let mut metas: Vec<&Value> = rows.iter().filter_map(|r| r.get("meta")).collect();
    metas.sort_by(|a, b| meta_str(b, "created_time").cmp(meta_str(a, "created_time")));

    // A fragment like "gmbh" can match hundreds of unrelated companies; merging
    // them into one 360 view is a lie. Group by identity and, when more than
    // one distinct customer matches, return a disambiguation list instead.
    let mut groups: std::collections::BTreeMap<String, Vec<&Value>> = Default::default();
    for m in &metas {
        groups.entry(identity_key(m)).or_default().push(m);
    }
    if groups.len() > 1 {
        // Vec order inside each group follows `metas` (created_time DESC), so
        // [0] is that customer's latest ticket.
        let mut candidates: Vec<Value> = groups
            .values()
            .map(|g| {
                let latest = g[0];
                let name = meta_str(latest, "customer");
                let email = meta_str(latest, "email");
                json!({
                    "name": tokenize(state, name, "Name"),
                    "email": tokenize(state, email, "Email"),
                    "company": meta_str(latest, "company"),
                    "tickets": g.len(),
                    "last_contact": meta_str(latest, "created_time"),
                })
            })
            .collect();
        candidates.sort_by(|a, b| b["tickets"].as_u64().cmp(&a["tickets"].as_u64()));
        let matched = candidates.len();
        candidates.truncate(20);
        return json_result(&json!({
            "query": query,
            "found": true,
            "ambiguous": true,
            "matched_customers": matched,
            "candidates": candidates,
            "hint": "Multiple distinct customers match this fragment. Re-query \
                     customer_360 with one candidate's `email` value — the \
                     Email_<hex> token IS a valid query key — or a company \
                     fragment unique to them.",
        }));
    }

    // Identity from the most recent ticket.
    let latest = metas.first().copied().unwrap_or(&Value::Null);
    let name_raw = meta_str(latest, "customer");
    let email_raw = meta_str(latest, "email");
    let phone_raw = meta_str(latest, "phone");
    let address_raw = meta_str(latest, "address");
    let identity = json!({
        "name": tokenize(state, name_raw, "Name"),
        "email": tokenize(state, email_raw, "Email"),
        "phone": tokenize(state, phone_raw, "Phone"),
        "address": tokenize(state, address_raw, "Address"),
        "company": meta_str(latest, "company"),
    });

    // Devices: distinct non-empty serials, with the model last seen for each.
    let mut devices: std::collections::BTreeMap<String, String> = Default::default();
    for m in &metas {
        let serial = meta_str(m, "serial_number");
        if !serial.is_empty() {
            devices
                .entry(serial.to_string())
                .or_insert_with(|| meta_str(m, "device_model").to_string());
        }
    }
    let devices_json: Vec<Value> = devices
        .iter()
        .map(|(serial, model)| json!({ "serial": serial, "model": model }))
        .collect();

    // Ticket aggregates.
    let total = metas.len();
    let mut open = 0usize;
    let mut open_tickets: Vec<Value> = Vec::new();
    let mut status_counts: std::collections::BTreeMap<String, usize> = Default::default();
    let now = chrono::Utc::now();

    for m in &metas {
        let status = meta_str(m, "status");
        *status_counts.entry(status.to_string()).or_default() += 1;
        if !is_closed(status) {
            open += 1;
            // Days waiting = now − created_time (RFC3339); best-effort.
            let created = meta_str(m, "created_time");
            let waiting_days = chrono::DateTime::parse_from_rfc3339(created)
                .ok()
                .map(|c| (now - c.with_timezone(&chrono::Utc)).num_days());
            open_tickets.push(json!({
                "ticket_number": meta_str(m, "ticket_number"),
                "subject": meta_str(m, "subject"),
                "status": status,
                "serial": meta_str(m, "serial_number"),
                "created_time": created,
                "waiting_days": waiting_days,
            }));
        }
    }

    // created_time is ISO-8601, so lexical min/max == chronological.
    let created_times: Vec<&str> = metas
        .iter()
        .map(|m| meta_str(m, "created_time"))
        .filter(|s| !s.is_empty())
        .collect();
    let first_contact = created_times.iter().min().copied().unwrap_or("");
    let last_contact = created_times.iter().max().copied().unwrap_or("");

    json_result(&json!({
        "query": query,
        "found": true,
        "ambiguous": false,
        "pii_revealed": false,
        "identity": identity,
        "devices": devices_json,
        "device_count": devices.len(),
        "tickets": {
            "total": total,
            "open": open,
            "closed": total - open,
            "status_breakdown": status_counts,
            "first_contact": first_contact,
            "last_contact": last_contact,
        },
        "open_tickets": open_tickets,
        "payments": "NO DATA SOURCE YET — order.total_cost is unpopulated and tickets carry no amount; wire an invoice source (Odoo/Zoho) before trusting spend figures.",
    }))
}

// ─── device_history ────────────────────────────────────────────────────────

async fn device_history(state: &Arc<AppState>, args: &Value, _tier: McpTier) -> Value {
    let serial = match args.get("serial").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return error_result("missing required argument `serial`"),
    };

    let rows: Vec<Value> = match state
        .db
        .query(
            "SELECT meta FROM document \
             WHERE type = 'support_ticket' \
               AND string::lowercase(meta.serial_number ?? '') = string::lowercase($serial)",
        )
        .bind(("serial", serial.clone()))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(v) => v,
        Err(e) => return error_result(format!("query failed: {e}")),
    };

    if rows.is_empty() {
        return json_result(&json!({
            "serial": serial,
            "found": false,
            "message": "no support tickets reference this serial number",
        }));
    }

    // Chronological ASC — it's a history.
    let mut metas: Vec<&Value> = rows.iter().filter_map(|r| r.get("meta")).collect();
    metas.sort_by(|a, b| meta_str(a, "created_time").cmp(meta_str(b, "created_time")));

    let mut open = 0usize;
    let mut models: std::collections::BTreeSet<String> = Default::default();
    let tickets: Vec<Value> = metas
        .iter()
        .map(|m| {
            let status = meta_str(m, "status");
            if !is_closed(status) {
                open += 1;
            }
            let model = meta_str(m, "device_model");
            if !model.is_empty() {
                models.insert(model.to_string());
            }
            let name = meta_str(m, "customer");
            let email = meta_str(m, "email");
            let address = meta_str(m, "address");
            json!({
                "ticket_number": meta_str(m, "ticket_number"),
                "subject": meta_str(m, "subject"),
                "status": status,
                "created_time": meta_str(m, "created_time"),
                "customer": tokenize(state, name, "Name"),
                "email": tokenize(state, email, "Email"),
                "address": tokenize(state, address, "Address"),
                "company": meta_str(m, "company"),
            })
        })
        .collect();

    json_result(&json!({
        "serial": serial,
        "found": true,
        "pii_revealed": false,
        "models": models,
        "tickets_total": tickets.len(),
        "tickets_open": open,
        "tickets": tickets,
    }))
}

// ─── ticket_search ─────────────────────────────────────────────────────────

async fn ticket_search(state: &Arc<AppState>, args: &Value, _tier: McpTier) -> Value {
    // `query` is optional WHEN a status filter is given (defect 68: agents used
    // a single letter as a pseudo-wildcard to enumerate by status and silently
    // missed every ticket whose subject lacked that letter — three query-less
    // calls with different letters MUST yield the same set; now they can just
    // omit the query). With neither query nor status there is nothing to
    // select by — that stays an error.
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(String::from);
    let status_filter = args
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    if query.is_none() && status_filter.is_none() {
        return error_result("provide `query`, `status`, or both — with neither there is nothing to select by");
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50) as usize;

    let rows: Vec<Value> = match match &query {
        Some(q) => {
            state
                .db
                .query(
                    "SELECT meta, type::string(record::id(id)) AS zoho_id FROM document \
                     WHERE type = 'support_ticket' \
                       AND (meta.ticket_number = $q \
                         OR string::contains(string::lowercase(meta.subject ?? ''), string::lowercase($q)))",
                )
                .bind(("q", q.clone()))
                .await
        }
        None => {
            state
                .db
                .query(
                    "SELECT meta, type::string(record::id(id)) AS zoho_id FROM document \
                     WHERE type = 'support_ticket'",
                )
                .await
        }
    }
    .and_then(|mut r| r.take(0))
    {
        Ok(v) => v,
        Err(e) => return error_result(format!("query failed: {e}")),
    };

    // Newest first, then apply the status filter and cap. Carry each ticket's
    // document record id (`zoho_id`) alongside its meta so the hit can name it.
    let mut hits: Vec<(&Value, &str)> = rows
        .iter()
        .filter_map(|r| {
            let meta = r.get("meta")?;
            let zoho_id = r.get("zoho_id").and_then(|v| v.as_str()).unwrap_or("");
            Some((meta, zoho_id))
        })
        .collect();
    hits.sort_by(|a, b| meta_str(b.0, "created_time").cmp(meta_str(a.0, "created_time")));

    let matched_total = hits.len();
    let tickets: Vec<Value> = hits
        .iter()
        .filter(|(m, _)| {
            let status = meta_str(m, "status");
            match status_filter.as_deref() {
                None => true,
                Some("open") => !is_closed(status),
                Some("closed") => is_closed(status),
                Some(f) => status.to_ascii_lowercase().contains(f),
            }
        })
        .take(limit)
        .map(|(m, zoho_id)| {
            let name = meta_str(m, "customer");
            let email = meta_str(m, "email");
            let address = meta_str(m, "address");
            json!({
                "ticket_number": meta_str(m, "ticket_number"),
                "zoho_id": zoho_id,
                "subject": meta_str(m, "subject"),
                "status": meta_str(m, "status"),
                "created_time": meta_str(m, "created_time"),
                "serial": meta_str(m, "serial_number"),
                "model": meta_str(m, "device_model"),
                "customer": tokenize(state, name, "Name"),
                "email": tokenize(state, email, "Email"),
                "address": tokenize(state, address, "Address"),
                "company": meta_str(m, "company"),
            })
        })
        .collect();

    json_result(&json!({
        "query": query,
        "status_filter": status_filter,
        "matched_before_status_filter": matched_total,
        "returned": tickets.len(),
        "pii_revealed": false,
        "tickets": tickets,
    }))
}

// ─── similar_tickets ───────────────────────────────────────────────────────

async fn similar_tickets(state: &Arc<AppState>, args: &Value, _tier: McpTier) -> Value {
    let nr = match args.get("ticket_number").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => return error_result("missing required argument `ticket_number`"),
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .clamp(1, 10) as usize;

    // Source vector.
    let vecs: Vec<Vec<f32>> = match state
        .db
        .query(
            "SELECT VALUE embedding FROM document \
             WHERE type = 'support_ticket' AND meta.ticket_number = $nr \
               AND embedding IS NOT NONE LIMIT 1",
        )
        .bind(("nr", nr.clone()))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(v) => v,
        Err(e) => return error_result(format!("query failed: {e}")),
    };
    let Some(vec) = vecs.into_iter().next() else {
        return json_result(&json!({
            "ticket_number": nr,
            "found": false,
            "message": "ticket not found or not embedded yet",
        }));
    };

    // KNN over the HNSW index; the source ticket is its own nearest neighbor,
    // so over-fetch by one and drop it below.
    let k = limit + 1;
    let rows: Vec<Value> = match state
        .db
        .query(format!(
            "SELECT meta.ticket_number AS ticket_number, meta.subject AS subject, \
                    meta.status AS status, meta.serial_number AS serial, \
                    meta.device_model AS model, meta.customer AS customer, \
                    meta.company AS company, meta.address AS address, \
                    meta.created_time AS created_time, \
                    vector::distance::knn() AS distance \
             FROM document WHERE embedding <|{k},100|> $vec AND type = 'support_ticket'"
        ))
        .bind(("vec", vec))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(v) => v,
        Err(e) => return error_result(format!("knn query failed: {e}")),
    };

    let neighbors: Vec<Value> = rows
        .into_iter()
        .filter(|r| r.get("ticket_number").and_then(|v| v.as_str()) != Some(nr.as_str()))
        .take(limit)
        .map(|r| {
            let get = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let customer = get("customer");
            let address = get("address");
            json!({
                "ticket_number": get("ticket_number"),
                "subject": get("subject"),
                "status": get("status"),
                "serial": get("serial"),
                "model": get("model"),
                "customer": tokenize(state, &customer, "Name"),
                "address": tokenize(state, &address, "Address"),
                "company": get("company"),
                "created_time": get("created_time"),
                "distance": r.get("distance").and_then(|v| v.as_f64()),
            })
        })
        .collect();

    json_result(&json!({
        "ticket_number": nr,
        "found": true,
        "pii_revealed": false,
        "neighbors": neighbors,
    }))
}

// ─── ticket_thread ───────────────────────────────────────────────────────────
//
// Reads the deterministic per-message body (crate::services::thread_body) from
// the local `document_raw` payloads, keyed off the SYNCED `support_thread`
// shells, and tokenizes every free-text/identity field exactly like the other
// tools on this surface. The heavy body text stays local (never synced), so a
// node missing a raw row reports `raw_unavailable` for that message instead of
// failing the whole call.

/// Resolve a `ticket` argument (public number, bare document id, or a
/// `document:<id>` record string) to the parent ticket's bare document id — the
/// value stored in each thread shell's `ticket_id`. Mirrors
/// `ai::tools::ListTicketAttachmentsTool::resolve_ticket_id`.
async fn resolve_ticket_doc_id(state: &Arc<AppState>, input: &str) -> String {
    let t = input
        .trim()
        .trim_start_matches("document:")
        .trim_matches('`')
        .trim();
    if !t.is_empty() && t.len() < 12 && t.bytes().all(|b| b.is_ascii_digit()) {
        let ids: Vec<Value> = state
            .db
            .query(
                "SELECT VALUE type::string(record::id(id)) FROM document \
                 WHERE type = 'support_ticket' AND meta.ticket_number = $tn LIMIT 1",
            )
            .bind(("tn", t.to_string()))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        if let Some(id) = ids.into_iter().next().and_then(|v| v.as_str().map(String::from)) {
            return id;
        }
    }
    t.to_string()
}

async fn ticket_thread(state: &Arc<AppState>, args: &Value, _tier: McpTier) -> Value {
    let ticket_arg = match args.get("ticket").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return error_result("missing required argument `ticket`"),
    };
    let only_new = args.get("only_new").and_then(|v| v.as_bool()).unwrap_or(true);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 100) as usize;

    let parent_id = resolve_ticket_doc_id(state, &ticket_arg).await;

    // Synced shells: lightweight header per thread (exist mesh-wide).
    let shells: Vec<Value> = match state
        .db
        .query(
            "SELECT type::string(record::id(id)) AS thread_id, direction, created_time \
             FROM document WHERE type = 'support_thread' AND ticket_id = $tid",
        )
        .bind(("tid", parent_id.clone()))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(v) => v,
        Err(e) => return error_result(format!("query failed: {e}")),
    };

    if shells.is_empty() {
        return json_result(&json!({
            "ticket": ticket_arg,
            "parent_document_id": parent_id,
            "found": false,
            "message": "no support-thread messages for this ticket (wrong number/id, \
                        or the ticket predates thread capture)",
        }));
    }

    // Local-only raw payloads (full body text). May be absent on a peer node.
    let raws: Vec<Value> = state
        .db
        .query(
            "SELECT type::string(record::id(id)) AS thread_id, payload \
             FROM document_raw WHERE type = 'support_thread' AND ticket_id = $tid",
        )
        .bind(("tid", parent_id.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let mut raw_map: std::collections::HashMap<&str, &Value> = std::collections::HashMap::new();
    for r in &raws {
        if let (Some(tid), Some(payload)) =
            (r.get("thread_id").and_then(|v| v.as_str()), r.get("payload"))
        {
            raw_map.insert(tid, payload);
        }
    }

    // Chronological ASC (created_time is ISO-8601 → lexical == chronological).
    let mut ordered: Vec<&Value> = shells.iter().collect();
    ordered.sort_by(|a, b| {
        let ca = a.get("created_time").and_then(|v| v.as_str()).unwrap_or("");
        let cb = b.get("created_time").and_then(|v| v.as_str()).unwrap_or("");
        ca.cmp(cb)
    });

    let total = ordered.len();
    let truncated = total > limit;
    let base = if truncated { total - limit } else { 0 };
    let window = &ordered[base..];

    // Pre-pass: every participant name in this thread, so a body that greets or
    // signs with a bare first name gets masked too (the generic heuristic only
    // catches capitalised multi-word names).
    let window_payloads: Vec<&Value> = window
        .iter()
        .filter_map(|s| s.get("thread_id").and_then(|v| v.as_str()))
        .filter_map(|tid| raw_map.get(tid).copied())
        .collect();
    let name_needles = thread_name_needles(state, &window_payloads);

    let mut messages: Vec<Value> = Vec::with_capacity(window.len());
    // Earlier extracted bodies feed thread_body's marker-less subtraction guard.
    let mut prev_texts: Vec<String> = Vec::new();

    for (i, shell) in window.iter().enumerate() {
        let seq = base + i + 1;
        let thread_id = shell.get("thread_id").and_then(|v| v.as_str()).unwrap_or("");
        let shell_dir = shell.get("direction").and_then(|v| v.as_str()).unwrap_or("");
        let shell_ct = shell.get("created_time").and_then(|v| v.as_str()).unwrap_or("");

        let Some(payload) = raw_map.get(thread_id) else {
            messages.push(json!({
                "seq": seq,
                "direction": shell_dir,
                "author": Value::Null,
                "from": Value::Null,
                "created_time": shell_ct,
                "text": Value::Null,
                "method": "raw_unavailable",
            }));
            continue;
        };

        let content = payload.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let summary = payload
            .get("summary")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let direction = payload
            .get("direction")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(shell_dir);
        let created_time = if !shell_ct.is_empty() {
            shell_ct
        } else {
            payload.get("createdTime").and_then(|v| v.as_str()).unwrap_or("")
        };

        // Author display name (structured) → Name token; sender email → Email token.
        let author = payload.get("author");
        let author_name = payload_author_name(payload);
        let author_email = author
            .and_then(|a| a.get("email"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let from_email = author_email.clone().or_else(|| {
            payload
                .get("fromEmailAddress")
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    parse_named_addresses(s)
                        .into_iter()
                        .map(|(_, e)| e)
                        .find(|e| !e.is_empty())
                })
        });

        let prev_refs: Vec<&str> = prev_texts.iter().map(String::as_str).collect();
        let extracted =
            crate::services::thread_body::extract_unique_body(content, summary, &prev_refs);
        prev_texts.push(extracted.text.clone());

        let text_field = if extracted.text.trim().is_empty() {
            Value::Null
        } else {
            let masked = mask_free_text(state, &extracted.text);
            json!(mask_known_names(&masked, &name_needles))
        };
        let author_field = if author_name.is_empty() {
            Value::Null
        } else {
            json!(tokenize(state, &author_name, "Name"))
        };
        let from_field = match from_email {
            Some(ref e) if !e.is_empty() => json!(tokenize(state, e, "Email")),
            _ => Value::Null,
        };

        let mut msg = json!({
            "seq": seq,
            "direction": direction,
            "author": author_field,
            "from": from_field,
            "created_time": created_time,
            "text": text_field,
            "method": extracted.method,
        });
        if !only_new {
            let full_chars =
                crate::services::thread_body::html_to_text(content).chars().count();
            msg["full_text_chars"] = json!(full_chars);
        }
        messages.push(msg);
    }

    json_result(&json!({
        "ticket": ticket_arg,
        "parent_document_id": parent_id,
        "found": true,
        "pii_revealed": false,
        "thread_count": total,
        "returned": messages.len(),
        "truncated": truncated,
        "only_new": only_new,
        "messages": messages,
    }))
}

// ─── Internal-brain tool mirrors ───────────────────────────────────────────
//
// The owner's rule: everything the internal brain can do must be reachable
// over /mcp. These adapters call the SAME implementations the Gemini
// orchestrator registers (crate::ai::tools), so the two surfaces can't drift.
// (`ask_human` is deliberately not mirrored — it pauses an `ai_task` and has
// no meaning outside the orchestrator loop.)

async fn search_database(state: &Arc<AppState>, args: &Value, _tier: McpTier) -> Value {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return error_result("missing required argument `query`"),
    };
    let table = match args.get("table").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return error_result("missing required argument `table`"),
    };

    let tool = SearchDatabaseTool { db: state.db.clone() };
    match tool.call(SearchDatabaseArgs { query, table }).await {
        Ok(mut out) => {
            // Same PII rule as everywhere on this surface: `order` rows carry
            // `customer_name` in clear — tokenize it for EVERY tier. (The
            // `document` projection carries no name field; its `ai_summary` is
            // already PPRL-scrubbed at summarization time.)
            if let Some(matches) = out.get_mut("matches").and_then(|m| m.as_array_mut()) {
                for row in matches {
                    let name = row
                        .get("customer_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    if let Some(name) = name {
                        if !name.trim().is_empty() {
                            row["customer_name"] = json!(tokenize(state, &name, "Name"));
                        }
                    }
                }
            }
            json_result(&out)
        }
        Err(e) => error_result(format!("search_database failed: {e}")),
    }
}

async fn list_ticket_attachments(state: &Arc<AppState>, args: &Value) -> Value {
    let ticket_id = match args.get("ticket_id").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return error_result("missing required argument `ticket_id`"),
    };
    let tool = ListTicketAttachmentsTool { db: state.db.clone() };
    match tool.call(ListTicketAttachmentsArgs { ticket_id }).await {
        Ok(out) => json_result(&out),
        Err(e) => error_result(format!("list_ticket_attachments failed: {e}")),
    }
}

async fn analyze_qc_report(state: &Arc<AppState>, args: &Value) -> Value {
    let file_ids: Vec<String> = match args.get("file_ids").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a
            .iter()
            .filter_map(|v| v.as_str())
            .map(String::from)
            .collect(),
        _ => return error_result("missing required argument `file_ids` (non-empty array)"),
    };
    let tool = AnalyzeQcReportTool {
        db: state.db.clone(),
        filestore: Arc::new(eck_core::utils::filestore::FileStore::new(".")),
    };
    match tool.call(AnalyzeQcReportArgs { file_ids }).await {
        Ok(out) => json_result(&out),
        Err(e) => error_result(format!("analyze_qc_report failed: {e}")),
    }
}

// ─── ocr_attachment / analyze_attachment_visual (ai::attachments) ───────────
//
// Same single implementation the orchestrator's rig tools call, so the two
// surfaces can't drift. `ocr_attachment` is local + free (normal tier);
// `analyze_attachment_visual` sends a raw scan off-node and is Master-gated
// (see tools_list) — a scan has no PII token layer to mask.

async fn ocr_attachment(state: &Arc<AppState>, args: &Value) -> Value {
    let file_ids: Vec<String> = match args.get("file_ids").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a.iter().filter_map(|v| v.as_str()).map(String::from).collect(),
        _ => return error_result("missing required argument `file_ids` (non-empty array)"),
    };
    let mut files = Vec::with_capacity(file_ids.len());
    for id in file_ids {
        files.push(crate::ai::attachments::ocr_one(&state.db, &id).await);
    }
    json_result(&json!({ "files": files }))
}

async fn analyze_attachment_visual(state: &Arc<AppState>, args: &Value, tier: McpTier) -> Value {
    if tier != McpTier::Master {
        return error_result("analyze_attachment_visual requires the master operator token");
    }
    let file_id = match args.get("file_id").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return error_result("missing required argument `file_id`"),
    };
    let question = match args.get("question").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return error_result("missing required argument `question`"),
    };
    let page = args.get("page").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let region = args
        .get("region")
        .and_then(|r| serde_json::from_value::<crate::ai::attachments::Region>(r.clone()).ok());
    let out =
        crate::ai::attachments::analyze_visual(&state.db, &file_id, &question, region, page).await;
    json_result(&out)
}

// ─── export_attachment (Master only) ───────────────────────────────────────
//
// Copy a CAS blob to the server's disk. Add-only (never overwrites) and the
// sha256 is verified against the file_resource row before any write, so a
// corrupt or mesh-poisoned blob can't be exported. Resolution goes through
// `resolve_file_full`, so replaced_by chains and mesh hydration come for free —
// which is why this reaches files ocr_attachment can't (encrypted PDFs, AVIF).

async fn export_attachment(state: &Arc<AppState>, args: &Value, tier: McpTier) -> Value {
    if tier != McpTier::Master {
        return error_result("export_attachment requires the master operator token");
    }
    let file_id = match args.get("file_id").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return error_result("missing required argument `file_id`"),
    };
    let dest_str = match args.get("dest_path").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return error_result("missing required argument `dest_path`"),
    };
    let dest = std::path::Path::new(&dest_str);
    if !dest.is_absolute() {
        return error_result("`dest_path` must be an absolute path including the filename");
    }
    // Fast fail before doing the work; create_new below is the real guarantee.
    if dest.exists() {
        return error_result("destination exists — export_attachment never overwrites");
    }

    let resolved = match crate::ai::attachments::resolve_file_full(&state.db, &file_id).await {
        Ok(r) => r,
        Err(_) => return error_result("file not found"),
    };
    let bytes = match tokio::fs::read(&resolved.path).await {
        Ok(b) => b,
        Err(e) => return error_result(format!("cannot read source bytes: {e}")),
    };

    let sha = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    };
    if !resolved.hash.is_empty() && sha != resolved.hash {
        return error_result(format!(
            "integrity check failed: recorded hash {} != bytes {} — nothing written",
            resolved.hash, sha
        ));
    }

    if let Some(parent) = dest.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return error_result(format!("cannot create destination directory: {e}"));
        }
    }
    // Add-only: create_new fails if the path appeared since the check above.
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .await;
    let mut file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return error_result("destination exists — export_attachment never overwrites")
        }
        Err(e) => return error_result(format!("cannot create destination file: {e}")),
    };
    use tokio::io::AsyncWriteExt;
    if let Err(e) = file.write_all(&bytes).await {
        return error_result(format!("cannot write destination file: {e}"));
    }

    json_result(&json!({
        "status": "ok",
        "dest_path": dest.display().to_string(),
        "size_bytes": bytes.len(),
        "sha256": sha,
        "original_name": resolved.name,
        "mime_type": resolved.mime,
        "cas_uuid": file_id,
    }))
}

// ─── surrealql_read (Master only) ──────────────────────────────────────────

async fn surrealql_read(state: &Arc<AppState>, args: &Value, tier: McpTier) -> Value {
    if tier != McpTier::Master {
        return error_result("surrealql_read requires the master operator token");
    }
    let q = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return error_result("missing required argument `query`"),
    };
    // Same guards as /X/ops/surrealql_read: read-only verb + Zone-1 denylist.
    let first_kw = q
        .split_whitespace()
        .next()
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    const READ_ONLY: &[&str] = &["SELECT", "INFO", "LIVE", "RETURN"];
    if !READ_ONLY.contains(&first_kw.as_str()) {
        return error_result(format!(
            "verb '{first_kw}' not in read-only set [SELECT, INFO, LIVE, RETURN]"
        ));
    }
    if let Some(forbidden) = crate::handlers::ops::query_references_zone1(&q) {
        return error_result(format!("query references Zone 1 table '{forbidden}'"));
    }

    let started = std::time::Instant::now();
    match state.db.query(q).await {
        Ok(mut r) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            tracing::info!(
                target: "ops_audit",
                verb = "mcp_surrealql_read", first_kw = %first_kw, duration_ms,
                "mcp tool executed"
            );
            let value: Result<Vec<Value>, _> = r.take(0);
            match value {
                Ok(v) => json_result(&json!({ "result": v, "duration_ms": duration_ms })),
                Err(e) => json_result(&json!({
                    "result": null,
                    "warning": format!("decode failed: {e}"),
                    "duration_ms": duration_ms,
                })),
            }
        }
        Err(e) => error_result(format!("query failed: {e}")),
    }
}

// ─── reveal_file_local (Master only) ───────────────────────────────────────
//
// Server-local twin of the shim's `reveal_file`: the report file lives on the
// WMS machine's own disk (not the remote caller's), so the reveal happens here.
// Named `reveal_file_local` so it never collides with the shim's `reveal_file`
// in a merged `tools/list`. The clear values go to the file on disk ONLY — the
// tool result carries counts + the output path, never a revealed value, and
// error messages never echo file contents.

/// Resolve the output path, CONFINED to the same parent directory as `path`.
/// `None`/empty `output` → overwrite `path`. A relative `output`, or one whose
/// parent differs from `path`'s, is rejected. Pure (no filesystem access).
fn confine_output(path: &std::path::Path, output: Option<&str>) -> Result<std::path::PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "`path` has no parent directory".to_string())?;
    match output.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Ok(path.to_path_buf()),
        Some(o) => {
            let op = std::path::Path::new(o);
            if !op.is_absolute() {
                return Err("`output` must be an absolute path in the same directory as `path`".to_string());
            }
            let op_parent = op
                .parent()
                .ok_or_else(|| "`output` has no parent directory".to_string())?;
            if op_parent != parent {
                return Err("`output` must be in the same directory as `path`".to_string());
            }
            Ok(op.to_path_buf())
        }
    }
}

/// Replace every resolved token in `text` with its clear value; tokens absent
/// from `revealed` stay pseudonymized. Pure.
fn apply_revealed(text: &str, revealed: &serde_json::Map<String, Value>) -> String {
    let mut out = text.to_string();
    for (token, val) in revealed {
        if let Some(v) = val.as_str() {
            out = out.replace(token, v);
        }
    }
    out
}

async fn reveal_file_local(state: &Arc<AppState>, args: &Value, tier: McpTier) -> Value {
    if tier != McpTier::Master {
        return error_result("reveal_file_local requires the master operator token");
    }
    let path_str = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => return error_result("missing required argument `path`"),
    };
    let path = std::path::Path::new(&path_str);
    if !path.is_absolute() {
        return error_result("`path` must be an absolute server-local path");
    }
    let out_path = match confine_output(path, args.get("output").and_then(|v| v.as_str())) {
        Ok(p) => p,
        Err(e) => return error_result(e),
    };

    // File must exist, be a regular file, and be within the size ceiling.
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return error_result(format!("cannot access `path`: {e}")),
    };
    if !meta.is_file() {
        return error_result("`path` is not a regular file");
    }
    const MAX_BYTES: u64 = 10 * 1024 * 1024;
    if meta.len() > MAX_BYTES {
        return error_result(format!(
            "file too large ({} bytes) — reveal is limited to 10 MiB",
            meta.len()
        ));
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return error_result(format!("cannot read `path`: {e}")),
    };

    // Richer extractor than the shim's local regex (includes Company_ etc.).
    let tokens = eck_core::utils::anonymizer::extract_pii_tokens(&text);
    if tokens.is_empty() {
        // Nothing to reveal — still honor an explicit output-copy request.
        if out_path.as_path() != path {
            if let Err(e) = std::fs::write(&out_path, &text) {
                return error_result(format!("cannot write output file: {e}"));
            }
        }
        return json_result(&json!({
            "revealed_count": 0,
            "unresolved_count": 0,
            "output_path": out_path.display().to_string(),
        }));
    }

    // Master-only resolve (tier already proven above) + the DB fallback for
    // tokens baked into stored summaries — same wiring as the reveal_tokens
    // method in mod.rs.
    let params = json!({ "tokens": tokens });
    let result = match super::reveal::reveal_tokens_method(&state.pii_reveal, tier, &params) {
        Ok(v) => super::reveal::augment_from_db(state, v).await,
        // Only the tier gate errors here, and tier is already Master — stay
        // defensive and NEVER echo internal reveal detail.
        Err(_) => return error_result("reveal failed"),
    };

    let revealed = result
        .get("revealed")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let unresolved_count = result
        .get("unresolved")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| tokens.len().saturating_sub(revealed.len()));

    let rewritten = apply_revealed(&text, &revealed);
    if let Err(e) = std::fs::write(&out_path, rewritten) {
        return error_result(format!("cannot write output file: {e}"));
    }

    // Counts + path ONLY — the clear values went to disk, never here.
    json_result(&json!({
        "revealed_count": revealed.len(),
        "unresolved_count": unresolved_count,
        "output_path": out_path.display().to_string(),
    }))
}

// ─── ask_brain (the Gemini Central Brain, one-shot) ────────────────────────

/// One brain run at a time per node (config `ECK_BRAIN_CONCURRENCY`): the runs
/// are paid multi-turn agent loops — queueing them silently would hide cost.
fn brain_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| {
        let n = std::env::var("ECK_BRAIN_CONCURRENCY")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1usize)
            .max(1);
        tokio::sync::Semaphore::new(n)
    })
}

async fn ask_brain(state: &Arc<AppState>, args: &Value, tier: McpTier) -> Value {
    let question = match args.get("question").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return error_result("missing required argument `question`"),
    };
    let context = args
        .get("context")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // The budget circuit breaker covers brain runs like all other AI work.
    if crate::ai::telemetry::current_budget_level() == crate::ai::telemetry::BudgetLevel::Halt {
        return error_result("AI budget exhausted (halt level) — brain runs are suspended");
    }
    let Ok(_slot) = brain_slots().try_acquire() else {
        return error_result("the brain is busy with another run — retry shortly");
    };

    let external = !tier.reveal_pii();
    match crate::ai::orchestrator::answer_oneshot(state, &question, &context, tier).await {
        Ok((answer, model)) => {
            // Belt and braces for external callers: the prompt already forbids
            // PII in the answer; the regex scrub catches emails/phones/streets
            // that slipped through anyway.
            let answer = if external {
                eck_core::utils::anonymizer::scrub_pii_regex(&answer).0
            } else {
                answer
            };
            json_result(&json!({
                "answer": answer,
                "model": model,
                "engine": "gemini",
                "billing": "normal brain work (managed/studio AI path)",
            }))
        }
        Err(e) => error_result(format!("brain run failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_masked_on_word_boundaries() {
        let needles = vec![
            ("phil weber".to_string(), "Name_AAA".to_string()),
            ("weber".to_string(), "Name_AAA".to_string()),
            ("phil".to_string(), "Name_AAA".to_string()),
        ];
        // Bare first name in a greeting and in a signature — both masked.
        assert_eq!(
            mask_known_names("Hallo Phil,\nviele Grüße\n-Phil", &needles),
            "Hallo Name_AAA,\nviele Grüße\n-Name_AAA"
        );
        // ...but never inside a longer word.
        assert_eq!(
            mask_known_names("Er studiert Philosophie in Weberstadt.", &needles),
            "Er studiert Philosophie in Weberstadt."
        );
        // Full name wins (needles arrive longest-first from thread_name_needles).
        assert_eq!(mask_known_names("von Phil Weber", &needles), "von Name_AAA");
    }

    #[test]
    fn known_name_inside_its_own_token_does_not_loop() {
        // Pathological: the needle reappears inside the replacement. A rescanning
        // implementation would grow the string forever; the single pass must not.
        let needles = vec![("name".to_string(), "Name_BBB".to_string())];
        assert_eq!(mask_known_names("Mein Name ist X", &needles), "Mein Name_BBB ist X");
    }

    #[test]
    fn masking() {
        assert_eq!(mask_email("john.doe@acme.de"), "j***@acme.de");
        assert_eq!(mask_email("noatsign"), "***");
        assert_eq!(mask_name("Anna Müller"), "A*** M***");
        assert_eq!(mask_name(""), "***");
        assert_eq!(mask_phone("+49 170 1234567"), "***67");
        assert_eq!(mask_phone("x"), "***");
    }

    #[test]
    fn identity_grouping() {
        // Email wins over name; case/whitespace-insensitive.
        let a = json!({"email": "A@X.de", "customer": "Anna"});
        let b = json!({"email": "a@x.de ", "customer": "Anna M."});
        assert_eq!(identity_key(&a), identity_key(&b));
        // No email → name; no name → company; all empty → shared anonymous key.
        let c = json!({"customer": "Bob"});
        let d = json!({"company": "Acme GmbH"});
        assert_eq!(identity_key(&c), "n:bob");
        assert_eq!(identity_key(&d), "c:acme gmbh");
        assert_ne!(identity_key(&a), identity_key(&c));
        assert_eq!(identity_key(&json!({})), identity_key(&json!({"email": ""})));
    }

    #[test]
    fn closed_detection() {
        assert!(is_closed("Closed"));
        assert!(is_closed("Auto closed"));
        assert!(is_closed("Closed - resolved"));
        assert!(is_closed("gelöst"));
        assert!(!is_closed("Open"));
        assert!(!is_closed("in_progress"));
    }

    // ── reveal_file_local: output-path confinement (pure) ──
    #[test]
    fn reveal_file_local_output_confinement() {
        // Use a real absolute dir so the check is platform-correct on Windows.
        let dir = std::env::temp_dir();
        let path = dir.join("report.md");

        // Default (no output) overwrites `path`.
        assert_eq!(confine_output(&path, None).unwrap(), path);
        // Empty output → treated as default.
        assert_eq!(confine_output(&path, Some("  ")).unwrap(), path);
        // Same-directory absolute output is accepted.
        let same = dir.join("report.revealed.md");
        assert_eq!(
            confine_output(&path, Some(same.to_str().unwrap())).unwrap(),
            same
        );
        // A different directory is rejected (confinement).
        let other = dir.join("sub").join("x.md");
        assert!(confine_output(&path, Some(other.to_str().unwrap())).is_err());
        // A relative output is rejected.
        assert!(confine_output(&path, Some("out.md")).is_err());
    }

    // ── reveal_file_local: token substitution round-trip (pure) ──
    #[test]
    fn reveal_file_local_apply_revealed_roundtrip() {
        let text = "Contact Name_00000000000000AB via Email_00000000000000CD or Phone_00000000000000EF.";
        let mut revealed = serde_json::Map::new();
        revealed.insert("Name_00000000000000AB".into(), json!("Anna Müller"));
        revealed.insert("Email_00000000000000CD".into(), json!("anna@x.de"));
        // Phone deliberately left unresolved — it must stay pseudonymized.
        let out = apply_revealed(text, &revealed);
        assert!(out.contains("Anna Müller"));
        assert!(out.contains("anna@x.de"));
        assert!(!out.contains("Name_00000000000000AB"));
        assert!(!out.contains("Email_00000000000000CD"));
        assert!(out.contains("Phone_00000000000000EF"));
    }

    // ── reveal_file_local: tools/list tier gate (Master only) ──
    #[test]
    fn reveal_file_local_master_only_in_tools_list() {
        let has = |tier| {
            tools_list(tier)
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t.get("name").and_then(|n| n.as_str()) == Some("reveal_file_local"))
        };
        assert!(has(McpTier::Master), "master must see reveal_file_local");
        assert!(!has(McpTier::Agent), "agent must NOT see reveal_file_local");
    }
}
