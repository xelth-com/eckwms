use std::sync::Arc;

use regex::Regex;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};

use eck_core::db::SurrealDb;
use eck_core::utils::anonymizer::{obfuscate_pii, parse_pii_token};
use eck_core::utils::filestore::FileStore;

use crate::ai::embeddings::embed_query;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ToolError(pub String);

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        ToolError(e.to_string())
    }
}

impl From<surrealdb::Error> for ToolError {
    fn from(e: surrealdb::Error) -> Self {
        ToolError(e.to_string())
    }
}

// ─── Tool: Ask Human ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AskHumanTool {
    pub db: SurrealDb,
    /// Full record ID of the owning task, e.g. "ai_task:abc123".
    pub task_rid: String,
    /// Broadcast channel to the frontend WebSocket fan-out. On pause we
    /// emit an `AI_TASK_PAUSED` envelope so the Operator Inbox reloads
    /// without waiting for the next Refresh click.
    pub ws_tx: tokio::sync::broadcast::Sender<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct AskHumanArgs {
    /// The question to ask the human operator. Be specific — the operator
    /// only sees this single message, not the surrounding conversation.
    pub question: String,
}

impl Tool for AskHumanTool {
    const NAME: &'static str = "ask_human";
    type Error = ToolError;
    type Args = AskHumanArgs;
    type Output = Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Pause execution and ask the human operator a question. \
                After calling this tool, produce a brief final message acknowledging \
                the pause — do NOT call any more tools. Execution resumes when the \
                operator replies."
                .to_string(),
            parameters: schemars::schema_for!(AskHumanArgs).into(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Store the question on the task and flip to `awaiting_human`.
        // Rule 19: type::record($rid) with full "table:id" string.
        self.db
            .query(
                "UPDATE type::record($rid) \
                 SET state = 'awaiting_human', \
                     awaiting_input_schema = { question: $q }, \
                     updated_at = time::now()",
            )
            .bind(("rid", self.task_rid.clone()))
            .bind(("q", args.question.clone()))
            .await?
            .check()?;

        debug!(
            "[AskHumanTool] Task {} paused awaiting human reply",
            self.task_rid
        );

        // Notify the UI. `send` returns Err only if there are zero live
        // subscribers (no browser tabs open) — not a real failure.
        let ws_msg = json!({
            "type": "AI_TASK_PAUSED",
            "task_id": self.task_rid,
            "question": args.question,
        });
        let _ = self
            .ws_tx
            .send(serde_json::to_string(&ws_msg).unwrap_or_default());

        Ok(json!({
            "status": "paused",
            "note": "Task has been paused. The operator will reply asynchronously. \
                    End your turn now — do not call any more tools."
        }))
    }
}

// ─── Tool: Analyze QC Report ─────────────────────────────────────────────────

