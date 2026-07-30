<!-- machine-first: generated from source audit 2026-07-29; audience=agents -->
## SCOPE

Every environment variable read anywhere in the open-source workspace crates
(`core`, `wms`, `relay`, `compliance`, `migrator`) via `std::env::var(...)` or
`std::env::var_os(...)` (directly or through a local wrapper that itself calls one of
those), plus `dotenvy` usage. A short **commercial edition** subsection at the end
covers `pos` (ecKasse register) and `shim` (desktop MCP-connector bridge) — neither
crate ships in the open-source workspace; their vars/routes are listed, not omitted,
per the audit brief.

`dotenvy::dotenv()` is called once at process start in `wms/src/main.rs`
(`async_main`, loads `.env` from CWD before anything else runs) and in
`migrator/src/main.rs`. `core`, `relay`, and `compliance` do NOT call `dotenvy` — they
rely on the process environment already being populated (by a `.env` a caller loaded,
or by systemd `EnvironmentFile=`).

Field meaning in the YAML block below:
- `name` — exact env var string.
- `files` — every file this audit found reading it (relative to repo root).
- `default` — literal fallback value used when unset, or `"none — <consequence>"` when
  there is no fallback (feature disabled vs. hard panic/error, stated explicitly).
- `effect` — one factual sentence: what the var controls.
- `feature` — one of: `mesh`, `AI-live`, `AI-batch`, `MCP`, `POS`, `scraper-proxy`,
  `PDA`, `geo`, `i18n`, `ops`, `relay`, `managed-seam`, `other`. `managed-seam` is used
  for the six vars that point a node at an EXTERNAL commercial licensing/AI-minting
  authority — see the dedicated note before that block.
- `required` — `true` only when the *process itself* cannot start, or a *default
  no-AI, single-node deployment* cannot run, without it set. Most vars are `false`
  because they gate an optional feature or have a workable default; where a var is
  required only *after* another feature is opted into, that condition is spelled out
  in `effect` and `required` stays `false`.

Some var names read by two different code paths with two different literal defaults
(e.g. `RELAY_URL`, `SURREAL_DB_PATH`, `PORT`) are listed ONCE with both defaults
called out in `effect` — do not assume one default applies fleet-wide; it depends on
which binary (node / relay / commercial pos) is reading it.

---

## 1. mesh

```yaml
vars:
  - name: SYNC_SECRET
    files: [core/src/utils/anonymizer.rs, wms/src/main.rs, wms/src/middleware/mesh_auth.rs]
    default: "none — core/src/utils/anonymizer.rs panics (.expect) the first time PII anonymization runs (e.g. any support-ticket import), which happens in ordinary non-AI use; wms/src/main.rs additionally asserts at boot when AI is enabled; wms/src/middleware/mesh_auth.rs treats an unset value as dev-mode passthrough (P2P /api/mesh/* routes become unauthenticated)"
    effect: "shared secret used both as the PPRL anonymization pepper AND as the P2P mesh-auth bearer token peers present to each other's /api/mesh/* routes"
    feature: mesh
    required: true
  - name: ECK_CHECKSUM_SWEEP_SECS
    files: [wms/src/services/scheduler.rs]
    default: "none — hourly entity-checksum/merkle-root sweep is disabled"
    effect: "interval in seconds for the idle-CPU merkle-root cache + checksum sweep; enable on one node only per prior incident notes"
    feature: mesh
    required: false
  - name: MESH_DATA_KEY
    files: [core/src/db.rs]
    default: "none — mesh-data encryption at rest is skipped"
    effect: "symmetric key used to encrypt mesh-synced data at rest in SurrealDB"
    feature: mesh
    required: false
  - name: SERVER_PUBLIC_KEY
    files: [core/src/utils/identity.rs]
    default: "none — a fresh Ed25519 keypair is generated and NOT persisted across restarts unless the caller also sets SERVER_PRIVATE_KEY"
    effect: "this node's public identity key (Ed25519, hex), used for signed mesh envelopes"
    feature: mesh
    required: false
  - name: SERVER_PRIVATE_KEY
    files: [core/src/utils/identity.rs]
    default: "none — see SERVER_PUBLIC_KEY; without both, node identity is ephemeral (regenerated every restart)"
    effect: "this node's private identity key (Ed25519, hex) — must be paired with SERVER_PUBLIC_KEY for a stable node identity"
    feature: mesh
    required: false
  - name: BASE_URL
    files: [wms/src/main.rs, wms/src/handlers/mesh.rs, wms/src/handlers/device.rs, wms/src/handlers/rma.rs, wms/src/mcp/connector.rs]
    default: "empty string — falls back to auto-detected outbound LAN IP + listen port for heartbeat/QR/link generation"
    effect: "this node's externally-reachable base URL, announced to the relay on heartbeat and embedded in pairing QR codes, RMA agreement links, and the MCP connector bundle"
    feature: mesh
    required: false
  - name: ECK_BOARD_URL
    files: [wms/src/services/mesh_relay_poller.rs]
    default: "none — falls back to the primary relay URL (see RELAY_URL/RELAY_URLS)"
    effect: "override URL for the relay-carried mesh task poller if it differs from the main relay"
    feature: mesh
    required: false
  - name: MESH_DEVICE_URLS
    files: [wms/src/handlers/device.rs]
    default: "none — cross-device direct-URL hints are skipped"
    effect: "comma-separated list of known peer device base URLs used as pairing/discovery hints"
    feature: mesh
    required: false
  - name: INSTANCE_ID
    files: [wms/src/main.rs, "pos/src/main.rs (commercial, standalone pos binary only)"]
    default: "none — a UUID is generated and persisted to local storage on first boot"
    effect: "this node's stable instance identifier used in mesh routing, heartbeats, and vclocks"
    feature: mesh
    required: false
  - name: MESH_NODE_ROLE
    files: [wms/src/main.rs, "pos/src/main.rs (commercial, standalone pos binary only)"]
    default: "\"full\" (wms); pos standalone binary defaults differ — see pos/src/main.rs"
    effect: "declares this node's mesh role (e.g. full vs cache); cache-role nodes run the LRU eviction worker bounded by MESH_CACHE_BUDGET_ROWS"
    feature: mesh
    required: false
  - name: MESH_CACHE_BUDGET_ROWS
    files: [wms/src/main.rs]
    default: "10000"
    effect: "row budget for the cache-role LRU eviction worker (only meaningful when MESH_NODE_ROLE=cache)"
    feature: mesh
    required: false
```

---

## 2. relay (relay crate + node-side relay client config)

