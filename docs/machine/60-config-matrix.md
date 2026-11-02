<!-- machine-first: generated from source audit 2026-07-29; audience=agents -->
## SCOPE

Every environment variable read anywhere in the open-source workspace crates
(`core`, `wms`, `relay`, `compliance`, `migrator`) via `std::env::var(...)` or
`std::env::var_os(...)` (directly or through a local wrapper that itself calls one of
those), plus `dotenvy` usage. A short **commercial edition** subsection at the end
covers `pos` (ecKasse register) and `shim` (desktop MCP-connector bridge) — neither
crate ships in the open-source workspace; their vars/routes are listed, not omitted,
per the audit brief.

This matrix also covers four subsystems added after the initial audit: the
zero-knowledge PII vault (`## 13`), SurrealKV cache/memtable tuning (`## 14`), the
OpenRouter / cheap-tier task-routing seam (`## 15`), and remote PII-span (NER)
masking (`## 16`). `ECK_LICENSE_PUBKEY`'s `files` list was expanded to include
`core/src/licensing.rs`, since that same key also verifies signed license claims and
plugin/POS license gates, not only relay node registration.

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
  - name: ECK_CHECKSUM_BOOT_WIPE
    files: [core/src/sync/engine.rs]
    default: "none — the checksum cache is not force-wiped at boot, the normal incremental sweep applies"
    effect: "forces a full entity-checksum cache wipe and rebuild at boot, for recovering a stuck/inconsistent checksum cache"
    feature: mesh
    required: false
  - name: ECK_ALLOW_PLAINTEXT_FILE_BYTES
    files: [wms/src/main.rs, wms/src/services/mesh_relay_poller.rs]
    default: "off unless \"1\" — plaintext file bytes are normally refused over the relay conduit on a keyless cache node"
    effect: "debug hatch allowing plaintext (unencrypted) file bytes to be sent/received over the relay conduit even when this node has no data-encryption key configured"
    feature: mesh
    required: false
  - name: ECK_REBUILD_STATUS_INDEXES_ON_BOOT
    files: [wms/src/main.rs]
    default: "none — the summary_status index is not force-rebuilt at boot"
    effect: "forces a summary_status index rebuild at boot; remedy for a stale-index planer bug that caused phantom re-summarizations"
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
    files: [core/src/licensing.rs, relay/src/handlers/register.rs, relay/src/handlers/client_mcp.rs]
    default: "none — relay does not verify a license signature on node registration; core-side license/plugin/POS license-gate verification is also unavailable"
    effect: "embedded/config issuer public key used to verify a signed license claim presented at /E/register and on the relay's client-MCP admission path, AND (core/src/licensing.rs) to verify signed license claims gating plugin and POS license checks node-side"
    feature: relay
    required: false
  - name: ECK_REVOCATION_FILE
    files: [core/src/licensing.rs]
    default: "none — no license CRL (revocation list) is consulted, no license id is treated as revoked"
    effect: "path to a license CRL drop-in file; listing a license id in this file is the kill switch that revokes it"
    feature: relay
    required: false
  - name: ECK_RELAY_CLIENT_MCP_AUTH
    files: [relay/src/handlers/client_mcp.rs]
    default: "none — the relay's client-MCP auth mode falls back to its built-in default selector"
    effect: "selects the auth mode the relay enforces on the client-MCP channel (/E/c/*)"
    feature: relay
    required: false
  - name: ECK_RELAY_ALLOW_KEY_ROTATION
    files: [relay/src/handlers/register.rs]
    default: "none — a registering node cannot rotate its identity key; re-registration with a new key is refused"
    effect: "permits a node to rotate its identity key at re-registration instead of being rejected for a key mismatch"
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
  - name: ECK_MCP_SESSION_DAYS
    files: [wms/src/mcp/session.rs]
    default: "a fixed default TTL in days when unset (see source)"
    effect: "TTL, in days, for a browser MCP session minted at POST /api/auth/mcp-session"
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
    files: [core/src/ai.rs]
    default: "https://xelth.com:3221/ai/embed"
    effect: "legacy authority embedding proxy; used ONLY when ECK_EMBED_VIA_AUTHORITY=1 (managed nodes embed directly on Vertex otherwise)"
    feature: AI-live
    required: false
  - name: ECK_EMBED_VIA_AUTHORITY
    files: [core/src/ai.rs]
    default: "unset (off) — managed nodes call Vertex gemini-embedding-2-preview:embedContent directly with the minted bearer"
    effect: "1/true/yes = rollback hatch: route managed-mode embeddings through the authority /ai/embed proxy (ECK_EMBED_URL) instead of Vertex"
    feature: AI-live
    required: false
  - name: ECK_VERTEX_EMBED_LOCATION
    files: [core/src/ai.rs]
    default: "us-central1 — the only region serving gemini-embedding-2-preview as of 2026-09-06; independent of the generation location returned by the mint"
    effect: "Vertex region for managed-mode embedContent calls"
    feature: AI-live
    required: false
  - name: ECK_VERTEX_EMBEDDING_MODEL
    files: [core/src/ai.rs]
    default: "none — GEMINI_EMBEDDING_MODEL is mapped: gemini-embedding-2* → gemini-embedding-2-preview (Vertex publisher id), anything else passes through"
    effect: "override the Vertex publisher model id used for managed-mode embeddings (e.g. drop the -preview suffix once GA); MUST stay in the gemini-embedding-2 vector space"
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
    files: [wms/src/ai/kb_client.rs, wms/src/ai/repair_distiller.rs, wms/src/ai/orchestrator.rs]
    default: "none — no KB recall for the brain (kb_brief tool answers 'not configured') and resolved-ticket lessons are not pushed anywhere"
    effect: "xelixir knowledge-base MCP endpoint: the brain's kb_brief tool + pre-injected KB digest for device tickets (read), and the repair distiller's kb_record push (write)"
    feature: AI-live
    required: false
  - name: XELIXIR_KB_TOKEN
    files: [wms/src/ai/kb_client.rs]
    default: "none — required alongside XELIXIR_KB_URL"
    effect: "bearer token for the xelixir knowledge-base MCP endpoint (kb_brief read + kb_record write)"
    feature: AI-live
    required: false
  - name: ECK_ADVICE_LEDGER_SYNC
    files: [wms/src/ai/advice_ledger.rs]
    default: "on; \"0\"/\"off\"/\"false\"/\"no\" disables the sync loop"
    effect: "enables the periodic remote-advice-ledger → xelixir KB sync loop (spawned from the repair-distiller worker); the loop also no-ops while the xelixir bridge is unconfigured (XELIXIR_KB_URL unset)"
    feature: AI-live
    required: false
  - name: ECK_ADVICE_LEDGER_SYNC_SECS
    files: [wms/src/ai/advice_ledger.rs]
    default: "21600 (6h); minimum 300s"
    effect: "sync period for the remote-advice-ledger → xelixir KB sync loop"
    feature: AI-live
    required: false
  - name: ECK_ADVICE_LEDGER_PATH
    files: [wms/src/ai/advice_ledger.rs]
    default: ".eck/advice/REMOTE_ADVICE_LEDGER.md"
    effect: "filesystem path to the hand-kept, append-only remote-advice ledger this sync loop parses (read-only — the loop never writes the file)"
    feature: AI-live
    required: false
  - name: ECK_SOP_DEDUP_COS
    files: [wms/src/ai/optimizer.rs]
    default: "0.92"
    effect: "cosine-similarity threshold above which a freshly distilled SOP is treated as a duplicate of an existing ai_sop row (duplicate_hits += 1, no insert); garbage/unset = default"
    feature: AI-live
    required: false
  - name: ECK_SUMMARY_WORKER
    files: [wms/src/main.rs]
    default: "1 (this node produces its own summaries)"
    effect: "set to 0 to make this node a summary consumer only (mesh-syncs summaries produced elsewhere)"
    feature: AI-live
    required: false
  - name: ECK_IMAGE_SUMMARY_WORKER
    files: [wms/src/ai/image_summary.rs, wms/src/ai/ocr_verify.rs]
    default: "disabled unless \"1\""
    effect: "enables the background image-summary (vision) worker on this node (same convention as ECK_SUMMARY_WORKER); off on a cache node regardless; also consulted by ocr_verify as one signal of whether this node is the AI author"
    feature: AI-live
    required: false
  - name: ECK_IMAGE_RESCORE
    files: [wms/src/ai/image_summary.rs]
    default: "false unless \"1\""
    effect: "opts the image-summary worker into a secondary backlog: rows already summarized but missing device_score/device_label fields (e.g. upgraded from an older build); only consulted once the normal backlog is empty"
    feature: AI-live
    required: false
  - name: ECK_IMAGE_WORKER_HOURS
    files: [wms/src/ai/image_summary.rs]
    default: "\"\" — no working-hours window, the worker runs anytime"
    effect: "local-time \"HH-HH\" window (end exclusive, may wrap past midnight) restricting when the image-summary worker runs; empty, \"0-24\", or unparsable = always on"
    feature: AI-live
    required: false
  - name: ECK_OCR_VERIFY
    files: [wms/src/ai/ocr_verify.rs]
    default: "\"dry\" on the AI author node (image-summary worker enabled or ECK_SUMMARY_WORKER not \"0\"), else \"off\"; always \"off\" with no AI credentials"
    effect: "mode for the background OCR-verify worker: off | dry (compute only) | on; skipped entirely on a cache node"
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
  - name: ECK_AI_USAGE_REPORT
    files: [core/src/ai.rs]
    default: "disabled unless truthy"
    effect: "enables a fire-and-forget usage-report POST after each AI call"
    feature: AI-live
    required: false
  - name: ECK_AI_MODEL_DENY
    files: [core/src/ai.rs]
    default: "none — no model is refused by the router"
    effect: "comma-separated list of model names the task router refuses to route to"
    feature: AI-live
    required: false
  - name: ECK_AI_TASK_ROUTES
    files: [core/src/ai.rs]
    default: "none — no per-task model-routing overrides"
    effect: "per-task model-routing overrides, formatted as \"task=model,task=model,...\""
    feature: AI-live
    required: false
  - name: ECK_AI_TASK_BUDGET
    files: [core/src/ai.rs]
    default: "\"\" — falls back to a per-task default table (e.g. brain≈25s, chat_compact≈60s, vision/classify≈120s)"
    effect: "per-task wall-clock time budget in seconds for the AI task router; resolved with explicit-task / wildcard / default precedence"
    feature: AI-live
    required: false
  - name: ECK_AI_HEDGE_SECS
    files: [core/src/ai.rs, wms/src/ai/orchestrator.rs]
    default: "none — no hedge request is fired (brain task only)"
    effect: "after this many seconds the task router fires a hedge request to a Vertex/Gemini fallback and takes whichever of the two responses returns first"
    feature: AI-live
    required: false
  - name: ECK_OR_REASONING
    files: [core/src/ai.rs]
    default: "none — no reasoning-effort override is sent"
    effect: "reasoning-effort override passed through to OpenRouter models that support one"
    feature: AI-live
    required: false
  - name: ECK_AI_DUMP_REQUESTS
    files: [core/src/ai.rs]
    default: "disabled"
    effect: "debug: dump every outgoing AI request payload to disk under ECK_AI_DUMP_DIR"
    feature: AI-live
    required: false
  - name: ECK_AI_DUMP_DIR
    files: [core/src/ai.rs]
    default: "\"./ai-dumps\""
    effect: "directory AI request-payload dumps are written to when ECK_AI_DUMP_REQUESTS is enabled"
    feature: AI-live
    required: false
  - name: ECK_AI_GEMINI_EXPLICIT_CACHE
    files: [core/src/ai.rs]
    default: "false"
    effect: "enables Gemini explicit context caching for eligible calls"
    feature: AI-live
    required: false
  - name: ECK_AI_GEMINI_CACHE_TTL_SECS
    files: [core/src/ai.rs]
    default: "3600 (minimum 60)"
    effect: "TTL in seconds for a Gemini explicit context cache entry"
    feature: AI-live
    required: false
  - name: ECK_AI_SHADOW
    files: [wms/src/ai/summarization.rs]
    default: "false"
    effect: "shadow-mode summarization: computes a summary but does not commit it, for comparing against the live result"
    feature: AI-live
    required: false
  - name: ECK_AI_IMAGE_POLICY
    files: [wms/src/ai/image_summary.rs]
    default: "none — falls back to the image-summary path's built-in gating"
    effect: "vision-call gating policy for the image-summary path"
    feature: AI-live
    required: false
  - name: ECK_AI_TOOL_CALLS_PER_TURN
    files: [wms/src/ai/orchestrator.rs]
    default: "16; \"0\"/\"off\" = no per-turn dispatch cap"
    effect: "max function/tool calls DISPATCHED per orchestrator model turn; calls beyond this in one turn get the fixed denial text instead of being executed"
    feature: AI-live
    required: false
  - name: ECK_AI_TOOL_CALLS_PER_TURN_ABORT
    files: [wms/src/ai/orchestrator.rs]
    default: "32 (raised if needed to stay above ECK_AI_TOOL_CALLS_PER_TURN); \"0\"/\"off\" = no ceiling"
    effect: "hard ceiling on the NUMBER of tool calls the model may issue in one turn; exceeding it aborts the run on this engine before anything is appended (model-malfunction guard, bounds worst-case run size)"
    feature: AI-live
    required: false
  - name: ECK_BRAIN_FALLBACK_MODEL
    files: [wms/src/ai/orchestrator.rs]
    default: "none — no fallback model chain is configured for the brain"
    effect: "fallback model (or chain) the orchestrator brain falls back to when the primary brain model call fails"
    feature: AI-live
    required: false
  - name: ECK_BRAIN_BENCH
    files: [wms/src/handlers/ai.rs]
    default: "disabled unless \"1\""
    effect: "enables the brain benchmark endpoint (POST /api/ai/bench/brain)"
    feature: AI-live
    required: false
  - name: ECK_BENCH_OR_KEY
    files: [wms/src/ai/orchestrator.rs]
    default: "\"\""
    effect: "OpenRouter API key used only by the brain bench harness, independent of the node's normal OPENROUTER_API_KEY"
    feature: AI-live
    required: false
  - name: ECK_BENCH_OR_BASE_URL
    files: [wms/src/ai/orchestrator.rs]
    default: "falls back to the normal OpenRouter base URL"
    effect: "OpenRouter base URL used only by the brain bench harness"
    feature: AI-live
    required: false
  - name: ECK_CHAT_DEFAULT_ENGINE
    files: [wms/src/handlers/ai.rs]
    default: "glm"
    effect: "default Brain-Chat engine when a chat request names none; only the configurable default, the product default stays glm by owner decision"
    feature: AI-live
    required: false
  - name: ECK_CHAT_HISTORY_MAX_CHARS
    files: [wms/src/handlers/ai.rs]
    default: "240000; \"0\"/\"off\" = unlimited"
    effect: "aggregate char budget bounding the whole Brain-Chat history fed into one prompt, not just each entry"
    feature: AI-live
    required: false
  - name: ECK_CHAT_KEEP_TURNS
    files: [wms/src/ai/orchestrator.rs]
    default: "15"
    effect: "number of verbatim conversation turns kept before compaction in a Brain-Chat session"
    feature: AI-live
    required: false
  - name: ECK_CHAT_COMPACT_CHUNK
    files: [wms/src/ai/orchestrator.rs]
    default: "6"
    effect: "number of rounds compacted per Brain-Chat compaction pass"
    feature: AI-live
    required: false
  - name: ECK_CHAT_RETENTION_DAYS
    files: [wms/src/main.rs]
    default: "none — no scheduled Brain-Chat transcript retention sweep runs"
    effect: "retention window (days) for the Brain-Chat transcript sweep"
    feature: AI-live
    required: false
  - name: GEMINI_CHAT_COMPACT_MODEL
    files: [wms/src/ai/orchestrator.rs]
    default: "falls back to GEMINI_GENERATION_MODEL when unset"
    effect: "Gemini model id used to compact Brain-Chat history"
    feature: AI-live
    required: false
  - name: ECK_SUMMARY_CF_KEYS
    files: [wms/src/ai/summarization.rs]
    default: "none — no extra custom-field labels are pulled into summarization context"
    effect: "lowercased Zoho custom-field labels feeding extra context into summarization (deployment-specific schema data)"
    feature: AI-live
    required: false
  - name: ECK_ORCHESTRATOR_WORKER
    files: [wms/src/main.rs]
    default: "disabled unless \"1\""
    effect: "enables the operator-inbox orchestrator background worker on this node"
    feature: AI-live
    required: false
  - name: ECK_ZOHO_FULL_REARM_BUDGET
    files: [wms/src/services/support.rs]
    default: "a fixed default budget when unset (see source)"
    effect: "budget cap (number of tickets) for a full re-arm/re-summarize sweep triggered against Zoho-sourced tickets"
    feature: AI-live
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
  - name: TRIP_OPEN_AUTOCLOSE_HOURS
    files: [wms/src/services/cell_resolver.rs]
    default: "24"
    effect: "hours without recorded activity after which the trip's HOME node auto-closes an abandoned open trip (recording died and never resumed); 0 disables the auto-close"
    feature: geo
    required: false
  - name: ECK_GEO_HQ_ZIP
    files: [wms/src/services/attachment_geo.rs]
    default: "none — no HQ postal-code fallback anchor is used"
    effect: "this deployment's HQ postal code, used as a fallback geo anchor when a ticket's own address cannot be resolved"
    feature: geo
    required: false
  - name: ECK_GEO_HQ_CITY
    files: [wms/src/services/attachment_geo.rs]
    default: "none — no HQ city fallback anchor is used"
    effect: "this deployment's HQ city, used as a fallback geo anchor when a ticket's own address cannot be resolved"
    feature: geo
    required: false
  - name: ECK_GEO_FALLBACK_LAT
    files: [wms/src/services/geocoder.rs, wms/src/handlers/geo.rs]
    default: "none — no fallback latitude is used for unresolved tickets"
    effect: "fallback latitude plotted for a ticket the geocoder could not resolve"
    feature: geo
    required: false
  - name: ECK_GEO_FALLBACK_LNG
    files: [wms/src/services/geocoder.rs, wms/src/handlers/geo.rs]
    default: "none — no fallback longitude is used for unresolved tickets"
    effect: "fallback longitude plotted for a ticket the geocoder could not resolve"
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
  - name: ECK_NAS_INGEST_ROOT
    files: [wms/src/handlers/ops.rs]
    default: "none — the one-shot bulk file-ingest ops job has no root path and cannot run"
    effect: "root filesystem path for the one-shot bulk file-ingest ops job (POST /api/admin/nas-ingest)"
    feature: ops
    required: false
  - name: ECK_PLUGIN_RUNTIME
    files: [wms/src/main.rs]
    default: "none — the WASM (extism) plugin runtime does not start on this node"
    effect: "enables the WASM plugin runtime on this node"
    feature: ops
    required: false
  - name: ECK_PLUGIN_REQUIRE_SIGNED
    files: [wms/src/handlers/plugins.rs]
    default: "a fixed default when unset (see source)"
    effect: "requires a valid issuer signature on every loaded WASM plugin, enforcing the license-gate CRL/kill-switch"
    feature: ops
    required: false
  - name: ECK_DEMO_PLUGIN_WASM
    files: [core/src/plugin/engine.rs]
    default: "none — falls back to the built-in bundled demo plugin path, if any"
    effect: "path override for a demo/bundled WASM plugin"
    feature: ops
    required: false
  - name: ECK_POS_DEMO_PLUGIN_WASM
    files: [core/src/plugin/engine.rs]
    default: "none — dev/test only; required to run the #[ignore]d demo_pos_happy_hour_plugin test, which otherwise panics with a message naming scripts/build-pos-demo-plugin.ps1"
    effect: "path to a locally-built demo POS price-rules WASM plugin, used only by an ignored integration test that proves a real extism-pdk build round-trips through the POS plugin engine's hot path"
    feature: ops
    required: false
  - name: ECK_TOKIO_MAX_BLOCKING
    files: [wms/src/main.rs]
    default: "a fixed default when unset (see source)"
    effect: "caps the size of the tokio blocking-thread pool"
    feature: ops
    required: false
  - name: ECK_SELF_WATCHDOG_INTERVAL_SECS
    files: [wms/src/services/self_watchdog.rs]
    default: "30 (0 disables the watchdog)"
    effect: "interval for the off-runtime self-watchdog that probes 127.0.0.1:<port>/E/health and kills the process after repeated unresponsive probes (recovers a wedged tokio runtime)"
    feature: ops
    required: false
  - name: ECK_ALLOC_STATS_SECS
    files: [wms/src/services/self_watchdog.rs]
    default: "300 (0 disables the thread; Linux/glibc only, no-op elsewhere)"
    effect: "interval for the allocator-stats thread; on the jemalloc build (feature jemalloc, default-on) logs [jemalloc] allocated=… active=… resident=… retained=… mapped=… metadata=… MiB | proc.anon=…; on a --no-default-features (glibc) build logs [malloc-trim] anon A→B MiB from a periodic malloc_trim(0)"
    feature: ops
    required: false
  - name: ECK_MALLOC_TRIM_SECS
    files: [wms/src/services/self_watchdog.rs]
    default: "legacy alias for ECK_ALLOC_STATS_SECS, honoured only when ECK_ALLOC_STATS_SECS is unset"
    effect: "see ECK_ALLOC_STATS_SECS"
    feature: ops
    required: false
  - name: _RJEM_MALLOC_CONF
    files: [wms/src/main.rs]
    default: "unset (compiled-in default: background_thread:true,dirty_decay_ms:5000,muzzy_decay_ms:0,narenas:4)"
    effect: "process env override of the jemalloc runtime options compiled into _rjem_malloc_conf (feature jemalloc); e.g. narenas:2"
    feature: ops
    required: false
  - name: ECK_SELF_WATCHDOG_FAILS
    files: [wms/src/services/self_watchdog.rs]
    default: "a fixed default consecutive-fail count when unset (see source)"
    effect: "how many consecutive unresponsive health probes trip the self-watchdog into killing the process"
    feature: ops
    required: false
  - name: ECK_SELF_WATCHDOG_GRACE_SECS
    files: [wms/src/services/self_watchdog.rs]
    default: "a fixed default grace window when unset (see source)"
    effect: "startup grace window before the self-watchdog begins probing"
    feature: ops
    required: false
  - name: ECK_SELF_WATCHDOG_PROBE_PORT
    files: [wms/src/services/self_watchdog.rs]
    default: "falls back to the node's own listen PORT"
    effect: "override the loopback port the self-watchdog probes for /E/health"
    feature: ops
    required: false
  - name: ECK_SELF_WATCHDOG_PATH
    files: [wms/src/services/self_watchdog.rs]
    default: "\"/E/health\""
    effect: "override the loopback path the self-watchdog probes for liveness"
    feature: ops
    required: false
  - name: ECK_DAILY_SYNC_HOUR
    files: [wms/src/services/scheduler.rs]
    default: "a fixed default hour when unset (see source)"
    effect: "hour of day (local) the daily full-sync job runs"
    feature: ops
    required: false
  - name: ECK_REVEAL_RATE
    files: [wms/src/reveal_guard.rs]
    default: "a fixed default rate when unset (see source)"
    effect: "rate limit applied to the MCP reveal_tokens method"
    feature: ops
    required: false
  - name: XELIXIR_AGENT_UNIT
    files: [wms/src/services/agent_manager.rs]
    default: "none — no systemd unit name is known for the local desktop agent"
    effect: "systemd unit name of the local xelixir desktop agent, used for service-management ops verbs"
    feature: ops
    required: false
  - name: XELIXIR_AGENT_RUN_MODE
    files: [wms/src/services/agent_manager.rs]
    default: "none — falls back to the agent's built-in default run mode"
    effect: "run-mode flag for the local xelixir desktop agent"
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
  - name: ECK_NAV_SITE_URL
    files: [wms/src/main.rs]
    default: "none — no external site link is shown in the sidebar nav"
    effect: "external site URL surfaced in the sidebar nav (e.g. an exhibition/features page)"
    feature: other
    required: false
  - name: ECK_DEFAULT_VIEW
    files: [wms/src/main.rs]
    default: "none — falls back to the built-in default UI view on boot"
    effect: "default UI view shown on boot (a kiosk deployment needs this set to \"dashboard\")"
    feature: other
    required: false
  - name: ECK_DEMO_LOGIN_HINT
    files: [wms/src/main.rs]
    default: "none — no demo-login hint text is shown on the login screen"
    effect: "hint text shown on the login screen, for demo/showcase deployments"
    feature: other
    required: false
  - name: ECK_PUBLIC_OBSERVER
    files: [wms/src/handlers/auth.rs]
    default: "none — no public read-only observer-tier login is available"
    effect: "enables a public read-only observer-tier login"
    feature: other
    required: false
  - name: SCRAPER_RUNTIME
    files: [wms/src/handlers/scraper_proxy.rs]
    default: "\"bun\""
    effect: "JS runtime binary used to spawn the local scraper process"
    feature: other
    required: false
  - name: CLAIMLIST_TOKEN_FILE
    files: [wms/src/services/claimlist.rs, wms/src/handlers/claimlist.rs]
    default: "none — the external claim-list workbook integration no-ops when unset"
    effect: "path to the auth-token file for the external claim-list workbook integration"
    feature: other
    required: false
  - name: CLAIMLIST_SHARE_URL
    files: [wms/src/services/claimlist.rs]
    default: "none — falls back to the integration's built-in default share URL"
    effect: "share URL override for the external claim-list workbook"
    feature: other
    required: false
  - name: ECK_CLAIM_PRODUCT_GROUP
    files: [wms/src/handlers/claimlist.rs]
    default: "\"\" — the product-group column is left blank on a minted claim"
    effect: "deployment data: default product-group label for a minted claim-list row when its source ticket carries none"
    feature: other
    required: false
  - name: ECK_NAS_DOCS_ROOT
    files: [wms/src/handlers/claimlist.rs]
    default: "\"\" — the presentation-only nas_folder label is omitted"
    effect: "deployment data: root of the legacy NAS document tree, used ONLY for the presentation-only nas_folder projection label (the CAS filestore stays the source of truth); e.g. a UNC share path"
    feature: other
    required: false
  - name: ECK_NAS_PHOTO_SUBFOLDERS
    files: [wms/src/services/nas_ingest.rs]
    default: "\"\" (comma-separated list; empty = none)"
    effect: "deployment data: extra first-level NAS subfolder names (lowercased) classified as photo evidence during NAS ingest, beyond the built-in fotos*/2.gerät* folders"
    feature: other
    required: false
  - name: PAIR_BOARD_URLS
    files: [wms/src/handlers/device.rs]
    default: "none — no pairing-board relay URLs are advertised in a pairing QR"
    effect: "comma-separated pairing-board relay URLs advertised in a pairing QR code"
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

