<!-- machine-first: generated from source audit 2026-07-29; audience=agents -->

## SCOPE
eckWMS open-core node — the MCP (Model Context Protocol) business-graph tool
catalog exposed at `POST /mcp`. Source of truth: `wms/src/mcp/tools.rs`
(`tools_list` builds the catalog, `dispatch_tool` routes calls) and
`wms/src/mcp/mod.rs` (transport, bearer auth, tier gating). Audience: an AI
agent or program calling this endpoint directly — not a human reader.

**Counts (sanity check):** `tools_list` catalogs **24** tools. `dispatch_tool`
has **19** named match arms (one per tool) plus one guard arm that routes the
5 examination-bundle tool names (`super::bundles::TOOL_NAMES`) to
`wms/src/mcp/bundles.rs`'s own `call` dispatcher, plus one
`other => error_result(...)` fallback arm that answers unknown tool names with
an MCP error result instead of a transport error — 19 + 5 = 24, consistent.

Of the 24, **11 are Master-tier-only** (filtered out of `tools_list` for the
lower tiers): `surrealql_read`, `reveal_file_local`, `export_attachment`,
`analyze_attachment_visual`, `chat_transcripts`, `chat_transcript`, plus the 5
examination-bundle tools — `bundle_begin`, `bundle_upload_chunk`,
`bundle_commit`, `bundle_abort`, `protocol_number_allocate` (enforced both at
discovery and, independently, at dispatch for every transport — see tools
19–24 below).

**Three tiers now.** `McpTier` is `Master`, `Agent`, or `Observer` (a
read-only, masked-PII tier for an unattended kiosk/exhibition session with
nobody signed in). One tool, `print_labels`, fires a physical printer, so it is
hidden from `Observer` (`tier != McpTier::Observer`) but visible to Agent and
Master; `list_printers` stays visible to all three (listing is harmless). Net
visibility: **Master 24**, **Agent 13**, **Observer 12**.

## Surface contract

- **Endpoint:** `POST /mcp` (also `POST /mcp/signed` — a separate, cert-signed
  ingress for paid subscription-cert callers described below).
- **Wire shape:** JSON-RPC 2.0 over the MCP Streamable-HTTP transport
  (protocol revision `2025-03-26`). Request: `{"jsonrpc":"2.0","id":N,
  "method":...,"params":...}`. Response on `/mcp`: `Content-Type:
  text/event-stream`, body `event: message\ndata: {jsonrpc-response}\n\n`.
  Methods: `initialize`, `notifications/initialized` (no response),
  `ping`, `tools/list`, `tools/call`, `reveal_tokens`.
- **Auth on `/mcp`:** header `Authorization: Bearer <token>`, compared with a
  constant-time byte compare against two configured tokens:
  - `ECK_MCP_MASTER_TOKEN` (falls back to `XELIXIR_SERVICE_TOKEN` if unset) →
    tier `Master`.
  - `ECK_MCP_AGENT_TOKEN` (optional; unset = no Agent tier available) → tier
    `Agent`.
  Neither token configured → `500 MCP token not configured`. No match →
  `401 Unauthorized`.
- **`/mcp/signed`:** no bearer token. Capability comes from an
  authority-signed `SignedClientMcp` cert embedded in the request body,
  verified by `crate::services::client_mcp::serve_signed`. This path (and the
  relay-carried poller path) always runs with the internal flag
  `over_relay = true`, which additionally hides/refuses `surrealql_read` and
  `reveal_file_local` regardless of the caller's tier — capability follows the
  credential/transport, not just the tier. Only the direct bearer `/mcp` path
  can reach those two tools.
- **`tools/list` differs by caller:** the JSON array returned depends on (a)
  tier (Master vs Agent, per the 4-tool gate above) and (b) transport
  (`over_relay` additionally drops `surrealql_read` and `reveal_file_local`
  from the list even for a Master-equivalent cert).
- **Result envelope:** every tool call in this catalog returns
  `{"content":[{"type":"text","text": "<JSON>"}]}` where `text` is a
  pretty-printed JSON string (built by the `json_result` helper) — the
  "result shape" documented per tool below is the shape of that embedded
  JSON, not the outer envelope. An error is
  `{"isError": true, "content":[{"type":"text","text": "<message>"}]}`.
- **`reveal_tokens` (JSON-RPC method, NOT a catalog tool):** Master-tier-only,
  never appears in `tools/list`, refused for the Agent tier. Resolves an
  array of pseudonym tokens back to their clear values; also re-derives
  tokens baked into stored AI summaries via a DB fingerprint lookup
  (`reveal::augment_from_db`). This is the mechanism an external client-side
  bridge tool (referred to in tool descriptions as `reveal_file`, not part of
  this catalog) is expected to call to un-mask a written report file on the
  caller's own machine. `reveal_file_local` (tool #13 below) is the
  server-local equivalent for files that already live on the WMS host.

## PII token grammar