```yaml
vars:
  - name: RELAY_URL
    files: [core/src/sync/relay_client.rs, wms/src/services/xelixir_router.rs, wms/src/services/client_mcp_poller.rs, wms/src/handlers/xelixir.rs]
    default: "core/src/sync/relay_client.rs defaults to \"https://9eck.com\" (the public free discovery relay); wms-side xelixir/client-mcp pollers default to \"http://localhost:3200\" — these differ, do not assume one applies everywhere"
    effect: "primary relay base URL this node dials for heartbeats, mesh-relay fallback, xelixir C2, and the paid client-MCP channel"
    feature: relay
    required: false
  - name: RELAY_URLS
    files: [core/src/sync/relay_client.rs]
    default: "none — falls back to the single RELAY_URL"
    effect: "comma-separated list of relay URLs for multi-relay fanout/failover (RelayClient::new_multi / relay_urls_from_env)"
    feature: relay
    required: false
  - name: LAN_BASE_URL
    files: [core/src/sync/relay_client.rs]
    default: "none — LAN-direct shortcut is skipped, all traffic goes through the relay URL(s)"
    effect: "optional direct LAN URL to a known peer, tried before falling back to the relay hop"
    feature: relay
    required: false
  - name: RELAY_ACK_MAX_BYTES
    files: [wms/src/services/mesh_relay_poller.rs]
    default: "a fixed byte budget (mesh-relay ack size cap; see source for the exact constant)"
    effect: "caps the size of an ack body the mesh-relay poller will send back through the relay, avoiding the relay's own body-size limit"
    feature: relay
    required: false
  - name: RELAY_ADMIN_TOKEN
    files: [relay/src/handlers/mesh.rs, wms/src/handlers/mesh.rs]
    default: "none — relay's GET /E/registry returns 403 Forbidden unconditionally (fails closed, not open, when unset)"
    effect: "shared bearer token gating the relay's /E/registry admin listing endpoint; the wms side reads the same var name when calling that endpoint"
    feature: relay
    required: false
  - name: PAIR_FREE_RELAY
    files: [wms/src/services/agent_manager.rs]
    default: "none — free-tier pairing does not advertise a fallback NAT-traversal relay in the QR payload"
    effect: "public relay URL advertised in a free-tier pairing QR so a NAT'd PDA can reach this node"
    feature: relay
    required: false
  - name: RELAY_PAYLOAD_MODE
    files: [relay/src/handlers/mesh_relay.rs]
    default: "\"open\""
    effect: "relay-side policy gate on /E/m/dispatch: open = passthrough for everyone (internal mesh/dev); disabled = discovery-only, payload relaying refused; paid = allowed only when at least one party is on a paid tier"
    feature: relay
    required: false
  - name: RELAY_RESOLVE_TOKEN
    files: [relay/src/handlers/xelixir.rs]
    default: "none — /E/resolve/{instance_id} has no extra gate beyond what's documented in 45-http-surface.md"
    effect: "optional bearer check referenced in relay resolve-handler code paths; unset leaves the route at its default (public) gate"
    feature: relay
    required: false
  - name: ECK_LICENSE_PUBKEY
    files: [relay/src/handlers/register.rs]
    default: "none — relay does not verify a license signature on node registration"
    effect: "public key the relay can use to verify a signed license claim presented at /E/register, when configured"
    feature: relay
    required: false
  - name: ECK_LICENSE_GRACE_SECS
    files: [relay/src/handlers/register.rs]
    default: "a fixed grace-period constant when unset (see source)"
    effect: "grace window the relay allows an expired/unverified license claim before treating a registering node as unlicensed"
    feature: relay
    required: false
  - name: ECK_FILE_FETCH_FORCE_RELAY
    files: [core/src/sync/engine.rs]
    default: "false / unset — direct peer-to-peer file fetch is tried first, relay is only the NAT fallback"
    effect: "forces blob/file fetches to always go through the relay hop instead of attempting a direct P2P connection first"
    feature: relay
    required: false
```

---

## 3. MCP

```yaml
vars:
  - name: ECK_MCP_MASTER_TOKEN
    files: [wms/src/mcp/mod.rs]
    default: "none — falls back to XELIXIR_SERVICE_TOKEN; if that is also unset, POST /mcp and /mcp/signed's bearer path both return 500 (\"MCP token not configured\")"
    effect: "bearer token granting Master-tier MCP access on POST /mcp (unmasked PII reveal, surrealql_read, reveal_file_local all require this tier)"
    feature: MCP
    required: false
  - name: ECK_MCP_AGENT_TOKEN
    files: [wms/src/mcp/mod.rs]
    default: "none — Agent tier is simply unavailable; only Master-tier bearer works"
    effect: "bearer token granting Agent-tier MCP access on POST /mcp (masked-PII, tool-restricted)"
    feature: MCP
    required: false
  - name: ECK_BRAIN_CONCURRENCY
    files: [wms/src/mcp/tools.rs]
    default: "a small fixed default (see source) if unset"
    effect: "max concurrent in-flight ask_brain MCP tool calls"
    feature: MCP
    required: false
  - name: ECK_SHIM_DIST_DIR
    files: [wms/src/mcp/connector.rs]
    default: "a conventional local build-output path"
    effect: "directory the admin-only GET /api/admin/mcp-connector handler reads the pre-built shim binaries from when assembling the downloadable connector bundle"
    feature: MCP
    required: false
  - name: ECK_NODE_DIRECT_URL
    files: [core/examples/relay_mcp_client.rs, "shim/src/main.rs (commercial)"]
    default: "none — the example/shim client has no direct-LAN shortcut and always goes via the relay"
    effect: "direct base URL of this node, bypassing the relay, for an MCP client that can reach the node directly"
    feature: MCP
    required: false
```

### Managed-mode seam — points at an EXTERNAL commercial authority

The six vars below are the ONLY seam between this open-source node and a commercial
licensing/AI-minting authority. **A standalone/self-hosted deployment leaves ALL SIX
unset.** When unset, every AI code path either stays fully local/disabled or falls
back to a directly-configured Gemini Studio key (see the `AI-live` section) — nothing
about core node function depends on these.

```yaml
vars:
  - name: ECK_AI_MODE
    files: [core/src/ai.rs, wms/src/main.rs]
    default: "empty/unset — treated as Studio (self-key) mode, not managed mode"
    effect: "set to \"managed\" to route AI-auth resolution through the external authority (mint short-lived Vertex bearers) instead of a locally-held Gemini Studio API key; the wms/src/main.rs read of this var is a cosmetic startup-log label only, the real resolution logic lives in core/src/ai.rs"
    feature: managed-seam
    required: false
  - name: ECK_VERTEX_MINT_URL
    files: [core/src/ai.rs]
    default: "none — managed-mode bearer minting is unavailable; only relevant when ECK_AI_MODE=managed"
    effect: "URL of the external authority's endpoint this node calls to mint a short-lived Vertex AI bearer token"
    feature: managed-seam
    required: false
  - name: ECK_VERTEX_USAGE_URL
    files: [core/src/ai.rs]
    default: "none — usage/spend is not reported back to the external authority"
    effect: "URL this node POSTs AI token-spend/usage telemetry to under managed mode"
    feature: managed-seam
    required: false
  - name: ECK_LICENSE_TOKEN
    files: [core/src/ai.rs, wms/src/handlers/device.rs, wms/src/handlers/pda.rs]
    default: "none — managed-mode auth-mint calls have no credential to present (core/src/ai.rs); separately, wms/src/handlers/device.rs and pda.rs treat an unset/empty value as \"free tier\" and omit the paid flag from pairing QR codes / PDA status"
    effect: "this node's license/subscription credential: presented to the external authority when minting a managed Vertex bearer, AND read as a presence-only flag elsewhere to mark this node as a paid-tier node in pairing QR codes and PDA status responses. NOTE: this is a DIFFERENT variable from the unprefixed LICENSE_TOKEN used by the xelixir agent spawn path (see `ops` section) — the similar names are easy to confuse and are NOT interchangeable."
    feature: managed-seam
    required: false
  - name: ECK_VERTEX_BEARER
    files: [core/src/ai.rs]
    default: "none — no directly-injected Vertex bearer; the node either mints one via ECK_VERTEX_MINT_URL (managed) or uses GEMINI_API_KEY (Studio/self-hosted Vertex)"
    effect: "a pre-obtained Vertex AI bearer token, injected directly instead of minted — an escape hatch around the managed-mint round trip"
    feature: managed-seam
    required: false
  - name: ECK_SUB_ROOT_PUBKEY
    files: [core/src/xelixir/subscription.rs, wms/src/main.rs, wms/src/services/client_mcp_poller.rs, relay/src/handlers/client_mcp.rs]
    default: "none — the entire paid client-MCP relay channel is disabled: wms/src/main.rs's relay-carried MCP poller (client_mcp_poller) is a no-op, POST /mcp/signed returns 503, and the relay's own POST /E/c/dispatch returns 503 (\"relay MCP channel disabled\")"
    effect: "root public key used to verify an authority-signed SubscriptionCert; gates the entire paid client-MCP channel end-to-end (relay admission AND node-side re-verification) — must be set identically on both the relay and every node that wants to accept subscription-cert MCP traffic"
    feature: managed-seam
    required: false
```