## 13. vault (zero-knowledge PII vault)

```yaml
vars:
  - name: ECK_VAULT_DISABLE
    files: [core/src/pii_vault.rs]
    default: "false"
    effect: "disables the zero-knowledge PII vault entirely"
    feature: other
    required: false
  - name: ECK_RAW_AT_REST
    files: [core/src/pii_vault.rs]
    default: "false"
    effect: "deliberate opt-out allowing raw (unmasked) PII to be stored at rest instead of being routed through the vault"
    feature: other
    required: false
  - name: ECK_VAULT_NODES
    files: [core/src/vault/client.rs]
    default: "none — no vault share-holder nodes are known; MUST be set before vault bootstrap"
    effect: "comma-separated list of the vault share-holder nodes (node-KEK + Shamir 2-of-3 scheme)"
    feature: other
    required: false
  - name: ECK_VAULT_ENROLL_CODE
    files: [wms/src/main.rs]
    default: "none — this node presents no enrollment code and cannot join the vault"
    effect: "one-time enrollment code a node presents to join the PII vault"
    feature: other
    required: false
  - name: ECK_VAULT_SERVE
    files: [wms/src/handlers/vault_safe.rs]
    default: "false"
    effect: "enables this node's vault-safe share-holder HTTP surface (/api/vault/*)"
    feature: other
    required: false
  - name: ECK_VAULT_KEYFILE
    files: [core/src/keyring.rs]
    default: "none — this node has no persisted vault key file"
    effect: "path to this node's vault key file (node KEK material)"
    feature: other
    required: false
  - name: ECK_VAULT_KEK_PATH
    files: [core/src/vault/kek.rs]
    default: "none — a conventional location is used (see source); keep it OUTSIDE the data/ dir"
    effect: "filesystem location of this node's vault key-encryption key (KEK), which never leaves the node"
    feature: other
    required: false
  - name: ECK_VAULT_KEK_FILE
    files: [core/src/vault/kek.rs]
    default: "none — alternate name for the KEK file location"
    effect: "same as ECK_VAULT_KEK_PATH, alternate variable name checked for the node KEK file"
    feature: other
    required: false
  - name: ECK_DATAFS_KEYFILE
    files: [core/src/keyring.rs]
    default: "none — this node has no persisted DataFS key file"
    effect: "path to this node's DataFS (encrypted-at-rest filesystem) key file"
    feature: other
    required: false
  - name: ECK_DATAFS_GOCRYPTFS
    files: [core/src/vault/datafs.rs]
    default: "none — falls back to the gocryptfs binary found on PATH"
    effect: "override path to the gocryptfs binary backing the encrypted DataFS volume"
    feature: other
    required: false
  - name: ECK_DATAFS_UNMOUNT_ON_EXIT
    files: [core/src/vault/datafs.rs]
    default: "false"
    effect: "unmounts the encrypted DataFS volume on process exit"
    feature: other
    required: false
  - name: ECK_SWEEP_PAGE_SIZE
    files: [wms/src/services/vault_sweep.rs]
    default: "a fixed default page size when unset (see source)"
    effect: "page size for the background vault back-fill sweep"
    feature: other
    required: false
  - name: ECK_SWEEP_PAGE_DELAY_MS
    files: [wms/src/services/vault_sweep.rs]
    default: "a fixed default delay when unset (see source)"
    effect: "inter-page delay pacing the background vault back-fill sweep"
    feature: other
    required: false
```