Every tool on this surface returns names, emails, phone numbers, addresses,
and company names as **stable pseudonym tokens** instead of clear text, for
**every** tier (Agent and Master alike) — the documented exceptions are
`ocr_attachment` (returns raw scanned/OCR text unmasked), and the master-only
`analyze_attachment_visual` (sends a raw image off-node) and
`chat_transcripts`/`chat_transcript` (Brain-Chat transcripts carry the clear
names the master answered with). A
token has the form `<Label>_<16 hex uppercase digits>`, e.g.
`Name_3F2A00B1C4D5E6F7`. Valid labels: `Name`, `Email`, `Phone`, `Address`,
`Company`, `Iban`, `VatId`, `Card` (only `Name`/`Email`/`Phone`/`Address`/
`Company` are produced by this MCP surface; `Iban`/`VatId`/`Card` are produced
by the same underlying scrubber elsewhere in the codebase and share the token
format). The token is a deterministic keyed SimHash of the clear value
(peppered with the mesh `SYNC_SECRET`): the **same** person/value always
yields the **same** token, so results correlate across separate tool calls —
and a token is a **valid query key**: pass one back into `customer_360`
(identity match) or `search_database` (exact match over every record
referencing that person/value) to pivot without ever seeing the clear value.
Only a Master-tier caller can resolve tokens to clear values, and only into a
written file — via the `reveal_tokens` JSON-RPC method (through an external
bridge) or the `reveal_file_local` tool (tool #13); no tool response ever
carries a clear value alongside its token.

## Tools

### 1. customer_360

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | tokenized (identity fields always pseudonymized; `company` stays clear) |

**Description (verbatim):**
> Assemble a 360° view of one customer from their support/repair history:
> identity, the devices (serial numbers) they've sent, first & last contact,
> total number of tickets, an open/closed status breakdown, and how long open
> tickets have been waiting. Match by email (exact), by a name fragment
> (case-insensitive substring), or by a pseudonym token from any earlier
> result (Name_<hex>, Email_<hex>, Phone_<hex>, Address_<hex> — exact
> identity match). If a fragment matches several distinct customers the tool
> returns `ambiguous: true` with a candidate list instead of a merged view —
> re-query with one candidate's email token. NOTE: payment totals are not yet
> sourced — the `payments` field will say so until an invoice source is
> wired.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Customer email (exact match) or a fragment of their name/company."
    }
  },
  "required": []
}
```

**Result shape (top-level keys):**
- Not found: `{ query, found: false, message }`
- Ambiguous (>1 distinct customer matched the fragment): `{ query, found: true, ambiguous: true, matched_customers, candidates: [{ name, email, company, tickets, last_contact }], hint }`
- Resolved to one customer: `{ query, found: true, ambiguous: false, pii_revealed: false, identity: { name, email, phone, address, company }, devices: [{ serial, model }], device_count, tickets: { total, open, closed, status_breakdown, first_contact, last_contact }, open_tickets: [{ ticket_number, subject, status, serial, created_time, waiting_days }], payments }`

---

### 2. device_history

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | tokenized (`customer`, `email`, `address`; `company` stays clear) |

**Description (verbatim):**
> Full repair/support history of one device by its serial number (exact,
> case-insensitive): every ticket that unit appears in, chronological, with
> status, subject, and the customer who sent it each time — plus how many of
> those tickets are still open.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "serial": {
      "type": "string",
      "description": "Device serial number, e.g. 'F91802147'."
    }
  },
  "required": ["serial"]
}
```

**Result shape (top-level keys):**
- Not found: `{ serial, found: false, message }`
- Found: `{ serial, found: true, pii_revealed: false, models: [...], tickets_total, tickets_open, tickets: [{ ticket_number, subject, status, created_time, customer, email, address, company }] }`

---

### 3. ticket_search

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | tokenized (`customer`, `email`, `address`; `company` stays clear) |