---

## 4. AI-live

```yaml
vars:
  - name: GEMINI_API_KEY
    files: [core/src/ai.rs, "pos/src/ai/agent.rs (commercial, its own independent read)"]
    default: "none — Studio-mode direct-key AI path is unavailable; the node runs with AI features off unless ECK_AI_MODE=managed is also configured"
    effect: "Gemini Studio API key for direct (non-managed) self-hosted AI calls; the commercial pos crate reads the same var name independently for its embedded cashier AI chat"
    feature: AI-live
    required: false
  - name: ECK_VERTEX_PROJECT
    files: [core/src/ai.rs]
    default: "none — Vertex AI backend is unavailable; only Studio (GEMINI_API_KEY) or managed-mint auth paths work"
    effect: "GCP project id used when calling Vertex AI directly (self-hosted Vertex, not managed mode)"
    feature: AI-live
    required: false
  - name: ECK_VERTEX_LOCATION
    files: [core/src/ai.rs]
    default: "a fixed default region (see source) when unset"
    effect: "GCP region for direct Vertex AI calls"
    feature: AI-live
    required: false
  - name: ECK_EMBED_URL
    files: [wms/src/ai/embeddings.rs, wms/src/ai/local_embed.rs]
    default: "none — falls back to the Gemini embedding API; see ECK_EMBED_MODE"
    effect: "URL of a self-hosted embedding backend, when not using Gemini's hosted embedding API"
    feature: AI-live
    required: false
  - name: ECK_EMBED_MODE
    files: [wms/src/ai/embeddings.rs, wms/src/ai/local_embed.rs]
    default: "\"cloud\" (uses the Gemini embedding API)"
    effect: "selects embedding backend: cloud (Gemini) vs local (on-box model via ECK_EMBED_MODEL_DIR)"
    feature: AI-live
    required: false
  - name: ECK_EMBED_MODEL_DIR
    files: [wms/src/ai/local_embed.rs]
    default: "none — local embedding mode cannot load a model, falls back to cloud if ECK_EMBED_MODE=local was set without this"
    effect: "filesystem path to the local embedding model weights (candle-based)"
    feature: AI-live
    required: false
  - name: ECK_EMBED_WORKER
    files: [wms/src/main.rs]
    default: "1 (this node acts as an embedding producer)"
    effect: "set to 0 to make this node an embedding consumer only (mesh-syncs vectors produced elsewhere) instead of computing its own"
    feature: AI-live
    required: false
  - name: GEMINI_GENERATION_MODEL
    files: [wms/src/main.rs, wms/src/ai/orchestrator.rs, "pos/src/ai/agent.rs (commercial, own default)"]
    default: "none in wms/src/main.rs — the GeoSweep AI address-discovery stage is skipped entirely when unset (checked via `if let Ok(model) = ...`); pos/src/ai/agent.rs defaults to its own model string when unset"
    effect: "primary Gemini model id used for generation/orchestration calls (also gates whether the paid GeoSweep address-discovery stage runs at all)"
    feature: AI-live
    required: false
  - name: GEMINI_EMBEDDING_MODEL
    files: [wms/src/ai/embeddings.rs]
    default: "a fixed Gemini embedding model id when unset (see source)"
    effect: "Gemini model id used for cloud embedding calls"
    feature: AI-live
    required: false
  - name: GEMINI_SUMMARY_MODEL
    files: [wms/src/ai/summarization_batch.rs, wms/src/ai/observer.rs]
    default: "a fixed default summary model id when unset (see source)"
    effect: "Gemini model id used for ticket/document summarization (both live and batch paths reference this as their base model choice)"
    feature: AI-live
    required: false
  - name: GEMINI_ORCHESTRATOR_MODEL
    files: [wms/src/ai/orchestrator.rs]
    default: "falls back to GEMINI_GENERATION_MODEL when unset"
    effect: "Gemini model id used specifically by the AI orchestrator (operator-inbox agent loop)"
    feature: AI-live
    required: false
  - name: GEMINI_ENRICH_MODEL
    files: [wms/src/handlers/ai.rs]
    default: "falls back to GEMINI_GENERATION_MODEL when unset"
    effect: "Gemini model id used by the /api/ai/enrich-csv batch CSV enrichment endpoint"
    feature: AI-live
    required: false
  - name: GEMINI_VISION_MODEL
    files: [wms/src/ai/attachments.rs]
    default: "a fixed default vision-capable model id when unset (see source)"
    effect: "Gemini model id used for attachment OCR/vision extraction"
    feature: AI-live
    required: false
  - name: ECK_ATTACHMENT_VISION_POLICY
    files: [wms/src/ai/attachments.rs]
    default: "a conservative default policy when unset (see source)"
    effect: "policy string controlling when the vision-model OCR path is allowed to run over an attachment (cost/PII gate)"
    feature: AI-live
    required: false
  - name: ECK_OCR_CMD
    files: [wms/src/ai/attachments.rs]
    default: "none — local OCR-binary text-layer extraction step is skipped, falls through to the vision-model ladder"
    effect: "path/command for a local OCR binary tried before the AI vision fallback"
    feature: AI-live
    required: false
  - name: ECK_OCRS_MODEL_DIR
    files: [wms/src/ai/attachments.rs]
    default: "none — the local `ocrs` model path is unset, that extraction step is skipped"
    effect: "filesystem path to local OCR model weights"
    feature: AI-live
    required: false
  - name: ECK_CLASSIFY_MODEL
    files: [wms/src/ai/doc_classify.rs]
    default: "falls back to GEMINI_GENERATION_MODEL when unset"
    effect: "Gemini model id used for the per-PDF doc_class classification sweep"
    feature: AI-live
    required: false
  - name: ECK_PII_NER
    files: [wms/src/ai/local_ner.rs]
    default: "none/unset — the lexicon-based PII masker is used; set to \"local\" to enable the candle NER-BERT-German model path"
    effect: "selects the person-name detection strategy inside the subject/PII masker: lexicon fallback vs local NER model"
    feature: AI-live
    required: false
  - name: ECK_NER_MODEL_DIR
    files: [wms/src/ai/local_ner.rs]
    default: "none — required only when ECK_PII_NER=local; without it that mode cannot load weights and falls back to the lexicon masker"
    effect: "filesystem path to the local NER-BERT-German model weights"
    feature: AI-live
    required: false
  - name: ECK_PII_EMBED_POLICY
    files: [wms/src/ai/pii_policy.rs]
    default: "a conservative default policy when unset (see source)"
    effect: "policy controlling whether/how PII is masked before text is sent to the embedding API"
    feature: AI-live
    required: false
  - name: ECK_PII_LLM_POLICY
    files: [wms/src/ai/pii_policy.rs]
    default: "a conservative default policy when unset (see source)"
    effect: "policy controlling whether/how PII is masked before text is sent to a generative LLM call"
    feature: AI-live
    required: false
  - name: ECK_LLM_MODE
    files: [wms/src/ai/pii_policy.rs]
    default: "a fixed default mode when unset (see source)"
    effect: "overall LLM masking mode switch consulted alongside the PII policy vars above"
    feature: AI-live
    required: false
  - name: ECK_TENANT_BRAND
    files: [wms/src/ai/branding.rs]
    default: "none — no tenant brand name is injected into AI prompts/summaries"
    effect: "this deployment's brand/company name, used to tune AI prompt context (e.g. distinguishing the tenant's own name from a customer's in summaries)"
    feature: AI-live
    required: false
  - name: ECK_TENANT_VERTICAL
    files: [wms/src/ai/branding.rs]
    default: "none — no industry-vertical hint is injected into AI prompts"
    effect: "this deployment's business vertical (e.g. repair shop, warehouse), used to tune AI prompt context"
    feature: AI-live
    required: false
  - name: XELIXIR_KB_URL
    files: [wms/src/ai/repair_distiller.rs]
    default: "none — resolved-ticket lessons are not pushed to any external knowledge base"
    effect: "URL of the xelixir knowledge-base ingest endpoint the repair distiller posts confirmed-fixed lessons to"
    feature: AI-live
    required: false
  - name: XELIXIR_KB_TOKEN
    files: [wms/src/ai/repair_distiller.rs]
    default: "none — required alongside XELIXIR_KB_URL for the distiller push to authenticate"
    effect: "bearer token for the xelixir knowledge-base ingest endpoint"
    feature: AI-live
    required: false
  - name: ECK_SUMMARY_WORKER
    files: [wms/src/main.rs]
    default: "1 (this node produces its own summaries)"
    effect: "set to 0 to make this node a summary consumer only (mesh-syncs summaries produced elsewhere)"
    feature: AI-live
    required: false
  - name: ECK_EXTRACT_SWEEP_SECS
    files: [wms/src/services/scheduler.rs, "referenced in a wms/src/main.rs comment"]
    default: "none/0 — the attachment-extraction (OCR/text-layer) sweep is disabled"
    effect: "interval in seconds for the attachment-extraction sweep; enable on exactly one blob-holding node"
    feature: AI-live
    required: false
  - name: ECK_EXTRACT_SWEEP_BATCH
    files: [wms/src/services/scheduler.rs]
    default: "a fixed batch size when unset (see source)"
    effect: "rows-per-pass batch size for the attachment-extraction sweep"
    feature: AI-live
    required: false
  - name: ECK_CLASSIFY_SWEEP_SECS
    files: [wms/src/services/scheduler.rs, "referenced in a wms/src/main.rs comment"]
    default: "none/0 — the attachment-classification sweep is disabled"
    effect: "interval in seconds for the per-PDF doc_class classification sweep; metered and budget-halt gated; enable on exactly one node"
    feature: AI-live
    required: false
  - name: ECK_CLASSIFY_SWEEP_BATCH
    files: [wms/src/services/scheduler.rs]
    default: "a fixed batch size when unset (see source)"
    effect: "rows-per-pass batch size for the attachment-classification sweep"
    feature: AI-live
    required: false
  - name: ECK_GEO_SWEEP_SECS
    files: [wms/src/main.rs]
    default: "none/0 — the background GeoSweep loop (customer-db -> attachment-ocr -> vorwahl -> grounding) does not start"
    effect: "interval in seconds for the background geo-resolution sweep; only the AI address-discovery stage inside it further needs GEMINI_GENERATION_MODEL"
    feature: geo
    required: false
```

