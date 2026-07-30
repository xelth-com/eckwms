<!-- machine-first: authored 2026-07-29; audience=agents -->

# 70 — Integration quickstart (for agents)

## SCOPE
Minimal, copy-pastable paths from "I have this repo / a running node" to "my
agent runtime is calling tools". Assumes the reader is itself an agent or an
agent framework configurator.

## A. Run a node (standalone, no cloud)
```bash
cargo build --release -p wms
# minimal env (see 60-config-matrix.md for the full matrix):
#   ECK_MCP_MASTER_TOKEN=<random hex, min 32 bytes>   # full-detail tier
#   ECK_MCP_AGENT_TOKEN=<different random hex>        # masked tier
#   GEMINI_API_KEY=<optional, enables document AI>    # plus GEMINI_*_MODEL vars
./target/release/wms          # HTTP on the configured port (default in config matrix)
```
No AI keys → the node runs; AI-derived fields (summaries, embeddings) stay
empty and every other capability works.

## B. Connect an MCP client (direct HTTP)
The node embeds an MCP server at `POST /mcp` (streamable HTTP transport).
```bash
# Claude Code
claude mcp add --transport http eckwms http://<node-host>:<port>/mcp \
  --header "Authorization: Bearer <ECK_MCP_MASTER_TOKEN or ECK_MCP_AGENT_TOKEN>"
```
Any MCP-capable runtime works the same way: streamable-HTTP endpoint + bearer.
`tools/list` output differs by tier (master sees reveal/export tools).

## C. What to call first (canonical flows)
```yaml
flows:
  - goal: anything about a customer
    call: customer_360 {query: <fragment|token|email>}
    then: if ambiguous:true → narrow with the exact email/company of one candidate
  - goal: a device's service history
    call: device_history {serial: "<serial>"}
  - goal: find tickets
    call: ticket_search {query, status?, limit?}   # subject/number match
    fallback: search_database {query}              # hybrid BM25+vector, finds unindexed
  - goal: read what customer and agent actually wrote (verbatim thread bodies)
    call: ticket_thread {ticket: "<public number|doc id>", only_new?: true, limit?: 20}
  - goal: neighbors of a known ticket
    call: similar_tickets {ticket_number}
  - goal: attachment content
    call: list_ticket_attachments → ocr_attachment / analyze_attachment_visual
```
Full catalog with JSON schemas: `40-mcp-tools.md`.

## D. The PII contract you MUST honor
Results never contain clear personal data on the agent tier. You will see
stable pseudonym tokens (`Name_<16hex>`, `Email_…`, `Phone_…`, `Address_…`,
sometimes `Company_…`). Properties an agent can rely on:
1. Same person ⇒ same token, across every tool and record — correlate freely.
2. Tokens are **valid query inputs**: feed a token back into `customer_360` or
   `search_database` to pivot without ever seeing the clear value.
3. De-pseudonymization exists only master-side and only into local files
   (`reveal_file`-class tools). Do not attempt reconstruction; see
   `30-pprl.md` for the threat model.

## E. Reaching a NAT'd node through a relay (optional)
When the node has no inbound route, a stdio↔relay bridge client carries MCP:
agent runtime ⇄ bridge (stdio) ⇄ relay dispatch/poll (`/E/c/*`, relay crate —
public) ⇄ node (`/mcp/signed`). Signed subscriber certs authorize the channel
(`80-boundaries.md`: self-host your own root, or commercial certs). The
vendor's bridge binary ships with the commercial channel; the wire protocol is
fully visible in the public relay crate, so a compatible bridge is
self-implementable. Runtime behaviors an agent should expect on this channel:
```yaml
relay_channel_semantics:
  busy:            {status: busy, retry_after_secs}          # node has the request, still working past the window
  node_not_polling:{status: node_not_polling, retry_after_secs} # node offline/restarting; retry shortly
  ack_window:      ECK_SHIM_ACK_WINDOW_SECS (default 120)
  no_pickup_bail:  ECK_SHIM_NOPICKUP_SECS   (default 25)
```
Both failure shapes are structured `isError` MCP results, not transport
errors — back off and retry rather than declaring the mesh down.

## F. REST instead of MCP
Every UI capability is plain REST under `/api` with JWT auth (login endpoint,
role-gated admin routes) — inventory in `45-http-surface.md`. MCP is the
preferred agent path because masking, token pivots, and structured results are
enforced there.