**Description (verbatim):**
> Find support tickets by PLACE, company, ticket number, or subject, and/or
> filter by status. `query` matches (case-insensitive): the ticket number
> (exact), the ZIP (exact), or a substring of the subject, COMPANY, or CITY.
> Use `city` to search by town — it matches the ticket's stated city AND the
> town DERIVED from its phone area code when no city was on file (so a ticket
> with no address can still be found by its town); use `company` to search by
> customer/company name. `city` and `company` are ANDed with `query` and each
> other when combined. Filter by `status`: 'open', 'closed', or a literal
> status fragment (e.g. 'escalated'). To ENUMERATE every ticket in a status,
> pass only `status` — never fake a wildcard with a single-letter query. At
> least one of query/city/company/status is required. Newest first. Each hit
> carries `city` + `city_source` ('zoho' = stated, 'area_code' = derived from
> the phone, …), `zip`, `company`, and its `zoho_id` (feed that to
> list_ticket_attachments).

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Ticket number/ZIP (exact) or a fragment of the subject, company, or city. Optional when city/company/status is given."
    },
    "city": {
      "type": "string",
      "description": "Optional: town to search by (case-insensitive substring). Matches the stated city OR the town derived from the phone area code."
    },
    "company": {
      "type": "string",
      "description": "Optional: company/customer name to search by (case-insensitive substring)."
    },
    "status": {
      "type": "string",
      "description": "Optional: 'open', 'closed', or a status fragment to filter by. Alone it enumerates the whole status."
    },
    "limit": {
      "type": "integer",
      "description": "Max results (default 20, cap 50)."
    }
  },
  "required": []
}
```
(At least one of `query`/`city`/`company`/`status` is required — enforced in
the handler, not the JSON schema.)

**Result shape (top-level keys):**
`{ query, status_filter, matched_before_status_filter, returned, pii_revealed: false, tickets: [{ ticket_number, zoho_id, subject, status, created_time, serial, model, customer, email, address, company, city, city_source, zip }] }`

---

### 4. similar_tickets

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | tokenized (`customer`, `address`; `company` stays clear) |

**Description (verbatim):**
> Find support tickets semantically similar to a given one (nearest
> neighbors in embedding space via the HNSW index — works across languages,
> no keyword overlap needed). Use it to spot repeat issues, related cases
> from the same customer, or known fixes for the same symptom.

**inputSchema (verbatim):**
```json
{
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
```

**Result shape (top-level keys):**
- Not found / not embedded: `{ ticket_number, found: false, message }`
- Found: `{ ticket_number, found: true, pii_revealed: false, neighbors: [{ ticket_number, subject, status, serial, model, customer, address, company, created_time, distance }] }`

---

### 5. search_database

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | tokenized (`order.customer_name`); `document` rows return the already PPRL-masked `ai_summary` from distillation time, no extra masking pass applied here |

**Description (verbatim):**
> Hybrid BM25 + vector search over one table when exact keys fail — the same
> fuzzy retriever the internal brain uses. `table`='order' finds repair/RMA
> records by order number, customer name, or issue description; `table`=
> 'document' finds Zoho support tickets by subject/summary content. A
> pseudonym token (Name_<hex>, Email_<hex>, …) as the query switches to an
> EXACT match — every record referencing that person. Returns up to 3
> compact fuzzy matches (10 for exact token lookups) for disambiguation.

**inputSchema (verbatim):**
```json
{
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
```

**Result shape (top-level keys):**
- Normal (fuzzy or empty query): `{ table, matches: [...] }`
  - `table='order'` row projection: `{ id, order_number, customer_name, product_name, issue_description, status }`
  - `table='document'` row projection: `{ id, ticket_number, subject, ai_summary, status }`
- Pseudonym-token query (exact match path): `{ table, match_mode: "exact_token", matches: [...] }`, and for `table='order'` with a non-`Name` token label: `{ table: "order", match_mode: "exact_token", matches: [], note }` (an `order` row only carries a customer name, so only `Name_` tokens can match it).

---

### 6. list_ticket_attachments

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | n/a — no identity fields in the response; the original filename is passed through **unmasked** (a filename may incidentally contain a name) |

**Description (verbatim):**
> List files attached to a support ticket — by its Zoho ticket ID (the
> internal 17-digit document id / `zoho_id`) or the plain public ticket
> number. Returns CAS UUIDs that can be fed directly into
> `analyze_qc_report`, `ocr_attachment`, or `export_attachment`, each with
> `linked_tickets` (how many tickets share that file) and
> `linked_ticket_numbers` (up to 10 of their public numbers). The count
> alone misleads: 3 known blank templates link 180+ tickets, but a small
> shared count usually means one customer's document filed on several of
> their own tickets — check the numbers before calling a shared file a
> template. A classified file also carries `doc_type`
> (reparaturauftrag/kostenvoranschlag/invoice/shipping_label/
> datenverarbeitung/qc_report/photo_screen/other) and `is_blank` (unfilled
> template) so you can pick the right file without OCRing every one.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "ticket_id": {
      "type": "string",
      "description": "Zoho ticket ID or plain ticket number (digits only, no 'document:' prefix)."
    }
  },
  "required": ["ticket_id"]
}
```

**Result shape (top-level keys):**
`{ ticket_id, attachments: [{ cas_uuid, name, mime_type, size_bytes, linked_tickets, linked_ticket_numbers, doc_type?, is_blank? }] }` (`doc_type`/`is_blank` present only when the file has been classified).

---

### 7. ticket_thread

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | tokenized (`author`, `from`, and any name/email/phone found inside `text`) |

**Description (verbatim):**
> Read the actual back-and-forth MESSAGE TEXT of a support ticket — what the
> customer and the agent literally wrote, with the quoted history of earlier
> messages stripped out (deterministic, no AI). Use this when the AI summary
> and attachments aren't enough and you need a specific reply ("did the
> customer agree to the Kostenvoranschlag?", "what serial did they
> mention?"). Accepts a public ticket number (e.g. "31107") or the internal
> document id. Returns messages oldest→newest, each with {seq, direction
> (in=customer→us / out=us→customer), author, from, created_time, text,
> method}; `method` names the extraction strategy that won. PII is
> TOKENIZED exactly like the other tools — `author`/`from` come back as
> Name_/Email_ tokens and any name/email/phone inside `text` is replaced
> with its stable pseudonym, so the message text is safe to read while
> staying correlatable and revealable. `only_new` (default true) returns
> just the fresh text; set it false to also get `full_text_chars` (the size
> of the un-stripped message) per row. The full body lives only on the node
> that scraped the ticket: a message whose raw payload is absent here comes
> back with `text: null, method: "raw_unavailable"` rather than failing the
> call. If the ticket has more than `limit` messages, the NEWEST `limit`
> are returned and `truncated` is set.

**inputSchema (verbatim):**
```json
{
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
```

**Result shape (top-level keys):**
- Not found: `{ ticket, parent_document_id, found: false, message }`
- Found: `{ ticket, parent_document_id, found: true, pii_revealed: false, thread_count, returned, truncated, only_new, messages: [{ seq, direction, author, from, created_time, text, method, full_text_chars? }] }` (`text`/`author`/`from` can be `null`; `method` is `"raw_unavailable"` when the local raw payload is missing).

---

### 8. analyze_qc_report

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | n/a (extracts device firmware/serial fields from a plain-text QC dump; no customer identity involved) |

**Description (verbatim):**
> Analyze one or more QC report files (plain-text device QC dumps) by
> their CAS UUIDs: extracts digital firmware, analog firmware, and serial
> number from each file.

**inputSchema (verbatim):**
```json
{
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
```

**Result shape (top-level keys):**
`{ reports: [{ file_id, status, digital_fw, analog_fw, serial, error }] }` — `status` is one of `"ok"` (regex pattern matched), `"not_found"` (file/storage_path missing), or `"no_match"` (file exists, firmware pattern absent); `digital_fw`/`analog_fw`/`serial`/`error` are nullable depending on `status`.

---

### 9. ocr_attachment

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only (local only — no AI call, not metered) |
| PII behavior | **not tokenized** — returns raw extracted/OCR text as-is, which may contain clear PII from the scanned document |

**Description (verbatim):**
> Extract text from ticket attachments (PDF scans / photos) by their CAS
> UUIDs — purely local, no AI, not metered. Tries the PDF text layer first,
> then decodes the page images (office-scanner CCITT G3/G4 fax, DCT JPEG, or
> raw bitmaps) and OCRs them. Returns per file: status
> ('ok'|'not_found'|'ocr_unavailable'|'encrypted'|'unsupported_codec').
> 'encrypted' means the file needs a real user password (owner-password
> PDFs are decrypted automatically and read normally); 'unsupported_codec'
> means the page images use a codec this build cannot decode
> (JBIG2Decode/JPXDecode) — named in the `codec` field, bytes still intact
> for export_attachment. Also returns source
> ('text_layer'|'ocrs'|'external_cmd'), the text (capped 6000 chars), a
> quality block (chars, lines, dictionary_ratio, garbage_ratio) to judge
> trustworthiness, recognized line boxes (normalized 0..1), and page count.
> The extract is cached on the file (with a normalized-text fingerprint
> that clusters re-uploaded copies of the same form) and mesh-replicated,
> so repeat calls are free and a node without the blob still returns the
> stored text. Call this BEFORE analyze_attachment_visual.

**inputSchema (verbatim):**
```json
{
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
```

**Result shape (top-level keys):**
`{ files: [{ file_id, name, status, source, text, quality: { chars, lines, dictionary_ratio, garbage_ratio }, lines: [{ text, x, y, w, h }], pages, codec? }] }` — `codec` present only when `status = "unsupported_codec"`.

---

### 10. analyze_attachment_visual

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | metered-AI (also appends one row to a server-local `vision_analysis` journal, never mesh-synced) |
| PII behavior | **not tokenized** — sends the raw page image off-node to a vision model, bypassing the PII masking layer entirely |

**Description (verbatim):**
> Ask Gemini vision a question about ONE attachment page image. Call
> `ocr_attachment` FIRST — only use this when the OCR text is insufficient.
> A scan bypasses PII masking, so prefer the SMALLEST region that answers
> the question (e.g. only the signature zone) and avoid regions with
> names/addresses unless required. Returns {status:'ok', answer, model,
> tokens_in, tokens_out}; may instead return 'denied_by_policy',
> 'not_found', or 'no_image'. Every response also carries prior_analyses:
> up to 5 recent earlier Q&A for this file — reuse an already-paid answer
> instead of asking again. METERED. Master token only.

**inputSchema (verbatim):**
```json
{
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
```

**Result shape (top-level keys):**
`{ status, answer?, model?, tokens_in?, tokens_out?, error?, hint?, prior_analyses: [{ question, answer, created_at }] }`. `status` ∈ `"ok"`, `"denied_by_policy"` (node vision policy = strict), `"budget_halt"` (AI budget circuit breaker), `"not_found"`, `"no_image"` (nothing rasterizable, or an owner-password-encrypted PDF), `"ai_unavailable"`, `"error"`.

---

### 11. export_attachment

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | writes-file (server-local disk) |
| PII behavior | n/a — copies raw bytes verbatim; whatever PII the original file contains is written as-is |

**Description (verbatim):**
> Copy one attachment's raw bytes out of the CAS to an absolute destination
> path on the WMS server's disk. Master token only. ADD-ONLY: it never
> overwrites — a destination that already exists is an error. Follows
> `replaced_by` chains, so it works for files ocr_attachment cannot read
> (encrypted owner-password PDFs, AVIF-optimized originals) — the bytes are
> always intact. The sha256 is verified against the file_resource record
> before anything is written. `file_id` is a CAS UUID from
> list_ticket_attachments. Returns {status:'ok', dest_path, size_bytes,
> sha256, original_name, mime_type, cas_uuid}.

**inputSchema (verbatim):**
```json
{
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
```

**Result shape (top-level keys):**
`{ status: "ok", dest_path, size_bytes, sha256, original_name, mime_type, cas_uuid }`; a pre-existing destination, a non-absolute path, or a sha256 mismatch each return an `isError` result instead (nothing is written).

---

### 12. surrealql_read

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | read-only |
| PII behavior | clear-for-master — no tokenization pass; Zone-1 tables (credentials, PII sidecars) are refused outright, everything else is returned raw |

**Description (verbatim):**
> Run a read-only SurrealQL query (SELECT / INFO / LIVE / RETURN) against
> the node's business database — the same governed window the ops plane
> uses. Zone-1 tables (credentials, PII sidecars) are refused. Master token
> only.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "A single read-only SurrealQL statement."
    }
  },
  "required": []
}
```

**Result shape (top-level keys):**
`{ result: [...], duration_ms }` on success; `{ result: null, warning, duration_ms }` when the query ran but the response failed to decode. A non-read-only verb or a Zone-1 table reference returns an `isError` result instead of executing. Never reachable over the relay/signed transport (`over_relay = true` refuses and hides it) — bearer `/mcp` with the Master token only.

---

### 13. reveal_file_local

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | writes-file (server-local disk; overwrites `path` or writes `output` in the same directory) |
| PII behavior | clear-for-master — this IS the reveal mechanism: real values are written into the file, never into the tool result |

**Description (verbatim):**
> Server-local variant of the shim's `reveal_file` bridge tool, for when the
> report file lives on the WMS server's OWN disk (same machine, or a shared
> disk mounted there) rather than on your client machine. After writing a
> report to a file, call this with its ABSOLUTE server-local path to
> replace the pseudonym tokens (Name_…, Email_…, Phone_…, Address_…,
> Company_…) with the real values IN THE FILE. The real values are written
> to the file on the server — only counts and the output path are returned
> here, never the clear values. Optional `output` (must be in the SAME
> directory as `path`) writes a revealed copy instead of overwriting.
> Master token only.

**inputSchema (verbatim):**
```json
{
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
```

**Result shape (top-level keys):**
`{ revealed_count, unresolved_count, output_path }` — counts and a path only, never a clear value. Constraints: `path` must be absolute, a regular file, ≤10 MiB; `output` (if given) must be an absolute path in the SAME directory as `path`. Never reachable over the relay/signed transport.

---

### 14. ask_brain

| field | value |
|---|---|
| tier | agent, master |
| side-effects | metered-AI (bills like any other brain/agent run; capped to `ECK_BRAIN_CONCURRENCY` concurrent runs per node, default 1) |
| PII behavior | tier-dependent: for the **Agent** tier the final answer text is passed through the plain regex PII scrub (`scrub_pii_regex`) as a belt-and-braces net (not the full stable-token scheme); for the **Master** tier the answer is returned as the model produced it, unscrubbed |

**Description (verbatim):**
> Delegate a whole question to the node's internal brain — the same Gemini
> agent that runs the WMS Central Brain, with its own tools
> (search_database, list_ticket_attachments, analyze_qc_report). Use it for
> multi-step questions that need reasoning across several lookups. METERED:
> the run is billed like any other brain work — prefer the direct tools for
> simple lookups.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "question": {
      "type": "string",
      "description": "The question for the internal agent, self-contained."
    },
    "context": {
      "type": "string",
      "description": "Optional supporting data (treated as untrusted data, max ~8k chars)."
    },
    "history": {
      "type": "array",
      "description": "Optional prior conversation turns (verbatim tail) for a multi-turn, cache-friendly chat. Each item is {role: 'user'|'assistant', text}.",
      "items": { "type": "object", "properties": { "role": {"type":"string"}, "text": {"type":"string"} } }
    },
    "summary": {
      "type": "string",
      "description": "Optional compacted running summary carried from a prior turn."
    },
    "session_id": {
      "type": "string",
      "description": "Optional transcript-telemetry session id (server mints one if absent)."
    }
  },
  "required": ["question"]
}
```

**Result shape (top-level keys):**
`{ answer, model, engine: "gemini", billing }` on success; an `isError` result if the AI budget circuit breaker is at halt level or the node is already running its concurrency limit of brain runs (retry shortly). A `session_id` echo ties the run to a stored Brain-Chat transcript (see `chat_transcripts`/`chat_transcript`).

---

### 15. chat_transcripts

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | read-only |
| PII behavior | **clear-for-master** — session listings carry the clear customer names the brain answered with; master surface only |

**Description (verbatim):**
> List recent Brain-Chat sessions (dashboard/shim/mcp) with per-session
> aggregates (turns, avg cache ratio, total cost, compactions). Master-only —
> transcripts hold clear customer names.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "limit": { "type": "integer", "description": "Max sessions (default 50)." },
    "user": { "type": "string", "description": "Optional: filter to one user." }
  }
}
```

**Result shape:** a list of session rows with the aggregates named above (turns, avg cache ratio, total cost, compactions) keyed by `session_id`.

---

### 16. chat_transcript

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | read-only |
| PII behavior | **clear-for-master** — full transcript with clear names; master surface only |

**Description (verbatim):**
> The full markdown transcript of one Brain-Chat session (Q/A + per-turn
> metrics + compaction summaries) — what the owner's analyst reads.
> Master-only.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "session_id": { "type": "string", "description": "The chat session id (from chat_transcripts)." }
  },
  "required": ["session_id"]
}
```