---

## 5. AI-batch (Vertex batch prediction — ~50% cheaper summarization)

```yaml
vars:
  - name: ECK_SUMMARY_BATCH
    files: [wms/src/ai/summarization_batch.rs]
    default: "false/unset — summarization uses the live (synchronous, per-call) path, not Vertex batch prediction"
    effect: "master switch enabling batch-mode summarization via Vertex batch prediction instead of live per-ticket calls"
    feature: AI-batch
    required: false
  - name: ECK_SUMMARY_BATCH_MIN
    files: [wms/src/ai/summarization_batch.rs]
    default: "a fixed minimum batch size when unset (see source)"
    effect: "minimum number of queued tickets before a batch job is submitted"
    feature: AI-batch
    required: false
  - name: ECK_SUMMARY_BATCH_MAX
    files: [wms/src/ai/summarization_batch.rs]
    default: "a fixed maximum batch size when unset (see source)"
    effect: "maximum number of tickets bundled into one Vertex batch-prediction job"
    feature: AI-batch
    required: false
  - name: ECK_AI_BATCH_BUCKET
    files: [wms/src/ai/summarization_batch.rs]
    default: "none — required for ECK_SUMMARY_BATCH=true to actually submit a job; batch submission fails/no-ops without a GCS bucket to stage input/output"
    effect: "GCS bucket URI used to stage Vertex batch-prediction input/output files"
    feature: AI-batch
    required: false
  - name: ECK_AI_BATCH_MAX_AGE_HOURS
    files: [wms/src/ai/summarization_batch.rs]
    default: "a fixed max-age default when unset (see source)"
    effect: "maximum hours a ticket can sit queued before it is force-flushed into a batch job even if ECK_SUMMARY_BATCH_MIN hasn't been reached"
    feature: AI-batch
    required: false
```

---

## 6. i18n

```yaml
vars:
  - name: I18N_LANGS
    files: [wms/src/handlers/i18n.rs]
    default: "a fixed built-in language list when unset (see source; English is always available)"
    effect: "comma-separated list of UI languages this node serves"
    feature: i18n
    required: false
  - name: I18N_LANG_DAILY_CAP
    files: [wms/src/handlers/i18n.rs]
    default: "a fixed default daily cap when unset (see source)"
    effect: "daily cap on machine-translation calls when an operator adds a new customer language at runtime"
    feature: i18n
    required: false
  - name: GEMINI_TRANSLATE_MODEL
    files: [wms/src/ai/translation.rs, wms/src/handlers/i18n.rs]
    default: "a fixed default Gemini flash-lite model id when unset (see source)"
    effect: "Gemini model id used for both label-dictionary translation and PPRL-masked in-context translation"
    feature: i18n
    required: false
  - name: TRANSLATE_DAILY_CAP
    files: [wms/src/ai/translation.rs]
    default: "a fixed default daily cap when unset (see source)"
    effect: "daily cap on AI translation calls for in-content (ticket/summary) translation"
    feature: i18n
    required: false
```

---

## 7. geo

```yaml
vars:
  - name: OPENCELLID_API_KEY
    files: [wms/src/services/cell_resolver.rs]
    default: "none — cell-tower geocoding for PDA trips is skipped, trips fall back to whatever GPS/network location was captured"
    effect: "API key for the OpenCelliD lookup used by the PDA trip cell-tower geocoding worker"
    feature: geo
    required: false
  - name: TRIP_RAW_RETENTION_DAYS
    files: [wms/src/handlers/trips.rs]
    default: "a fixed retention window when unset (see source)"
    effect: "days raw (pre-GoBD-seal) trip point data is retained before pruning"
    feature: geo
    required: false
  - name: TRIP_ROAD_FACTOR
    files: [wms/src/handlers/trips.rs]
    default: "a fixed multiplier when unset (see source)"
    effect: "straight-line-to-road-distance correction factor applied to Fahrtenbuch distance calculations"
    feature: geo
    required: false
```

---

## 8. PDA