---

## 14. KV / SurrealKV tuning

```yaml
vars:
  - name: ECK_KV_USERS_BLOCK_CACHE_MB
    files: [core/src/db.rs]
    default: "16"
    effect: "SurrealKV block-cache size (MiB) for the node-local setup-admin users store"
    feature: other
    required: false
  - name: ECK_KV_USERS_MEMTABLE_MB
    files: [core/src/db.rs]
    default: "16"
    effect: "maximum SurrealKV memtable size (MiB) before flush, for the node-local setup-admin users store"
    feature: other
    required: false
  - name: ECK_KV_BLOCK_CACHE_MB
    files: [core/src/db.rs]
    default: "min(SurrealKV's own default, 20% of system RAM, 512) MiB"
    effect: "SurrealKV block-cache size (MiB) for the main embedded store"
    feature: other
    required: false
  - name: ECK_KV_MEMTABLE_MB
    files: [core/src/db.rs]
    default: "RAM-tiered (64 MiB under 4 GiB system RAM; 256/1024/4096 MiB at higher tiers); see source"
    effect: "maximum SurrealKV memtable size (MiB) before flush"
    feature: other
    required: false
  - name: ECK_HNSW_CACHE_MB
    files: [core/src/db.rs]
    default: "256 MiB when system RAM ≥ 4 GiB, else 64 MiB"
    effect: "cache size (MiB) for the HNSW vector-index"
    feature: other
    required: false
```