**Result shape:** the markdown transcript text of the one session.

---

### 17. list_printers

| field | value |
|---|---|
| tier | agent, master, **observer** (visible to all three) |
| side-effects | read-only (queries the node's CUPS scheduler) |
| PII behavior | n/a — printer queue names only |

**Description (verbatim):**
> List the physical printers (CUPS queues) available on THIS node that accept
> jobs — name, whether it is the node default, and its CUPS state note. Feed a
> `name` into `print_labels` as `printer` to target a specific queue. An empty
> list means the node has no CUPS scheduler or no configured printers (labels
> can still be generated as a downloadable PDF, just not printed here).

**inputSchema (verbatim):**
```json
{ "type": "object", "properties": {}, "required": [] }
```

**Result shape:** a list of `{ name, is_default, state }` printer rows (empty when no CUPS scheduler / no configured queues).

---

### 18. print_labels

| field | value |
|---|---|
| tier | agent, master (**hidden from observer** — fires a physical printer) |
| side-effects | **writes physical output** (submits a CUPS print job) |
| PII behavior | n/a — generates ECK label sheets from serial ranges, no identity fields |

**Description (verbatim):**
> Generate a sheet of ECK labels and print it directly to a physical printer
> on this node (CUPS). `type` picks the label kind: 'i' item, 'b' box, 'p'
> place/bin, 'l' label. `count` is how many labels; `start_number` the first
> serial (default 0). `cols`/`rows` set the grid (default 3×7 = 21 per A4
> page). `printer` names the queue from `list_printers` (omit for the node
> default); `copies` prints the whole sheet N times (1..=50). Returns the CUPS
> job id. Use `list_printers` first if unsure which queues exist.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "type": { "type": "string", "description": "Label kind: 'i' (item), 'b' (box), 'p' (place/bin), 'l' (label). Default 'i'." },
    "count": { "type": "integer", "description": "Number of labels to generate." },
    "start_number": { "type": "integer", "description": "First serial number (default 0)." },
    "cols": { "type": "integer", "description": "Grid columns per page (default 3)." },
    "rows": { "type": "integer", "description": "Grid rows per page (default 7)." },
    "printer": { "type": "string", "description": "Target CUPS queue name (from list_printers); omit for the node default." },
    "copies": { "type": "integer", "description": "How many times to print the sheet (1..=50, default 1)." }
  },
  "required": ["type", "count"]
}
```

**Result shape:** `{ status, job_id, printer, ... }` on success (the submitted CUPS job id); an `isError` result when the tier is Observer, no CUPS scheduler is present, or the queue rejects the job.

---

### 19. ticket_stats

| field | value |
|---|---|
| tier | agent, master |
| side-effects | read-only |
| PII behavior | aggregated counts/groups only — no name/email/phone/address fields are returned; `company`/`city` keys stay clear (same as `ticket_search`) |

**Description (verbatim):**
> Aggregate statistics over support tickets, computed server-side over ALL
> tickets (never paged, never capped at 50) — use this for any 'how many',
> 'oldest'/'newest N', or 'per status / company / city / period' question
> instead of paging `ticket_search`. All filters are optional and ANDed:
> `status` ('open', 'closed', or a status fragment — same vocabulary as
> ticket_search), `company` and `city` (case-insensitive substring), and the
> date window `created_from` (inclusive) / `created_to` (EXCLUSIVE), each an
> ISO date ('2026-09-01') or datetime. Returns `matched_total` (the true
> count after every filter), `status_counts` (per raw status), and `rows`:
> the top `top` tickets (default 5, max 20) by `order` ('oldest' default, or
> 'newest'), each with ticket_number, subject, status, created_time,
> `age_days`, and company. Set `group_by` ('status'|'company'|'city'|'model')
> to also get `groups` (top 20 by count + an 'other' bucket). No free-text
> query and no raw SQL — filter by the fields above.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "status": { "type": "string", "description": "Optional: 'open', 'closed', or a status fragment." },
    "company": { "type": "string", "description": "Optional: company/customer name (case-insensitive substring)." },
    "city": { "type": "string", "description": "Optional: town (stated or derived from the phone area code)." },
    "created_from": { "type": "string", "description": "Optional: only tickets created on/after this ISO date/datetime (inclusive)." },
    "created_to": { "type": "string", "description": "Optional: only tickets created before this ISO date/datetime (exclusive)." },
    "group_by": { "type": "string", "description": "Optional: 'status', 'company', 'city', or 'model' to return per-group counts." },
    "order": { "type": "string", "description": "Edge rows order: 'oldest' (default) or 'newest'." },
    "top": { "type": "integer", "description": "How many edge rows to return (1..20, default 5)." }
  },
  "required": []
}
```