```yaml
vars:
  - name: ENC_KEY
    files: [wms/src/handlers/pda.rs]
    default: "none — PDA payload field-level encryption is skipped where this key would be used"
    effect: "symmetric key for encrypting sensitive PDA payload fields"
    feature: PDA
    required: false
  - name: QR_PREFIXES
    files: [wms/src/handlers/pda.rs]
    default: "a fixed built-in prefix set when unset (see source)"
    effect: "recognized QR/barcode payload prefixes the PDA scan handler dispatches on"
    feature: PDA
    required: false
  - name: REPAIR_ORDER_PREFIX
    files: [wms/src/handlers/pda.rs]
    default: "a fixed default prefix string when unset (see source)"
    effect: "prefix used to recognize a scanned repair-order barcode"
    feature: PDA
    required: false
  - name: QR_TENANT_SUFFIX
    files: [wms/src/handlers/pda.rs]
    default: "none — no tenant-disambiguation suffix is appended/matched on generated QR payloads"
    effect: "tenant-scoping suffix appended to generated QR codes so multi-tenant deployments don't cross-scan"
    feature: PDA
    required: false
```

---

## 9. ops

```yaml
vars:
  - name: XELIXIR_SERVICE_TOKEN
    files: [wms/src/middleware/service_token.rs, wms/src/mcp/mod.rs]
    default: "none — every /X/internal/* and /X/ops/* request gets 503 (\"XELIXIR_SERVICE_TOKEN is not configured on this node\"); POST /mcp also loses its fallback Master-tier credential (see ECK_MCP_MASTER_TOKEN)"
    effect: "shared bearer token gating the /X/internal/* and /X/ops/* endpoint families (sibling-service + xelixir autonomous-ops calls), and a fallback Master-tier credential for /mcp"
    feature: ops
    required: false
  - name: XELIXIR_ADMIN_PUBKEYS
    files: [wms/src/handlers/xelixir.rs]
    default: "none — POST /X/self/start and /X/self/stop refuse every signed envelope (no allow-listed signer)"
    effect: "allow-list of Ed25519 public keys permitted to sign an inter-node /X/self/* activation envelope"
    feature: ops
    required: false
  - name: ECK_FLEET_ROOT_PUBKEY
    files: [wms/src/handlers/xelixir.rs, core/src/xelixir/envelope.rs]
    default: "none — the fleet-root signature-chain verification path for /X/self/* is unavailable; only a direct XELIXIR_ADMIN_PUBKEYS match works"
    effect: "root public key allowing an envelope signer to be verified via certificate chain instead of a direct allow-list match"
    feature: ops
    required: false
  - name: ECK_FLEET_ADMIN_PRIVKEY
    files: [wms/src/services/agent_manager.rs]
    default: "none — this node cannot sign fleet-admin ops commands"
    effect: "this node's fleet-admin private key for signing cert-based ops dispatches"
    feature: ops
    required: false
  - name: ECK_FLEET_ADMIN_PUBKEY
    files: [wms/src/services/agent_manager.rs]
    default: "none — paired with ECK_FLEET_ADMIN_PRIVKEY"
    effect: "this node's fleet-admin public key"
    feature: ops
    required: false
  - name: ECK_FLEET_ADMIN_CERT
    files: [wms/src/services/agent_manager.rs]
    default: "none — no fleet-admin CA certificate is presented"
    effect: "path to this node's fleet-admin CA-signed certificate, used in cert-signed ops flows"
    feature: ops
    required: false
  - name: LICENSE_TOKEN
    files: [wms/src/services/agent_manager.rs]
    default: "none — the local xelixir agent's boot auto-start is skipped and on-demand spawn_agent() always returns an error"
    effect: "presence-gates whether the local xelixir desktop agent can be spawned at all (auto-start on boot and on-demand). NOTE: distinct from ECK_LICENSE_TOKEN (managed-seam section) despite the similar name — do not conflate them."
    feature: ops
    required: false
  - name: XELTH_CLAIM_URL
    files: [wms/src/services/agent_manager.rs]
    default: "\"\" in the open-core build (feature inert until pointed at an authority; commercial builds ship a vendor endpoint default)"
    effect: "URL this node POSTs a license-claim request to when spawning the local desktop agent"
    feature: ops
    required: false
  - name: XELTH_WS_URL
    files: [wms/src/services/agent_manager.rs]
    default: "\"\" in the open-core build (feature inert until pointed at an authority; commercial builds ship a vendor endpoint default)"
    effect: "WebSocket URL the local desktop agent dials out to for its control-plane connection (use a direct-port form — a reverse-proxied path that 301-redirects breaks raw WS clients, which don't follow redirects)"
    feature: ops
    required: false
  - name: XELTH_SESSION_BASE
    files: [wms/src/services/agent_manager.rs]
    default: "a fixed default base URL when unset (see source)"
    effect: "base URL used to construct a human-followable session link for an active xelixir agent session"
    feature: ops
    required: false
  - name: XELIXIR_AGENT_USER
    files: [wms/src/services/agent_manager.rs]
    default: "none — the agent runs in-process as the WMS process uid (historical/headless behavior)"
    effect: "OS user account the xelixir desktop agent should run as, instead of inheriting the WMS server's own uid"
    feature: ops
    required: false
  - name: INSTANCE_NAME
    files: [wms/src/services/agent_manager.rs]
    default: "an auto-generated name when unset (see source)"
    effect: "human-readable display name for this node's xelixir agent registration"
    feature: ops
    required: false
  - name: WAYLAND_DISPLAY
    files: [wms/src/services/agent_manager.rs]
    default: "\"wayland-0\""
    effect: "Wayland display socket name the xelixir desktop agent attaches to for screen capture/input on Linux"
    feature: ops
    required: false
  - name: WMS_LOCAL_BASE
    files: [wms/src/handlers/xelixir.rs]
    default: "none — falls back to BASE_URL / auto-detected local URL"
    effect: "override base URL used specifically for local xelixir dispatch loopback calls"
    feature: ops
    required: false
  - name: ECK_OPS_FILE_PREFIXES
    files: [wms/src/handlers/ops.rs]
    default: "a conservative built-in allow-list when unset (see source)"
    effect: "comma-separated allow-listed filesystem path prefixes GET /X/ops/file_read and POST /X/ops/file_write are permitted to touch"
    feature: ops
    required: false
  - name: WMS_PROJECT_ROOT
    files: [wms/src/handlers/ops.rs]
    default: "current working directory when unset"
    effect: "repository root path used by the git_pull/cargo_build/deploy ops verbs"
    feature: ops
    required: false
  - name: ECK_DEPLOY_USER
    files: [wms/src/handlers/ops.rs]
    default: "current process user when unset"
    effect: "OS user the deploy/restart_service ops verbs run their subprocess as"
    feature: ops
    required: false
  - name: HOME
    files: [wms/src/handlers/ops.rs]
    default: "OS-provided; not set by this app"
    effect: "read by some ops-verb subprocess invocations that need a HOME env for the child process"
    feature: ops
    required: false
  - name: PATH
    files: [wms/src/handlers/ops.rs]
    default: "OS-provided; not set by this app"
    effect: "inherited/read for locating binaries (git, cargo, package manager) invoked by ops verbs"
    feature: ops
    required: false
  - name: RATE_LIMIT_AUTH_PER_MIN
    files: [core/src/ratelimit.rs]
    default: "a fixed default requests-per-minute cap when unset (see source)"
    effect: "per-IP rate limit applied to authentication endpoints"
    feature: ops
    required: false
```

---

## 10. scraper-proxy

