use std::sync::Arc;

use regex::Regex;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};

use eck_core::db::SurrealDb;
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
    /// report produced by an InBody device (e.g. from a `qcreport_*` dump).
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
            description: "Analyze one or more QC report files (plain-text InBody \
                device dumps) by their CAS UUIDs. Extracts digital firmware, analog \
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
    /// Zoho ticket ID (digits only, no "document:" prefix). Matches the
    /// `ticket_id` field from the task context.
    pub ticket_id: String,
}

#[derive(Serialize)]
struct AttachmentInfo {
    cas_uuid: String,
    name: String,
    mime_type: String,
    size_bytes: i64,
}

impl Tool for ListTicketAttachmentsTool {
    const NAME: &'static str = "list_ticket_attachments";
    type Error = ToolError;
    type Args = ListTicketAttachmentsArgs;
    type Output = Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List files attached to a support ticket. Returns CAS \
                UUIDs that can be fed directly into `analyze_qc_report`. Call \
                this BEFORE asking the human — most QC reports are already \
                downloaded from Zoho and just need to be located."
                .to_string(),
            parameters: schemars::schema_for!(ListTicketAttachmentsArgs).into(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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
                    out.size_bytes AS size_bytes \
                 FROM has_attachment \
                 WHERE in = type::record($trid) \
                 AND out.cas_uuid IS NOT NONE",
            )
            .bind(("trid", format!("document:`{}`", args.ticket_id)))
            .await?
            .take(0)?;

        let attachments: Vec<AttachmentInfo> = rows
            .into_iter()
            .filter_map(|r| {
                let cas = r.get("cas_uuid").and_then(|v| v.as_str())?.to_string();
                Some(AttachmentInfo {
                    cas_uuid: cas,
                    name: r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    mime_type: r
                        .get("mime_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("application/octet-stream")
                        .to_string(),
                    size_bytes: r.get("size_bytes").and_then(|v| v.as_i64()).unwrap_or(0),
                })
            })
            .collect();

        debug!(
            "[ListTicketAttachmentsTool] ticket={} found {} attachments",
            args.ticket_id,
            attachments.len()
        );
        Ok(json!({ "ticket_id": args.ticket_id, "attachments": attachments }))
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
                subject/content. Returns up to 3 matches with a compact \
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

        let bm25_fields: &[&str] = match table {
            "order" => &["issue_description", "customer_name", "order_number"],
            "document" => &["payload.content"],
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

        let rows: Vec<Value> = if q_vector.is_empty() {
            // BM25-only fallback.
            let sql = format!(
                "SELECT {projection} FROM {table} WHERE {bm25_where} LIMIT 3"
            );
            self.db.query(&sql).await?.take(0)?
        } else {
            // Hybrid: union BM25 OR-set with HNSW top-10, RRF merge.
            let sql = format!(
                "LET $vec = SELECT id, vector::distance::knn() AS distance \
                 FROM {table} WHERE embedding <|10,100|> $qe;\
                 LET $bm = SELECT id FROM {table} WHERE {bm25_where} LIMIT 10;\
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