**Result shape (top-level keys):**
`{ matched_total, status_counts: {status: count}, rows: [{ ticket_number, subject, status, created_time, age_days, company }], groups?: [{ key, count }] (present only when `group_by` is set, max 20 + an "other" bucket), as_of, filters: {status, company, city, created_from, created_to, group_by, order, top} (filters as actually applied), pii_revealed: false }`. An invalid `created_from`/`created_to` returns an `isError` result instead of silently ignoring the bound. Shares its status-matching and date-comparator logic with `ticket_search` (same vocabulary, same undated-last ordering).

---

### 20. bundle_begin

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | writes (creates/updates a `bundle_staging` row and, if the serial is unknown, a `device` record; no file bytes yet) |
| PII behavior | n/a — device/ticket identifiers only, no customer identity fields |

**Description (verbatim):**
> MASTER ONLY. Open (or refresh, idempotent while open) the staging area for
> an examination bundle (EN 62353 STK/MTK/Funktionskontrolle/repair evidence)
> of one device. Creates the `device` record for the serial if missing.
> Rejects an unknown ticket_id. Staging expires 48 h after the last begin.
> See docs/MESH_BUNDLE_TOOLS.md.

**inputSchema (verbatim, `manufacturer`/`model` descriptions paraphrased — the
source default and example name a tenant-specific brand, not reproduced
here):**
```json
{
  "type": "object",
  "properties": {
    "bundle_id": { "type": "string", "description": "Client idempotency key, [A-Za-z0-9._-]{1,128}." },
    "serial": { "type": "string", "description": "Device serial number." },
    "manufacturer": { "type": "string", "description": "Defaults to the tenant's configured device manufacturer when omitted." },
    "model": { "type": "string", "description": "e.g. a device model name/number." },
    "udi": { "type": "string" },
    "ticket_id": { "type": "string", "description": "Ticket document id (Zoho id) or ticket number." },
    "manifest": { "type": "object", "description": "The bundle manifest JSON ({files:{name:sha256}, protocol_no, dut, …})." },
    "schema_version": { "type": "integer", "description": "Must be 1." }
  },
  "required": ["bundle_id", "serial", "manifest"]
}
```