```yaml
vars:
  - name: SCRAPER_PORT
    files: [wms/src/handlers/scraper_proxy.rs]
    default: "\"38211\""
    effect: "local port of the Node.js scraper process; /S/* proxies to http://127.0.0.1:$SCRAPER_PORT"
    feature: scraper-proxy
    required: false
  - name: SCRAPER_DIR
    files: [wms/src/handlers/scraper_proxy.rs]
    default: "tries ../eckwms/scraper (legacy sibling project) first, then ./scraper relative to CWD"
    effect: "filesystem directory containing the Node.js scraper's server.js, spawned by POST /api/scraper/start"
    feature: scraper-proxy
    required: false
  - name: ENABLE_SCRAPERS
    files: [wms/src/main.rs]
    default: "false/unset — scraper-related background wiring does not activate at boot"
    effect: "master switch enabling scraper integration wiring at node startup"
    feature: scraper-proxy
    required: false
  - name: ECK_ZOHO_FULL_REVERSE
    files: [wms/src/handlers/support.rs]
    default: "false/unset — Zoho ticket ingest processes in normal (forward) order"
    effect: "reverses processing order for a Zoho Desk full-reverse backfill run"
    feature: scraper-proxy
    required: false
  - name: ECK_ZOHO_SKIP_BINARIES
    files: [wms/src/handlers/support.rs]
    default: "false/unset — binary attachment payloads are processed normally during Zoho import"
    effect: "skips binary attachment payload processing during Zoho Desk ticket import (faster metadata-only backfill)"
    feature: scraper-proxy
    required: false
  - name: OCU_API_URL
    files: [wms/src/services/ocu.rs]
    default: "none — the OCU API delivery-provider integration is disabled (env-gated seam)"
    effect: "base URL of the OCU (open-source OPAL-facade) delivery API this node's Rust-native client calls"
    feature: scraper-proxy
    required: false
  - name: OCU_API_KEY
    files: [wms/src/services/ocu.rs]
    default: "none — required alongside OCU_API_URL to authenticate"
    effect: "API key for the OCU delivery API"
    feature: scraper-proxy
    required: false
```

---

## 11. other

```yaml
vars:
  - name: JWT_SECRET
    files: [core/src/auth.rs]
    default: "none — a random secret is generated at boot and NOT persisted, so every restart invalidates all previously issued JWTs (all users are logged out)"
    effect: "HMAC signing secret for user/admin JWTs"
    feature: other
    required: false
  - name: SURREAL_DB_PATH
    files: [wms/src/main.rs, migrator/src/main.rs, "relay/src/main.rs (own default), pos/src/main.rs (commercial, own default)"]
    default: "wms/migrator default \"data/wms.db\"; relay defaults to \"data/relay.db\"; commercial standalone pos binary uses its own default — see pos/src/main.rs"
    effect: "filesystem path to the embedded SurrealKV database file for the reading binary"
    feature: other
    required: false
  - name: SURREAL_USERS_DB_PATH
    files: [wms/src/main.rs]
    default: "a fixed default path when unset (see source)"
    effect: "separate SurrealKV database path used only for the node-local setup-admin bootstrap user row"
    feature: other
    required: false
  - name: SURREAL_REMOTE_URL
    files: [core/src/db.rs]
    default: "none — embedded/local SurrealKV is used instead of a remote SurrealDB server"
    effect: "connects to a remote SurrealDB instance instead of the default embedded SurrealKV file"
    feature: other
    required: false
  - name: SURREAL_NS
    files: [core/src/db.rs]
    default: "a fixed default namespace string when unset (see source)"
    effect: "SurrealDB namespace used for both embedded and remote connections"
    feature: other
    required: false
  - name: SURREAL_NS_USER
    files: [core/src/db.rs]
    default: "none — only relevant with SURREAL_REMOTE_URL"
    effect: "namespace-scoped auth username for a remote SurrealDB connection"
    feature: other
    required: false
  - name: SURREAL_NS_PASS
    files: [core/src/db.rs]
    default: "none — only relevant with SURREAL_REMOTE_URL"
    effect: "namespace-scoped auth password for a remote SurrealDB connection"
    feature: other
    required: false
  - name: SURREAL_ROOT_USER
    files: [core/src/db.rs]
    default: "none — only relevant with SURREAL_REMOTE_URL"
    effect: "root auth username for a remote SurrealDB connection"
    feature: other
    required: false
  - name: SURREAL_USER
    files: [core/src/db.rs]
    default: "none — alternate/legacy var name checked alongside SURREAL_ROOT_USER"
    effect: "same purpose as SURREAL_ROOT_USER, alternate name"
    feature: other
    required: false
  - name: SURREAL_ROOT_PASS
    files: [core/src/db.rs]
    default: "none — only relevant with SURREAL_REMOTE_URL"
    effect: "root auth password for a remote SurrealDB connection"
    feature: other
    required: false
  - name: SURREAL_PASS
    files: [core/src/db.rs]
    default: "none — alternate/legacy var name checked alongside SURREAL_ROOT_PASS"
    effect: "same purpose as SURREAL_ROOT_PASS, alternate name"
    feature: other
    required: false
  - name: PORT
    files: [wms/src/main.rs, "relay/src/main.rs (own default), pos/src/main.rs (commercial, own default)"]
    default: "wms defaults to 3210; relay defaults to 3200; commercial standalone pos binary uses its own default — see pos/src/main.rs"
    effect: "TCP port the reading binary's HTTP server binds to (0.0.0.0)"
    feature: other
    required: false
  - name: WMS_LOG_DIR
    files: [wms/src/main.rs]
    default: "a fixed default log directory when unset (see source)"
    effect: "directory the node writes its own log files to"
    feature: other
    required: false
  - name: HEDERA_ACCOUNT_ID
    files: [core/src/sync/hedera.rs, core/src/anchor.rs]
    default: "none — Hedera anchoring of the audit chain is disabled; POST /api/audit/anchor becomes a no-op/error"
    effect: "Hedera account id used to submit audit-chain anchor transactions"
    feature: other
    required: false
  - name: HEDERA_PRIVATE_KEY
    files: [core/src/sync/hedera.rs, core/src/anchor.rs]
    default: "none — required alongside HEDERA_ACCOUNT_ID"
    effect: "Hedera account private key for signing anchor transactions"
    feature: other
    required: false
  - name: HEDERA_KEY
    files: [core/src/sync/hedera.rs]
    default: "none — alternate/legacy var name for the Hedera key"
    effect: "same purpose as HEDERA_PRIVATE_KEY, alternate name checked in some call sites"
    feature: other
    required: false
  - name: HEDERA_TOPIC_ID
    files: [core/src/sync/hedera.rs]
    default: "none — a new Hedera Consensus Service topic is not pre-selected; anchoring may create/require one"
    effect: "Hedera Consensus Service topic id audit-anchor messages are submitted to"
    feature: other
    required: false
  - name: HEDERA_NETWORK
    files: [core/src/sync/hedera.rs]
    default: "a fixed default network (e.g. testnet) when unset (see source)"
    effect: "selects Hedera network (mainnet/testnet) for anchor transactions"
    feature: other
    required: false
  - name: HEDERA_MIRROR_URL
    files: [core/src/sync/hedera.rs]
    default: "a fixed default mirror-node URL when unset (see source)"
    effect: "Hedera mirror-node REST URL used to verify/read back anchored messages"
    feature: other
    required: false
  - name: HEDERA_NODE_ACCOUNT
    files: [core/src/sync/hedera.rs]
    default: "none — SDK default node selection is used"
    effect: "pins a specific Hedera consensus node account for submitting transactions"
    feature: other
    required: false
  - name: HEDERA_NODE_URL
    files: [core/src/sync/hedera.rs]
    default: "none — SDK default node URL is used"
    effect: "pins a specific Hedera consensus node network address"
    feature: other
    required: false
  - name: COMPLIANCE_PORT
    files: [compliance/src/main.rs]
    default: "a fixed default port when unset (see source)"
    effect: "TCP port the standalone compliance-tool binary binds to, when it exposes an HTTP surface"
    feature: other
    required: false
  - name: LEGACY_DATABASE_URL
    files: [migrator/src/main.rs]
    default: "none — required to run the legacy Postgres migration; migrator errors without it"
    effect: "connection URL of the legacy PostgreSQL database the migrator reads from"
    feature: other
    required: false
  - name: LEGACY_FILESTORE_BASE
    files: [migrator/src/main.rs]
    default: "none — legacy file-blob migration is skipped"
    effect: "filesystem base path of the legacy filestore the migrator copies blobs from"
    feature: other
    required: false
  - name: ECK_QR_HOSTS
    files: [wms/src/handlers/print.rs]
    default: "falls back to BASE_URL / auto-detected host when unset"
    effect: "comma-separated list of hostnames embedded in generated warehouse-label QR codes (lets a label resolve correctly across multiple reachable hostnames for the same node)"
    feature: other
    required: false
  - name: ECK_COMPANY_NAME
    files: [wms/src/ai/branding.rs, wms/src/handlers/print.rs]
    default: "none — no company name is embedded in labels/AI prompt branding context"
    effect: "this deployment's company/business name, used in generated labels and AI prompt context"
    feature: other
    required: false
  - name: ECK_COMPANY_LOCATION
    files: [wms/src/ai/branding.rs]
    default: "none — no company location is injected into AI prompt branding context"
    effect: "this deployment's business location string, used to tune AI prompt context"
    feature: other
    required: false
  - name: ECK_BRAND_STOPLIST
    files: [core/src/utils/anonymizer.rs]
    default: "a built-in default stoplist when unset (see source)"
    effect: "comma-separated list of brand/company words the PII masker must NEVER treat as a person name (protects e.g. product/company names from being masked as PII)"
    feature: other
    required: false
  - name: ECK_OWN_ADDRESS_MARKERS
    files: [core/src/utils/anonymizer.rs]
    default: "none — no deployment-specific address markers are excluded from masking beyond the built-in defaults"
    effect: "comma-separated markers (e.g. this company's own street/city) the PII masker excludes from address masking, since they identify the business itself, not a customer"
    feature: other
    required: false
  - name: ODOO_URL
    files: [wms/src/services/odoo.rs, wms/src/handlers/odoo.rs]
    default: "none — Odoo integration is disabled"
    effect: "base URL of the customer's Odoo ERP instance"
    feature: other
    required: false
  - name: ODOO_DB
    files: [wms/src/services/odoo.rs]
    default: "none — required alongside ODOO_URL"
    effect: "Odoo database name to connect to"
    feature: other
    required: false
  - name: ODOO_LOGIN
    files: [wms/src/services/odoo.rs]
    default: "none — required alongside ODOO_URL"
    effect: "Odoo API login/username"
    feature: other
    required: false
  - name: ODOO_API_KEY
    files: [wms/src/services/odoo.rs]
    default: "none — required alongside ODOO_URL"
    effect: "Odoo API key/password"
    feature: other
    required: false
  - name: ODOO_WRITE_ENABLED
    files: [wms/src/handlers/odoo.rs]
    default: "false/unset — /api/odoo/project/set-onhand and /project/run are read/dry-run only, no writes reach the customer's production Odoo"
    effect: "must be explicitly truthy to allow this node to WRITE back into the customer's Odoo ERP (on top of admin-JWT gating already on that route)"
    feature: other
    required: false
```