#[derive(Clone)]
pub struct AnalyzeQcReportTool {
    pub db: SurrealDb,
    pub filestore: Arc<FileStore>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct AnalyzeQcReportArgs {
    /// CAS UUIDs of QC report files to analyze. Each file is a plain-text QC
    /// report produced by a device (e.g. from a `qcreport_*` dump).
    pub file_ids: Vec<String>,
}

#[derive(Serialize)]
struct QcReport {
    file_id: String,
    status: String,
    digital_fw: Option<String>,
    analog_fw: Option<String>,
    serial: Option<String>,
    error: Option<String>,
}

impl Tool for AnalyzeQcReportTool {
    const NAME: &'static str = "analyze_qc_report";
    type Error = ToolError;
    type Args = AnalyzeQcReportArgs;
    type Output = Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Analyze one or more QC report files (plain-text device \
                QC dumps) by their CAS UUIDs. Extracts digital firmware, analog \
                firmware, and serial number from each file. Returns a list with one \
                entry per file — status will be 'ok', 'not_found' (file missing), or \
                'no_match' (file exists but firmware pattern absent)."
                .to_string(),
            parameters: schemars::schema_for!(AnalyzeQcReportArgs).into(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Middle segment in real QC reports is "Default Version" (contains a
        // space), so we use `[^/]+` instead of `\S+?` for that position.
        let re = Regex::new(r"Program Version\s*:\s*(\S+?)/(\S+?)/[^/]+/(\S+?)\(")
            .map_err(|e| ToolError(format!("regex compile failed: {e}")))?;

        let mut reports: Vec<QcReport> = Vec::with_capacity(args.file_ids.len());

        for file_id in args.file_ids {
            // Resolve CAS UUID → storage_path via file_resource (Rule 1).
            let rows: Vec<Value> = self
                .db
                .query(
                    "SELECT storage_path FROM file_resource \
                     WHERE cas_uuid = $id AND storage_path IS NOT NONE LIMIT 1",
                )
                .bind(("id", file_id.clone()))
                .await?
                .take(0)?;

            let storage_path = match rows.into_iter().next().and_then(|r| {
                r.get("storage_path")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            }) {
                Some(p) => p,
                None => {
                    reports.push(QcReport {
                        file_id: file_id.clone(),
                        status: "not_found".into(),
                        digital_fw: None,
                        analog_fw: None,
                        serial: None,
                        error: Some("file_resource or storage_path missing".into()),
                    });
                    continue;
                }
            };

            let bytes = match self.filestore.read(&storage_path).await {
                Ok(b) => b,
                Err(e) => {
                    warn!("[AnalyzeQcReportTool] Read {} failed: {}", storage_path, e);
                    reports.push(QcReport {
                        file_id: file_id.clone(),
                        status: "not_found".into(),
                        digital_fw: None,
                        analog_fw: None,
                        serial: None,
                        error: Some(e),
                    });
                    continue;
                }
            };

            let text = String::from_utf8_lossy(&bytes);
            match re.captures(&text) {
                Some(caps) => {
                    reports.push(QcReport {
                        file_id: file_id.clone(),
                        status: "ok".into(),
                        digital_fw: caps.get(1).map(|m| m.as_str().to_string()),
                        analog_fw: caps.get(2).map(|m| m.as_str().to_string()),
                        serial: caps.get(3).map(|m| m.as_str().to_string()),
                        error: None,
                    });
                }
                None => {
                    reports.push(QcReport {
                        file_id: file_id.clone(),
                        status: "no_match".into(),
                        digital_fw: None,
                        analog_fw: None,
                        serial: None,
                        error: Some(
                            "'Program Version : ...' pattern not present in file".into(),
                        ),
                    });
                }
            }
        }

        Ok(json!({ "reports": reports }))
    }
}

// ─── Tool: List Ticket Attachments ───────────────────────────────────────────

#[derive(Clone)]
pub struct ListTicketAttachmentsTool {
    pub db: SurrealDb,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct ListTicketAttachmentsArgs {
    /// Zoho ticket ID (the internal 17-digit document id, no "document:"
    /// prefix) OR the short public ticket number printed on correspondence —
    /// a plain number under 12 digits is resolved to the document id first.
    pub ticket_id: String,
}

#[derive(Serialize)]
struct AttachmentInfo {
    cas_uuid: String,
    name: String,
    mime_type: String,
    size_bytes: i64,
    /// How many has_attachment edges point at this file_resource. > 1 across
    /// many tickets usually means a shared blank template form, not a customer
    /// document.
    linked_tickets: i64,
    /// Up to 10 public ticket numbers of the tickets that reference this same
    /// blob. A raw `linked_tickets` count alone misleads (a customer's signed doc
    /// filed on several of THEIR tickets looks like a template); the numbers let
    /// the agent tell "one customer, a few tickets" from "a blank form on 180".
    linked_ticket_numbers: Vec<String>,
    /// Cheap-model document class (layer-2 classification), present only when the
    /// blob has been classified: the card's `doc_type`
    /// ("reparaturauftrag"|"kostenvoranschlag"|"invoice"|"shipping_label"|
    /// "datenverarbeitung"|"qc_report"|"photo_screen"|"other", or
    /// "unclassifiable_low_quality" when the extract was too garbled to classify).
    #[serde(skip_serializing_if = "Option::is_none")]
    doc_type: Option<String>,
    /// From the same card: whether the form looks like a blank/unfilled template.
    #[serde(skip_serializing_if = "Option::is_none")]
    is_blank: Option<bool>,
}

impl Tool for ListTicketAttachmentsTool {
    const NAME: &'static str = "list_ticket_attachments";
    type Error = ToolError;
    type Args = ListTicketAttachmentsArgs;
    type Output = Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List files attached to a support ticket, by its Zoho \
                document id OR the short public ticket number. Returns CAS \
                UUIDs that can be fed directly into `analyze_qc_report`, each \
                with `linked_tickets` (how many tickets share that file) and \
                `linked_ticket_numbers` (up to 10 of those tickets' public \
                numbers). The count alone misleads: 3 known blank templates link \
                180+ tickets, but a small shared count usually means one \
                customer's document filed on several of their own tickets — check \
                the numbers before treating a shared file as a template. When a \
                file has been classified it also carries `doc_type` \
                (reparaturauftrag/kostenvoranschlag/invoice/shipping_label/ \
                datenverarbeitung/qc_report/photo_screen/other) and `is_blank` \
                (an unfilled template) — use them to pick the right file without \
                OCRing every one. Call this BEFORE asking the human — most QC \
                reports are already downloaded from Zoho and just need to be \
                located."
                .to_string(),
            parameters: schemars::schema_for!(ListTicketAttachmentsArgs).into(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let ticket_id = self.resolve_ticket_id(&args.ticket_id).await?;

        // Walk the has_attachment graph edge. The ticket is stored as
        // document:$ticket_id — match on backtick-quoted record ID to
        // survive numeric Zoho IDs (SurrealDB would otherwise parse them
        // as integers and fail the record equality check).
        let rows: Vec<Value> = self
            .db
            .query(
                "SELECT \
                    out.cas_uuid AS cas_uuid, \
                    out.original_name AS name, \
                    out.mime_type AS mime_type, \
                    out.size_bytes AS size_bytes, \
                    out.doc_class AS doc_class \
                 FROM has_attachment \
                 WHERE in = type::record($trid) \
                 AND out.cas_uuid IS NOT NONE",
            )
            .bind(("trid", format!("document:`{}`", ticket_id)))
            .await?
            .take(0)?;

        let mut attachments: Vec<AttachmentInfo> = rows
            .into_iter()
            .filter_map(|r| {
                let cas = r.get("cas_uuid").and_then(|v| v.as_str())?.to_string();
                // The layer-2 card, when the blob has been classified.
                let (doc_type, is_blank) = match r.get("doc_class") {
                    Some(dc) if dc.is_object() => (
                        dc.get("doc_type").and_then(|v| v.as_str()).map(String::from),
                        dc.get("is_blank").and_then(|v| v.as_bool()),
                    ),
                    _ => (None, None),
                };
                Some(AttachmentInfo {
                    cas_uuid: cas,
                    name: r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    mime_type: r
                        .get("mime_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("application/octet-stream")
                        .to_string(),
                    size_bytes: r.get("size_bytes").and_then(|v| v.as_i64()).unwrap_or(0),
                    linked_tickets: 0,
                    linked_ticket_numbers: Vec::new(),
                    doc_type,
                    is_blank,
                })
            })
            .collect();

        // Deduped CAS content is linked from every ticket that shipped the same
        // file, so an attachment can fan out to many tickets — count the edges
        // per file, and pull up to 10 of the tickets' public numbers so the agent
        // can tell one customer's cross-filed document from a shared blank form
        // (≤10 files, two small queries each).
        for att in &mut attachments {
            let n: Vec<Value> = self
                .db
                .query(
                    "SELECT VALUE count() FROM has_attachment \
                     WHERE out.cas_uuid = $cu GROUP ALL",
                )
                .bind(("cu", att.cas_uuid.clone()))
                .await?
                .take(0)?;
            att.linked_tickets = n.first().and_then(|v| v.as_i64()).unwrap_or(0);

            let nums: Vec<Value> = self
                .db
                .query(
                    "SELECT VALUE in.meta.ticket_number FROM has_attachment \
                     WHERE out.cas_uuid = $cu AND in.meta.ticket_number IS NOT NONE LIMIT 10",
                )
                .bind(("cu", att.cas_uuid.clone()))
                .await?
                .take(0)?;
            att.linked_ticket_numbers = nums
                .into_iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(String::from)
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                })
                .collect();
        }

        debug!(
            "[ListTicketAttachmentsTool] ticket={} found {} attachments",
            ticket_id,
            attachments.len()
        );
        Ok(json!({ "ticket_id": ticket_id, "attachments": attachments }))
    }
}

impl ListTicketAttachmentsTool {
    /// A caller may pass the internal 17-digit Zoho document id or the short
    /// public ticket number printed on correspondence. A plain number under 12
    /// digits is treated as a public ticket number and resolved to its document
    /// id via `meta.ticket_number`; anything else is used verbatim.
    async fn resolve_ticket_id(&self, input: &str) -> Result<String, ToolError> {
        let t = input.trim();
        if !t.is_empty() && t.len() < 12 && t.bytes().all(|b| b.is_ascii_digit()) {
            let ids: Vec<Value> = self
                .db
                .query(
                    "SELECT VALUE type::string(record::id(id)) FROM document \
                     WHERE type = 'support_ticket' AND meta.ticket_number = $tn LIMIT 1",
                )
                .bind(("tn", t.to_string()))
                .await?
                .take(0)?;
            if let Some(id) = ids.into_iter().next().and_then(|v| v.as_str().map(String::from)) {
                return Ok(id);
            }
        }
        Ok(t.to_string())
    }
}

// ─── Tool: OCR Attachment ────────────────────────────────────────────────────

#[derive(Clone)]
pub struct OcrAttachmentTool {
    pub db: SurrealDb,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct OcrAttachmentArgs {
    /// CAS UUIDs of attachment files to OCR (from `list_ticket_attachments`).
    pub file_ids: Vec<String>,
}

impl Tool for OcrAttachmentTool {
    const NAME: &'static str = "ocr_attachment";
    type Error = ToolError;
    type Args = OcrAttachmentArgs;
    type Output = Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Extract text from ticket attachments (PDF scans/photos) by their \
                CAS UUIDs — purely local, no AI. For each file it tries the PDF text layer \
                first, then decodes the page images (office-scanner CCITT G3/G4 fax, DCT JPEG, \
                or raw bitmaps) and OCRs them (external tesseract if configured, else the \
                built-in engine). Returns per file: status \
                ('ok'|'not_found'|'ocr_unavailable'|'encrypted'|'unsupported_codec'). \
                'encrypted' means the file needs a real user password (owner-password PDFs are \
                decrypted automatically and read normally); 'unsupported_codec' means the page \
                images use a codec this build cannot decode (JBIG2Decode/JPXDecode) — the \
                blocking codec is named in the `codec` field and the intact bytes still \
                export_attachment. Also returns source ('text_layer'|'ocrs'|'external_cmd'), the \
                text (capped 6000 chars), a quality block (chars, lines, dictionary_ratio, \
                garbage_ratio) so you can judge whether the text is trustworthy, recognized line \
                boxes (normalized 0..1), and page count. The extract is cached on the file \
                (with a normalized-text fingerprint that clusters re-uploaded copies of the same \
                form) and mesh-replicated, so repeat calls are free and a node without the blob \
                still returns the stored text. Call this BEFORE analyze_attachment_visual — only \
                escalate to vision when this text is missing or too garbled to answer the \
                question."
                .to_string(),
            parameters: schemars::schema_for!(OcrAttachmentArgs).into(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut files = Vec::with_capacity(args.file_ids.len());
        for id in args.file_ids {
            files.push(crate::ai::attachments::ocr_one(&self.db, &id).await);
        }
        debug!("[OcrAttachmentTool] processed {} file(s)", files.len());
        Ok(json!({ "files": files }))
    }
}

// ─── Tool: Analyze Attachment Visual (policy-gated Gemini vision) ─────────────

#[derive(Clone)]
pub struct AnalyzeAttachmentVisualTool {
    pub db: SurrealDb,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct AnalyzeAttachmentVisualArgs {
    /// CAS UUID of the attachment to look at.
    pub file_id: String,
    /// A specific, self-contained question about what the image shows.
    pub question: String,
    /// Optional page index (default 0) for multi-page scan PDFs.
    #[serde(default)]
    pub page: Option<u32>,
    /// Optional relative (0..1) region to crop to before sending — use the
    /// SMALLEST region that answers the question.
    #[serde(default)]
    pub region: Option<crate::ai::attachments::Region>,
}

impl Tool for AnalyzeAttachmentVisualTool {
    const NAME: &'static str = "analyze_attachment_visual";
    type Error = ToolError;
    type Args = AnalyzeAttachmentVisualArgs;
    type Output = Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Ask Gemini vision a question about ONE attachment page image. Call \
                ocr_attachment FIRST — only use this when the OCR text is insufficient to \
                answer. A scanned image bypasses PII masking, so prefer the SMALLEST region \
                that answers the question (e.g. only the signature zone) and avoid regions \
                containing names/addresses unless the question requires them. Args: file_id, \
                question, optional page (default 0), optional region {x,y,w,h} relative 0..1. \
                Returns {status:'ok', answer, model, tokens_in, tokens_out}; may instead \
                return 'denied_by_policy' (node forbids sending scans off-node — then use \
                ask_human), 'not_found', or 'no_image'. Every response also carries \
                prior_analyses: up to the 5 most recent earlier Q&A for this file (question, \
                answer, created_at) — check them FIRST and reuse an already-paid answer instead \
                of asking again. This call is METERED."
                .to_string(),
            parameters: schemars::schema_for!(AnalyzeAttachmentVisualArgs).into(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(crate::ai::attachments::analyze_visual(
            &self.db,
            &args.file_id,
            &args.question,
            args.region,
            args.page.unwrap_or(0),
        )
        .await)
    }
}

// ─── Tool: Search Database ──────────────────────────────────────────────────
//
// WHY: CSV enrichment (and other agentic workflows) need to match free-text
// customer input against our `order` (RMA/repairs) and `document`
// (Zoho support tickets) tables. The orchestrator already has ticket-level
// tools (`list_ticket_attachments`, `analyze_qc_report`), but nothing that
// lets an agent say "given this arbitrary fragment, is there a matching
// record anywhere?" — which is exactly what unstructured CSV rows require.
//
// The existing hybrid search path on `/api/rma/search` (see rma.rs) is a
// tuned production retriever with per-term RRF across three BM25 fields and
// the HNSW vector index. For in-agent use we want something simpler and
// tolerant of missing capabilities (e.g. Gemini key unavailable):
//   * OR across BM25 fields instead of per-term RRF — noisier rankings are
//     fine because the LLM does the final disambiguation.
//   * HNSW is merged via `search::rrf` only when an embedding is available.
//   * Results are trimmed to a small projection (no embedding vectors, no
//     raw payload) so the tool output stays inside a few hundred tokens.

#[derive(Clone)]
pub struct SearchDatabaseTool {
    pub db: SurrealDb,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct SearchDatabaseArgs {
    /// Free-text query — typically a noun phrase, order number fragment,
    /// customer name, or issue description pulled out of the CSV row.
    pub query: String,
    /// Which table to search. Supported: `order` (RMA / repairs) and
    /// `document` (Zoho support tickets). Any other value returns an error.
    pub table: String,
}

impl Tool for SearchDatabaseTool {
    const NAME: &'static str = "search_database";
    type Error = ToolError;
    type Args = SearchDatabaseArgs;
    type Output = Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Hybrid BM25 + vector search over a single table. \
                Use `table`='order' to look up repair/RMA records by order \
                number, customer name, or issue description. Use \
                `table`='document' to look up Zoho support tickets by \
                subject/summary content. A pseudonym token (Name_<hex>, \
                Email_<hex>, …) as the query performs an EXACT match — every \
                record referencing that person — instead of fuzzy retrieval. \
                Returns up to 3 fuzzy (10 exact) matches with a compact \
                projection — enough to disambiguate a CSV row, not enough \
                to exfiltrate the whole record."
                .to_string(),
            parameters: schemars::schema_for!(SearchDatabaseArgs).into(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let q = args.query.trim();
        if q.is_empty() {
            return Ok(json!({ "table": args.table, "matches": [] }));
        }
        let table = args.table.as_str();
        if !matches!(table, "order" | "document") {
            return Err(ToolError(format!(
                "unsupported table '{}': must be 'order' or 'document'",
                args.table
            )));
        }

        // Pseudonym-token queries (`Name_<hex>` etc., as the /mcp surface
        // returns and the masked summaries embed) can NEVER match fuzzy
        // retrieval: the analyzer shreds the token into "name" + hex shards
        // (matching every German signature block), and embedding a hash
        // string is semantic noise. Route them to exact fingerprint lookup.
        if let Some(label) = parse_pii_token(q) {
            return self.search_by_token(table, q, label).await;
        }

        // embed_query self-resolves auth (studio key or managed Vertex bearer);
        // any failure (incl. unconfigured) degrades to BM25-only search.
        let q_vector = match embed_query(q).await {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "[SearchDatabaseTool] embed_query failed ({}) — BM25 only",
                    e
                );
                Vec::new()
            }
        };