---

## 15. OpenRouter / task-routing / cheap-tier seam

This is an env-gated seam, not a required dependency: the upstream OpenRouter key/URL
are operator-supplied, and this node will not serve OpenRouter requests to other mesh
nodes unless `ECK_OR_SERVE=1` is explicitly set.

```yaml
vars:
  - name: OPENROUTER_API_KEY
    files: [core/src/ai.rs, wms/src/ai/orchestrator.rs]
    default: "\"\" — no operator OpenRouter key is configured, cheap-tier/non-Gemini model calls are unavailable"
    effect: "operator's OpenRouter API key, used for non-Gemini/cheap-tier model calls"
    feature: AI-live
    required: false
  - name: XELTH_ZAI_KEY
    files: [core/src/ai.rs]
    default: "falls back to OPENROUTER_API_KEY"
    effect: "Z.ai key used in bench/test context, in place of the general OpenRouter key"
    feature: AI-live
    required: false
  - name: ECK_OR_PROXY_URL
    files: [core/src/ai.rs]
    default: "\"\" — cheap-tier calls are not routed through an OR proxy"
    effect: "base URL of an OpenRouter proxy this node routes cheap-tier calls through"
    feature: AI-live
    required: false
  - name: ECK_OR_BASE_URL
    files: [core/src/ai.rs, wms/src/handlers/ops.rs]
    default: "none — falls back to the OpenRouter SDK's own default upstream URL"
    effect: "OpenRouter upstream base URL"
    feature: AI-live
    required: false
  - name: ECK_OR_UPSTREAM_URL
    files: [wms/src/handlers/or_proxy.rs]
    default: "none — falls back to ECK_OR_BASE_URL / the built-in default"
    effect: "upstream override specifically for this node's OR proxy handler"
    feature: AI-live
    required: false
  - name: ECK_OR_TIMEOUT_SECS
    files: [core/src/ai.rs, wms/src/handlers/or_proxy.rs]
    default: "minimum 120s"
    effect: "timeout applied to OpenRouter requests"
    feature: AI-live
    required: false
  - name: ECK_OR_POLL_MS
    files: [core/src/ai.rs]
    default: "a fixed default poll interval when unset (see source)"
    effect: "poll interval (ms) for the OpenRouter relay-carried request poller"
    feature: AI-live
    required: false
  - name: ECK_OR_SERVE
    files: [wms/src/services/or_relay_poller.rs, wms/src/main.rs]
    default: "off unless \"1\""
    effect: "makes this node SERVE OpenRouter requests for other mesh nodes (POST /api/ai/or + the relay /E/ai/* poller)"
    feature: AI-live
    required: false
  - name: ECK_OR_SERVE_NODE
    files: [core/src/ai.rs]
    default: "\"\""
    effect: "node identity this node advertises when serving OpenRouter requests for others"
    feature: AI-live
    required: false
  - name: ECK_OR_SERVE_CONCURRENCY
    files: [wms/src/services/or_relay_poller.rs]
    default: "a fixed default concurrency when unset (see source)"
    effect: "maximum concurrent OR-serve requests this node handles for other mesh nodes"
    feature: AI-live
    required: false
  - name: ECK_OR_SERVE_MODELS
    files: [wms/src/handlers/or_proxy.rs]
    default: "none — no model allow-list, OR-serve gating falls back to its built-in default"
    effect: "allow-list of models this node will serve on behalf of other mesh nodes"
    feature: AI-live
    required: false
  - name: ECK_OR_PRICES
    files: [wms/src/handlers/or_proxy.rs, wms/src/ai/image_summary.rs]
    default: "none — falls back to the OR proxy's built-in default price table"
    effect: "per-model price table used to meter OR proxy usage"
    feature: AI-live
    required: false
  - name: ECK_OR_MASTER_KEY
    files: [wms/src/handlers/or_proxy.rs]
    default: "none — no admin master key is configured for the OR proxy"
    effect: "admin master key for the OR proxy's privileged operations"
    feature: AI-live
    required: false
  - name: ECK_OR_CLIENT_CAPS
    files: [wms/src/handlers/or_proxy.rs]
    default: "\"\" — no per-client rate/spend caps are enforced on the OR proxy"
    effect: "per-client rate/spend caps for the OR proxy"
    feature: AI-live
    required: false
  - name: ECK_OR_MAX_TOKENS
    files: [core/src/ai.rs]
    default: "3000 per model; 12000 for xiaomi/* models (reasoning models spend budget on thinking first)"
    effect: "override string \"<model-prefix>=<n>,…\" for the per-model max_tokens floor applied to an OpenRouter/cheap-tier request's maxOutputTokens"
    feature: AI-live
    required: false
  - name: ECK_OR_SLUG_API_KEY
    files: [core/src/ai.rs]
    default: "\"\" — a SLUG model (name contains \"/\") cannot be routed to the vendor-direct slug upstream"
    effect: "API key for the vendor-direct \"slug\" OpenRouter-compatible upstream (e.g. Z.ai's OpenAI-compatible endpoint) used for SLUG-named models"
    feature: AI-live
    required: false
  - name: ECK_OR_SLUG_BASE_URL
    files: [core/src/ai.rs]
    default: "https://openrouter.ai/api/v1"
    effect: "base URL for the vendor-direct \"slug\" upstream paired with ECK_OR_SLUG_API_KEY"
    feature: AI-live
    required: false
  - name: ECK_OR_SLUG_MASTER_KEY
    files: [wms/src/handlers/or_proxy.rs]
    default: "\"\" — the OR-serve handler's second (slug) upstream is unavailable, serving falls back to ECK_OR_UPSTREAM_URL"
    effect: "master key for the OR-serve handler's slug upstream, used when serving a SLUG-named model for a peer mesh node"
    feature: AI-live
    required: false
  - name: ECK_OR_SLUG_UPSTREAM_URL
    files: [wms/src/handlers/or_proxy.rs]
    default: "\"\" — paired with ECK_OR_SLUG_MASTER_KEY; empty disables the slug upstream path for OR-serve"
    effect: "upstream URL for the OR-serve handler's slug route (paired with ECK_OR_SLUG_MASTER_KEY), used when a SLUG-named model can't be served by the vendor-direct upstream"
    feature: AI-live
    required: false
```

