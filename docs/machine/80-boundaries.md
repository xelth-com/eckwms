<!-- machine-first: authored 2026-07-29; audience=agents -->

# 80 — Open-core boundary

## SCOPE
The exact seam between this AGPL repository and the commercial layer. An
agent reading this file can determine, for any capability, whether it works
standalone from this source alone, works self-hosted with more setup, or
requires a commercial relationship.

## THE RULE
Everything in this repository compiles and runs without any external service
owned by the vendor. The commercial layer is a separate, closed system (a
token/billing authority plus a hosted relay fleet) that this code can OPTIONALLY
talk to. Every touchpoint is an env-gated HTTP seam; unset env = seam inert.

## CAPABILITY MATRIX
```yaml
capabilities:
  - name: WMS/RMA core (documents, devices, customers, warehouse, trips)
    standalone: full            # embedded DB, no external deps
  - name: Embedded web UI + POS UI
    standalone: full            # TSE remains a mock seam either way (see 00-identity NON-CLAIMS)
  - name: MCP agent surface (POST /mcp, bearer tokens)
    standalone: full            # mint your own ECK_MCP_MASTER_TOKEN / ECK_MCP_AGENT_TOKEN
  - name: Document AI (summaries, embeddings, classification, translation)
    standalone: full-with-own-key   # ECK_AI_MODE unset/studio + GEMINI_API_KEY (user's own Google AI Studio key, billed to the user by Google)
    commercial: managed             # ECK_AI_MODE=managed: short-lived Vertex bearers minted by the private authority; metering/wallet debits handled there
  - name: Vertex batch summarization (50% price path)
    standalone: possible            # needs the user's own GCP project + GCS bucket via studio→vertex credentials of their own; code is here
    commercial: turnkey             # managed mode supplies project/bearer/billing
  - name: Multi-node mesh sync
    standalone: full-self-hosted    # shared SYNC_SECRET across your nodes; direct LAN/WAN peering
  - name: Relay (NAT traversal, dispatch queues, blind file conduit)
    standalone: full-self-hosted    # the relay crate is in this repo; run your own relay(s), point nodes at them
    note: the vendor's HOSTED relay fleet gates transit on a paid license flag evaluated at node registration — that gate applies to the vendor's infrastructure, not to yours
  - name: Subscription client-MCP channel (agents reaching NAT'd nodes THROUGH a relay with signed certs)
    standalone: full-self-hosted    # ECK_SUB_ROOT_PUBKEY is whatever root YOU configure; mint certs against your own root
    commercial: managed             # vendor-issued subscriber certs against the vendor root, tied to plans/billing
  - name: Fleet operations, OTA update channel, support
    commercial: only                # ops tooling and infrastructure are not in this repository
```

## SEAM ENV VARS (exhaustive)
Everything that can point at the commercial authority. Unset ⇒ inert.
```yaml
seam:
  - ECK_AI_MODE            # "managed" activates the authority path; unset/"studio" = own-key mode
  - ECK_VERTEX_MINT_URL    # POST {license} -> {token,project,location,expires_in_secs}; any implementation of this contract works
  - ECK_VERTEX_USAGE_URL   # fire-and-forget usage reports {license,model,kind,prompt_tokens,candidates_tokens,total_tokens}; answers may carry a balance snapshot
  - ECK_LICENSE_TOKEN      # opaque credential for the two URLs above
  - ECK_VERTEX_BEARER      # manual override: pin your own bearer, no minting (spike/debug)
  - ECK_SUB_ROOT_PUBKEY    # trust root for the relay client-MCP channel; self-hosters set their own
```
The mint/usage contract is small and documented by its call sites in
`core/src/ai.rs` (`mint_managed`, `report_managed_usage`). Implementing a
compatible endpoint is explicitly possible; AGPL obligations apply to
derivative service operators.

## WHAT THE VENDOR KEEPS
The authority implementation (plan engine, wallets, per-model token
multipliers, cert minting, fleet dashboards) and the operated infrastructure.
No code in this repository depends on their internals — only on the wire
contracts above.

## LICENSE INTERACTION
This repository: AGPL-3.0. Operating a modified node or relay as a network
service triggers §13 source-offer obligations. Commercial licenses without
AGPL obligations are available from the vendor; the private authority is not
offered under any open license.