        // Tokenize: BM25 `@@` operator cannot bind variables, so we hand-
        // escape each term. Terms of 2 chars or less rarely contribute and
        // blow up the OR query size.
        let terms: Vec<String> = q
            .split_whitespace()
            .filter(|t| t.len() > 2)
            .map(|t| t.replace('\'', "''").replace('\\', "\\\\"))
            .collect();
        let safe_q = q.replace('\'', "''").replace('\\', "\\\\");

        // `document` searches the MASKED fields (ai_summary + distilled
        // subject) — support_ticket rows never had `payload.content` (raw
        // lives in document_raw), so the old index made document BM25 dead
        // weight AND, had payload existed, would have let any caller probe
        // raw PII text that the output layer then carefully tokenizes.
        // Search must only see what the caller is allowed to read back.
        let bm25_fields: &[&str] = match table {
            "order" => &["issue_description", "customer_name", "order_number"],
            "document" => &["ai_summary", "meta.subject"],
            _ => &[],
        };

        let bm25_where = if terms.is_empty() {
            bm25_fields
                .iter()
                .map(|f| format!("{f} @@ '{safe_q}'"))
                .collect::<Vec<_>>()
                .join(" OR ")
        } else {
            terms
                .iter()
                .flat_map(|term| {
                    bm25_fields
                        .iter()
                        .map(move |f| format!("{f} @@ '{term}'"))
                })
                .collect::<Vec<_>>()
                .join(" OR ")
        };