**Result shape (top-level keys):**
`{ bundle_id, device_id, device_created, ticket, expires_at, state: "open" }` on a fresh/refreshed begin; `{ bundle_id, device_id, device_created: false, ticket, state: "committed", receipt }` when the bundle_id was already committed with the identical ticket/manifest (idempotent replay — no new staging). An `isError` result on an unknown `ticket_id`, a `manifest` that isn't a JSON object, an unsupported `schema_version`, a re-begin with a different serial (`conflict`), or a bundle previously aborted (`aborted` — start a new `bundle_id`).

---

### 21. bundle_upload_chunk

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | writes (stages chunk bytes on local disk under `data/bundle_staging/<bundle_id>/`; assembles + verifies the file once all chunks arrive) |
| PII behavior | n/a — raw file bytes, no structured identity fields |

**Description (verbatim):**
> MASTER ONLY. Upload one chunk (≤ 640 KiB raw, base64) of one bundle file.
> Chunks may arrive in any order; a repeated chunk is ignored. When all
> chunks are present the node verifies size_bytes + sha256 and marks the file
> complete (a mismatch drops the file so it can be re-declared).

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "bundle_id": { "type": "string" },
    "role": { "type": "string", "enum": ["protocol_pdf", "protocol_html", "tester_payload", "manifest", "photo", "other"] },
    "file_name": { "type": "string" },
    "mime_type": { "type": "string" },
    "size_bytes": { "type": "integer" },
    "sha256": { "type": "string", "description": "sha256 hex of the WHOLE file." },
    "chunks_total": { "type": "integer" },
    "chunk_index": { "type": "integer", "description": "0-based." },
    "content_base64": { "type": "string" }
  },
  "required": ["bundle_id", "role", "file_name", "size_bytes", "sha256", "chunks_total", "chunk_index", "content_base64"]
}
```

**Result shape (top-level keys):**
`{ file_name, role, received, chunks_total, complete, sha256_ok, duplicate }` — `duplicate: true` when `chunk_index` was already stored (byte content not rewritten). Once `received == chunks_total` the node assembles and re-hashes the file: on a match `complete`/`sha256_ok` flip `true`; on a mismatch the file entry is dropped (`isError`, `sha256_mismatch`) and must be re-declared from chunk 0. After the bundle is already `committed`, a chunk matching a file already in the receipt returns `{ ..., complete: true, sha256_ok: true, already_committed: true }`; one that doesn't match any committed file's sha256 is an `isError` (`closed`). `role` must be one of the enum values above (actual enum sourced from `ROLES` in `bundles.rs`).

---

### 22. bundle_commit

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | writes (lands staged files into the CAS as `file_resource`, creates one `device_event`, `has_attachment` provenance edges from the event — and the ticket, if any — and marks the `protocol_reservation` + `bundle_staging` rows committed; one DB transaction) |
| PII behavior | n/a — device/event/protocol fields only |

**Description (verbatim):**
> MASTER ONLY. Validate the staged bundle (all files complete +
> sha256-verified, manifest consistent, required roles present, protocol_no
> reserved for THIS bundle) and atomically file it: CAS blobs, device_event,
> provenance has_attachment edges from the event and the ticket. An identical
> retry returns the earlier receipt; a different event for the same
> bundle_id fails with `conflict`. Corrections = new bundle with revision+1
> and supersedes_protocol_no.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "bundle_id": { "type": "string" },
    "event": {
      "type": "object",
      "properties": {
        "kind": { "type": "string", "enum": ["STK", "MTK", "FUNKTIONSKONTROLLE", "REPAIR", "OTHER"] },
        "date": { "type": "string", "description": "YYYY-MM-DD" },
        "protocol_no": { "type": "string", "description": "YYYY-NNNN" },
        "revision": { "type": "integer" },
        "result": { "type": "string", "enum": ["PASS", "PASS_WITH_NOTES", "FAIL", "NOT_ASSESSED"] },
        "next_due": { "type": ["string", "null"], "description": "YYYY-MM or null" },
        "examiner": { "type": "string" },
        "instrument": { "type": "object" },
        "standard": { "type": "string" },
        "legal_basis": { "type": "string" },
        "scope": { "type": "string" },
        "operator_snapshot": { "type": "object" },
        "notes": { "type": "string" },
        "supersedes_protocol_no": { "type": ["string", "null"] }
      },
      "required": ["kind", "date", "protocol_no", "result", "examiner"]
    },
    "required_roles": { "type": "array", "items": { "type": "string" }, "description": "Default: protocol_pdf, protocol_html, tester_payload, manifest." }
  },
  "required": ["bundle_id", "event"]
}
```