---

## 12. POS (open-source gate + commercial edition)

`POS_ENABLED` itself is read from the **open-source** `wms` crate (it decides whether
`wms/src/main.rs` merges the commercial POS router at all); everything else in this
section lives in the commercial-only `pos` and `shim` crates, listed per the audit
brief rather than omitted.

```yaml
vars:
  - name: POS_ENABLED
    files: [wms/src/main.rs]
    default: "false — /K/* is unavailable (falls through to the WMS SPA fallback)"
    effect: "gates whether the open-source node merges the commercial POS router at /K/* at boot. Interim env flag pending a proper eck_core::licensing scope check. Has no effect at all if the `pos-module` Cargo feature was not compiled in — the binary logs a warning and /K/* stays unavailable either way"
    feature: POS
    required: false
    edition: open-source
```

### Commercial edition only — `pos` crate

Several vars the `pos` crate reads are the SAME names already documented above with
their own defaults noted inline (`GEMINI_GENERATION_MODEL`, `GEMINI_API_KEY`,
`SYNC_SECRET`, `INSTANCE_ID`, `MESH_NODE_ROLE`, `RELAY_URL`, `RELAY_URLS`,
`HEDERA_ACCOUNT_ID`/`HEDERA_PRIVATE_KEY`/`HEDERA_TOPIC_ID`/`HEDERA_NETWORK`,
`SURREAL_DB_PATH`, `PORT`) — not repeated here. New commercial-only vars:

```yaml
vars:
  - name: TSE_PROVIDER
    files: [pos/src/services/tse.rs]
    default: "none — no real fiscal TSE (Technical Security Equipment) provider is wired; the seam exists but there is no legally compliant TSE integration yet (known legal blocker for German fiscal use)"
    effect: "selects which TSE hardware/cloud provider backs the fiscal signing seam"
    feature: POS
    required: false
    edition: commercial
  - name: PRINTER_TRANSPORT
    files: [pos/src/lib.rs]
    default: "a fixed default transport when unset (see source)"
    effect: "selects receipt-printer transport (e.g. TCP vs USB)"
    feature: POS
    required: false
    edition: commercial
  - name: PRINTER_TCP_IP
    files: [pos/src/lib.rs]
    default: "none — only relevant when PRINTER_TRANSPORT selects TCP"
    effect: "IP address of a network receipt printer"
    feature: POS
    required: false
    edition: commercial
  - name: PRINTER_TCP_PORT
    files: [pos/src/lib.rs]
    default: "a fixed default port when unset (see source)"
    effect: "TCP port of a network receipt printer"
    feature: POS
    required: false
    edition: commercial
  - name: PRINTER_USB_VID
    files: [pos/src/lib.rs]
    default: "none — only relevant when PRINTER_TRANSPORT selects USB"
    effect: "USB vendor id used to locate the receipt printer"
    feature: POS
    required: false
    edition: commercial
  - name: PRINTER_USB_PID
    files: [pos/src/lib.rs]
    default: "none — only relevant when PRINTER_TRANSPORT selects USB"
    effect: "USB product id used to locate the receipt printer"
    feature: POS
    required: false
    edition: commercial
```

### Commercial edition only — `shim` crate (desktop MCP-connector bridge)

`shim` has no HTTP server of its own (see `45-http-surface.md`); it is a desktop
process that bridges a local MCP client to a node over the relay or direct LAN.