---

## 16. PII remote-masking / local NER

```yaml
vars:
  - name: ECK_PII_SPANS_URL
    files: [wms/src/ai/pii_remote.rs]
    default: "none — no remote PII-span detector is called; this node relies entirely on its own local masking"
    effect: "URL of a remote PII-span (NER) detector this node calls to mask prose it cannot mask locally"
    feature: AI-live
    required: false
  - name: ECK_PII_SPANS_TOKEN
    files: [wms/src/ai/pii_remote.rs]
    default: "none — required alongside ECK_PII_SPANS_URL to authenticate"
    effect: "bearer token for the remote PII-span detector"
    feature: AI-live
    required: false
  - name: ECK_PII_SPANS_TIMEOUT_SECS
    files: [wms/src/ai/pii_remote.rs]
    default: "a fixed default timeout when unset (see source)"
    effect: "timeout for the remote PII-span detector call"
    feature: AI-live
    required: false
  - name: ECK_PII_SPANS_SERVE
    files: [wms/src/main.rs]
    default: "none — this node does not serve the PII-span detector for other nodes"
    effect: "makes this node serve the PII-span detector (POST /api/pii/spans) for other nodes, sharing its local candle NER model as a service"
    feature: AI-live
    required: false
  - name: ECK_NER_IDLE_UNLOAD_SECS
    files: [wms/src/ai/local_ner.rs]
    default: "a fixed default idle window when unset (see source)"
    effect: "unloads the local candle NER model from memory after this idle interval, for RAM relief"
    feature: AI-live
    required: false
  - name: ECK_EMBED_IDLE_UNLOAD_SECS
    files: [wms/src/ai/local_embed.rs]
    default: "a fixed default idle window when unset (see source)"
    effect: "unloads the local embedding model from memory after this idle interval, for RAM relief"
    feature: AI-live
    required: false
  - name: ECK_PDF_RENDER_CMD
    files: [wms/src/ai/attachments.rs]
    default: "none — PDF pages are not pre-rasterized by an external command before OCR/vision"
    effect: "external command used to rasterize PDF pages before OCR/vision extraction"
    feature: AI-live
    required: false
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
  # added 2026-09-07 (attachment bench round D)
  - name: ECK_BRAIN_IMAGE_PASSTHROUGH
    files: [wms/src/ai/orchestrator.rs, wms/src/ai/attachment_image.rs]
    default: "1 (on); \"0\" disables"
    effect: "after analyze_attachment_visual / ocr_attachment the page image itself (JPEG ≤1280 px, ≤900 KB, ≤3 per run, raster or scan-PDF page) is appended as a user turn so multimodal loop models see the page, not only Gemini's description; bench request field image_passthrough overrides per run"
    feature: ai-brain
    required: false
  - name: ECK_VISION_CACHE_COS
    files: [wms/src/ai/attachments.rs]
    default: "0.92"
    effect: "cosine floor for the semantic vision-journal cache: a prior masked answer for the same file/page/region whose question embedding is at least this similar is served instead of a new paid Gemini call (exact normalised-question hits are always served)"
    feature: ai-brain
    required: false
  # added 2026-09-07 (backlog audit): serial-prefix -> model when the model field is empty
  - name: ECK_SERIAL_MODEL_PREFIXES
    files: [wms/src/services/support.rs]
    default: "\"\" (no derivation)"
    effect: "comma-separated PREFIX=Model pairs (case-insensitive, longest prefix wins) used to fill meta.device_model from meta.serial_number at ticket ingest when the model custom field is empty; never overrides an explicit model. Vendor numbering scheme = deployment data. Existing rows need a one-off backfill on the HOME node (UPDATE document SET meta.device_model = ... WHERE meta.device_model = '' AND string::starts_with(meta.serial_number, 'F9'))"
    feature: scraper-proxy
    required: false

  - name: ECK_STALE_REPLY_DAYS
    files: [wms/src/ai/stale_reply.rs, wms/src/ai/orchestrator.rs]
    default: "7"
    effect: "silence threshold (days) for the deterministic \"customer delivered, we are silent\" triage pre-check: when a ticket's newest thread is inbound AND we have been silent at least this many days, the orchestrator injects a CUSTOMER DELIVERED block into the brain's system prompt (open with an apology + concrete next step, never re-ask the customer) and records the verdict on the ai_task row (meta.stale_reply). \"0\" disables the check entirely. The check is fail-open — any DB error is logged at debug and the run proceeds."
    feature: ai-brain
    required: false