**Result shape (top-level keys):**
The receipt: `{ event_id, device_id, protocol_no, bundle_id, files: [{ role, name, cas_uuid, sha256, deduped }], manifest_covered, manifest_missing, receipt_sha256 }`. An identical retry (same `event` payload hash) replays the stored receipt unchanged. An `isError` result on: any staged file not complete/sha256-verified (`incomplete`), a required role with no file, a manifest/serial or manifest/protocol mismatch (`manifest_mismatch`), `protocol_no` not reserved for this bundle (`protocol_not_reserved` — call `protocol_number_allocate` first) or already taken by another bundle (`protocol_no_taken`), a different committed event for the same `bundle_id` (`conflict`), or a `supersedes_protocol_no` with no matching prior event (`supersedes_not_found`). Coverage against the manifest is by sha256, not by file name — files outside the manifest pass on their own verified hash.

---

### 23. bundle_abort

| field | value |
|---|---|
| tier | **master-only** |
| side-effects | writes (marks the `bundle_staging` row `aborted` and deletes its staged tmp bytes from disk) |
| PII behavior | n/a |

**Description (verbatim):**
> MASTER ONLY. Abort an open bundle staging and delete its tmp bytes. A
> protocol number reserved for it stays reserved (numbers are never
> recycled).

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": { "bundle_id": { "type": "string" } },
  "required": ["bundle_id"]
}
```

**Result shape (top-level keys):**
`{ bundle_id, state: "aborted" }`. Idempotent — aborting an already-aborted bundle returns the same shape without error. An `isError` result (`closed`) if the bundle is already `committed`. A reservation held by the bundle is NOT released (protocol numbers are never recycled).

---

### 24. protocol_number_allocate

| field | value |
|---|---|
| tier | **master-only**, and only on the authoritative writer node (`ECK_PROTOCOL_WRITER=1`) |
| side-effects | writes (`protocol_counter` + `protocol_reservation` rows, one transaction) |
| PII behavior | n/a |

**Description (verbatim):**
> MASTER ONLY, authoritative writer node only (else `not_writer`). Reserve
> the next protocol number YYYY-NNNN for a bundle; the same bundle_id always
> gets the same number back (`reused: true`). Numbers are never recycled.

**inputSchema (verbatim):**
```json
{
  "type": "object",
  "properties": {
    "year": { "type": "integer" },
    "bundle_id": { "type": "string" },
    "kind": { "type": "string", "enum": ["STK", "MTK", "FUNKTIONSKONTROLLE", "REPAIR", "OTHER"] }
  },
  "required": ["year", "bundle_id"]
}
```

**Result shape (top-level keys):**
`{ number: "YYYY-NNNN", reserved_at, reused, state: "reserved" }`. A repeat call with the same `bundle_id`+`year` returns the existing reservation with `reused: true` (and the reservation's current `state`, e.g. `"committed"` after `bundle_commit` landed it) instead of allocating a new number. The number is `max(protocol_counter, every reservation, every recorded device_event for that year) + 1` — never re-issued even if the counter row is lost. An `isError` result (`not_writer`) on any node where `ECK_PROTOCOL_WRITER` is unset.