        let projection = match table {
            "order" => {
                "record::id(id) AS id, order_number, customer_name, \
                 product_name, issue_description, status"
            }
            "document" => {
                "record::id(id) AS id, meta.ticket_number AS ticket_number, \
                 meta.subject AS subject, ai_summary, status"
            }
            _ => "record::id(id) AS id",
        };

        // The tool's contract for 'document' is Zoho support tickets, but the
        // table also holds repair folders and threads (some embedded) — without
        // a type filter they surface as meta-less rows and eat result slots.
        let type_filter = match table {
            "document" => " AND type = 'support_ticket'",
            _ => "",
        };

        let rows: Vec<Value> = if q_vector.is_empty() {
            // BM25-only fallback.
            let sql = format!(
                "SELECT {projection} FROM {table} WHERE ({bm25_where}){type_filter} LIMIT 3"
            );
            self.db.query(&sql).await?.take(0)?
        } else {
            // Hybrid: union BM25 OR-set with HNSW top-10, RRF merge.
            let sql = format!(
                "LET $vec = SELECT id, vector::distance::knn() AS distance \
                 FROM {table} WHERE embedding <|10,100|> $qe{type_filter};\
                 LET $bm = SELECT id FROM {table} WHERE ({bm25_where}){type_filter} LIMIT 10;\
                 LET $hybrid = search::rrf([$vec, $bm], 3, 60);\
                 SELECT {projection} FROM $hybrid.id;"
            );
            let mut response = self
                .db
                .query(&sql)
                .bind(("qe", q_vector))
                .await?;
            // 4 statements -> final SELECT index is 3.
            response.take(3)?
        };

