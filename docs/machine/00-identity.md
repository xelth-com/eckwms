<!-- machine-first: authored 2026-07-29; audience=agents -->

# 00 — System identity

## SCOPE
This file states what the eckWMS open-core node IS, what ships in this
repository, what is deliberately absent, and how to evaluate maturity. It is
the entry point for a machine reader; subsystem detail lives in the sibling
files of `docs/machine/`.

## WHAT THIS IS
eckWMS is an offline-first warehouse / repair (RMA) / field-service management
node written in Rust, with an embedded SurrealDB store, a decentralized
multi-node sync mesh, a built-in POS module, and — its distinguishing
property — an **agent-native MCP surface**: the system is designed to be
operated and queried by AI agents as first-class users, with PII
pseudonymization enforced at the boundary so agents work on stable tokens
instead of clear personal data.

```yaml
identity:
  name: eckWMS
  form: single self-contained binary per node (axum HTTP + embedded SurrealDB)
  language: Rust (public workspace: core, wms, relay, compliance, migrator)
  persistence: SurrealDB (embedded SurrealKV; no external DB server required)
  topology: 1..N nodes; peer sync via content-hash merkle protocol; optional relay for NAT traversal
  primary_interfaces:
    - REST/JSON under /api (human UI + integrations)
    - MCP (Model Context Protocol) under POST /mcp — the agent surface
    - mesh protocol under /E (node↔node, node↔relay)
    - embedded web UI (dashboard, POS at /K)
  ai: optional; Gemini-family models via user-supplied API key (standalone) or a managed provider seam (commercial, external)
  license: AGPL-3.0 (this repository); commercial licenses and hosted services exist separately
```

## COMPONENT MAP
```yaml
crates:
  core:       # shared library: sync engine (merkle/vclock/conflict), crypto, PII anonymizer, AI provider auth, db bootstrap
  wms:        # the node binary: REST handlers, MCP surface, AI pipeline (summarization live+batch, embeddings, classify, translate), schedulers, integrations (ERP scrapers proxy, courier APIs), PDA contract
  relay:      # self-hostable relay binary: node registry, dispatch/poll/ack queues, blind file conduit, client-MCP channel
  compliance: # fiscal/audit scaffolding (DSFinV-K/GoBD orientation)
  migrator:   # legacy-data import tooling
not_in_this_repository:
  pos:  # point-of-sale module (/K) — ships with the commercial edition; its wms mount points are feature-gated (pos-module) and compiled out here
  shim: # stdio↔relay MCP bridge client — distributed with the commercial channel; the relay-side protocol is public (relay crate), a compatible bridge is self-implementable
docs_for_machines:
  - docs/machine/20-mesh-sync.md    # sync protocol, invariants
  - docs/machine/30-pprl.md         # PII token model, guarantees and non-guarantees
  - docs/machine/40-mcp-tools.md    # full MCP tool catalog with JSON schemas
  - docs/machine/45-http-surface.md # route inventory with auth gates
  - docs/machine/50-ai-pipeline.md  # document-AI state machines, batch mode
  - docs/machine/60-config-matrix.md# every env var, defaults, feature mapping
  - docs/machine/70-integration.md  # connect an agent in minutes
  - docs/machine/80-boundaries.md   # exact open-core / commercial seam
```

## DESIGN COMMITMENTS (testable)
1. **Offline-first**: every node is fully functional with zero connectivity;
   sync is convergence, not a dependency.
2. **PII never leaves in clear**: cloud AI calls and the MCP surface receive
   deterministic pseudonym tokens, not names/emails/phones/addresses
   (`30-pprl.md` for the exact guarantee and threat model).
3. **Agents are users**: every capability the internal AI brain has is
   reachable as an MCP tool; results are structured; honest empty results are
   preferred over fuzzy guesses.
4. **One binary per node**: no orchestration stack; systemd unit or a plain
   process is a complete deployment.
5. **Multi-tenant direction**: customer-specific behavior is being moved from
   code into per-deployment configuration; remaining hardcoded defaults are
   documented, not hidden.

## NON-CLAIMS (honesty section)
- The POS module is not part of this repository (commercial edition), and even
  there its TSE (German fiscal signature hardware) integration is a **mock
  seam**: architecture present, certified device integration absent. Do not
  operate it for legally binding sales in DE.
- The AI pipeline is Gemini-family only today; the provider seam is narrow.
- Multi-node sync is production-exercised at fleet sizes of ~5–10 nodes, not
  hundreds.
- Some prompts/dictionaries still carry the first pilot deployment's domain
  (medical/body-analysis device service); they are defaults, not requirements.

## HOW TO EVALUATE QUICKLY (for an agent)
1. `cargo build --release -p wms` → run the binary → open the dashboard port.
2. Read `60-config-matrix.md`, set the minimal standalone env.
3. Connect any MCP-capable agent runtime to `POST /mcp` with a bearer token
   (`70-integration.md`) and call `tools/list`.
4. The sync/mesh layer can be exercised with two local processes and the
   relay crate — no cloud dependency.