```yaml
vars:
  - name: ECK_SHIM_MODE
    files: [shim/src/main.rs]
    default: "a fixed default mode when unset (see source)"
    effect: "selects the shim's transport mode: relay-carried vs direct-to-node"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_URL
    files: [shim/src/main.rs]
    default: "none — required in direct mode; without it the shim cannot dial a node directly"
    effect: "target node URL for direct-mode MCP bridging"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_BEARER
    files: [shim/src/main.rs]
    default: "none — required in direct mode to authenticate against the node's /mcp bearer gate"
    effect: "bearer token the shim presents to a node's POST /mcp in direct mode"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_TOKEN
    files: [shim/src/main.rs]
    default: "none — the shim distribution bundle's own admission token is unset"
    effect: "capability token minted into a distributed shim bundle (see /api/admin/mcp-connector), identifying this shim instance"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_KEY_FILE
    files: [shim/src/main.rs]
    default: "none — required for cert-signed relay-carried mode"
    effect: "path to this shim's private key file, used to sign SubscriptionCert-carried requests"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_CERT_FILE
    files: [shim/src/main.rs]
    default: "none — required for cert-signed relay-carried mode"
    effect: "path to this shim's SubscriptionCert file"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_RELAY
    files: [shim/src/main.rs]
    default: "none — required in relay-carried mode; without it the shim has no relay to dial"
    effect: "relay base URL the shim uses for the relay-carried /E/c/* client-MCP channel"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_TARGET
    files: [shim/src/main.rs]
    default: "none — required in relay-carried mode"
    effect: "target node's instance UUID the shim dispatches relay-carried MCP requests to"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_ACK_WINDOW_SECS
    files: [shim/src/main.rs]
    default: "a fixed default window when unset (see source)"
    effect: "how long the shim waits for a task's ack/result before giving up polling /E/c/result"
    feature: MCP
    required: false
    edition: commercial
  - name: ECK_SHIM_NOPICKUP_SECS
    files: [shim/src/main.rs]
    default: "a fixed default timeout when unset (see source)"
    effect: "how long the shim waits for the target node to even start polling (pick up) a dispatched task before treating it as unreachable"
    feature: MCP
    required: false
    edition: commercial
```

---

## Minimal env recipes

```yaml
recipes:
  standalone-no-AI:
    description: "single node, no AI features at all — pure WMS/mesh/PDA functionality"
    set:
      - "SYNC_SECRET=<random-32-byte-string>   # required: PII masker panics on first ticket import without it"
      - "JWT_SECRET=<random-secret>            # optional but avoids logging out all users on every restart"
    leave_unset: ["GEMINI_API_KEY", "ECK_AI_MODE", "ECK_VERTEX_MINT_URL", "ECK_VERTEX_USAGE_URL", "ECK_LICENSE_TOKEN", "ECK_VERTEX_BEARER", "ECK_SUB_ROOT_PUBKEY"]
    result: "AI-gated routes (summarize, enrich-csv, ask_brain, translation, GeoSweep AI stage) either no-op or return an AI-disabled error; everything else works"

  standalone-with-own-gemini-key:
    description: "single node, self-funded Gemini Studio key, no commercial authority involved"
    set:
      - "SYNC_SECRET=<random-32-byte-string>"
      - "GEMINI_API_KEY=<your Google AI Studio key>"
      - "GEMINI_GENERATION_MODEL=<model id, e.g. gemini-2.5-flash>"
      - "GEMINI_EMBEDDING_MODEL=<model id>"
      - "GEMINI_SUMMARY_MODEL=<model id>"
    leave_unset: ["ECK_AI_MODE", "ECK_VERTEX_MINT_URL", "ECK_VERTEX_USAGE_URL", "ECK_LICENSE_TOKEN", "ECK_VERTEX_BEARER", "ECK_SUB_ROOT_PUBKEY"]
    result: "AI features run against Google's Studio API directly, billed to your own key; no commercial authority is contacted"

  MCP-surface:
    description: "expose this node's business-graph MCP tools to an agent (Claude Desktop, etc.)"
    set:
      - "ECK_MCP_MASTER_TOKEN=<random bearer>   # or rely on XELIXIR_SERVICE_TOKEN as a fallback"
      - "ECK_MCP_AGENT_TOKEN=<random bearer>    # optional: enables a masked-PII Agent tier alongside Master"
    result: "POST /mcp is reachable with the bearer(s) you set; admin can also download a ready-to-run connector bundle via GET /api/admin/mcp-connector (admin-JWT)"

  self-hosted-mesh-2-plus-nodes:
    description: "two or more of your own nodes sync data with each other over a relay"
    set:
      - "SYNC_SECRET=<same value on every node>   # required: this is the mesh-auth shared secret AND the PII pepper — must match across the mesh"
      - "RELAY_URL=<your relay's base URL>        # or RELAY_URLS for multiple relays; self-hosted or the public https://9eck.com default"
      - "BASE_URL=<this node's externally reachable URL>   # per node, for heartbeat + pairing"
      - "MESH_NODE_ROLE=full                      # set to cache on a public-facing cache node"
    result: "nodes heartbeat to the relay every 5 min, discover each other, and sync via /api/mesh/* (mesh-secret gated) with relay fallback via /E/m/* when direct P2P fails"

  self-hosted-relay:
    description: "run your own relay instead of depending on the public one"
    set:
      - "PORT=3200                       # relay's own default; only set to override"
      - "SURREAL_DB_PATH=data/relay.db   # relay's own default; only set to override"
      - "RELAY_ADMIN_TOKEN=<random bearer>   # optional: protects GET /E/registry; unset = that one route stays 403-closed"
      - "RELAY_PAYLOAD_MODE=open         # default; set to disabled for a discovery-only public board, or paid to require a paid party"
    leave_unset: ["ECK_SUB_ROOT_PUBKEY", "ECK_LICENSE_PUBKEY"]
    result: "a bare relay with mesh registration/push/pull, xelixir C2 relay, and mesh-relay-fallback all working; the paid client-MCP channel (/E/c/*) stays permanently disabled (503) since no subscription root is configured — that channel is inherently a managed-mode feature"
```

---

## Judgment calls

- Several vars are read by more than one binary with **different literal defaults**
  (`RELAY_URL`, `SURREAL_DB_PATH`, `PORT`). Rather than pick one and risk misleading
  an agent configuring a specific binary, each is listed once with all known defaults
  spelled out in `effect`/`default` — cross-check which binary you're configuring.
- `ECK_LICENSE_TOKEN` (managed-seam) and `LICENSE_TOKEN` (ops) are two DIFFERENT
  variables with confusingly similar names, used for unrelated purposes (managed-AI
  credential + paid-tier flag, vs. local xelixir desktop-agent spawn licensing). Both
  are documented with an explicit cross-reference note rather than merged, since
  merging them would be a factual error.
- `required: true` was reserved for `SYNC_SECRET` only. Several other vars
  (`GEMINI_*_MODEL`, `XELIXIR_SERVICE_TOKEN`, `ECK_AI_BATCH_BUCKET`, TSE-related pos
  vars) are hard-required in the sense that a specific *opted-in* feature panics or
  hard-errors without them, but the base process/deployment does not need them —
  that conditionality is written into each `effect` field instead of the boolean, to
  avoid a misleading blanket `true`.
- Exact literal defaults marked "a fixed default … when unset (see source)" are cases
  where this audit confirmed a `.unwrap_or_else`/constant default exists in the code
  but did not transcribe the precise literal value into this document — read the cited
  file if the exact number/string matters for your deployment.
- `POS_ENABLED` is filed under the `POS` feature bucket but is technically read by the
  **open-source** `wms` crate, not by the commercial `pos` crate itself — flagged
  explicitly rather than silently placed in the commercial subsection, since it is one
  of the few vars a fully open-source build still needs to reckon with (compiling
  `pos-module` in or out is a separate, build-time decision).


  # added 2026-07-30 (post-audit): deployment-data seams extracted from code
  - name: ECK_INTERNAL_SENDER_DOMAINS
    files: [wms/src/services/support.rs]
    default: "\"\" (no internal-domain match)"
    effect: "comma-separated sender-domain suffixes treated as the operating company own support agents (direction detection at ticket ingest)"
    feature: scraper-proxy
    required: false
  - name: ECK_MODEL_CF_KEYS
    files: [wms/src/services/support.rs]
    default: "device model,device_model,model"
    effect: "lowercased Zoho custom-field labels that carry the device model (deployment Zoho schema data)"
    feature: scraper-proxy
    required: false