        debug!(
            "[SearchDatabaseTool] table={} query={:?} -> {} rows",
            table,
            q,
            rows.len()
        );
        Ok(json!({ "table": args.table, "matches": rows }))
    }
}

impl SearchDatabaseTool {
    /// Exact lookup by pseudonym token. `document` rows carry a
    /// `pii_fingerprints` array (every token present in the masked summary +
    /// subject — see wms summarization + the main.rs self-heal), so a token
    /// is a straight CONTAINS match. `order` has no fingerprint field; its
    /// only PII column is `customer_name`, so re-derive each row's Name token
    /// (obfuscate_pii is deterministic) and compare in Rust — a rare path
    /// over a small table.
    async fn search_by_token(
        &self,
        table: &str,
        token: &str,
        label: &str,
    ) -> Result<Value, ToolError> {
        let rows: Vec<Value> = match table {
            "document" => {
                self.db
                    .query(
                        "SELECT record::id(id) AS id, meta.ticket_number AS ticket_number, \
                                meta.subject AS subject, ai_summary, status \
                         FROM document \
                         WHERE type = 'support_ticket' AND pii_fingerprints CONTAINS $t \
                         LIMIT 10",
                    )
                    .bind(("t", token.to_string()))
                    .await?
                    .take(0)?
            }
            "order" => {
                if label != "Name" {
                    return Ok(json!({
                        "table": "order",
                        "match_mode": "exact_token",
                        "matches": [],
                        "note": format!(
                            "`order` rows only carry a customer NAME — a {label}_ token \
                             cannot match. Query table='document' with it instead."
                        ),
                    }));
                }
                let candidates: Vec<Value> = self
                    .db
                    .query(
                        "SELECT record::id(id) AS id, customer_name FROM order \
                         WHERE customer_name != NONE AND customer_name != ''",
                    )
                    .await?
                    .take(0)?;
                let ids: Vec<String> = candidates
                    .iter()
                    .filter_map(|r| {
                        let name = r.get("customer_name")?.as_str()?.trim();
                        if !name.is_empty() && obfuscate_pii(name, "Name") == token {
                            r.get("id")?.as_str().map(String::from)
                        } else {
                            None
                        }
                    })
                    .take(10)
                    .collect();
                if ids.is_empty() {
                    Vec::new()
                } else {
                    self.db
                        .query(
                            "SELECT record::id(id) AS id, order_number, customer_name, \
                                    product_name, issue_description, status \
                             FROM order WHERE record::id(id) IN $ids",
                        )
                        .bind(("ids", ids))
                        .await?
                        .take(0)?
                }
            }
            _ => Vec::new(),
        };

        debug!(
            "[SearchDatabaseTool] exact token {} on {} -> {} rows",
            token,
            table,
            rows.len()
        );
        Ok(json!({
            "table": table,
            "match_mode": "exact_token",
            "matches": rows,
        }))
    }
}
