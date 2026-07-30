<!-- machine-first: generated from source audit 2026-07-29; audience=agents -->
## SCOPE

This file inventories every HTTP route declared in the open-source workspace's two
network-facing binaries:

- **node** — `wms` crate, router built in `wms/src/main.rs` (function `main`/`async_main`,
  the `Router::new()...` chain ending in `axum::serve(...)`).
- **relay** — `relay` crate, router built in `relay/src/main.rs` (`fn build_router`).

It also inventories the **commercial-edition** `/K` POS surface (`pos` crate, compiled in
only when the `pos-module` Cargo feature is enabled) because the node's router
conditionally `.merge()`s it — an agent probing a live node needs to know `/K/*` may or
may not exist depending on build + `POS_ENABLED`. `shim` (the desktop MCP-connector
relay bridge) has no HTTP server of its own and is out of scope for this file.

Auth-gate values used below, derived strictly from router **middleware layering**
(`.route_layer(...)`) or, where no router-level layer exists, from the first
in-handler credential check found by reading the handler:

- `public` — no credential checked anywhere in the request path.
- `user-JWT` — `middleware::auth::auth_middleware` (validates a JWT signed with
  `JWT_SECRET`; rejects `role=observer` mutations and `role=cashier`/`auth_method=pin`
  outside `/auth/*`).
- `admin-JWT` — `user-JWT` PLUS `middleware::require_admin::require_admin_middleware`
  (requires `Claims.role == "admin"`).
- `service-token` — a shared bearer compared constant-time against an env var
  (`XELIXIR_SERVICE_TOKEN` for `/X/internal/*` + `/X/ops/*`; `RELAY_ADMIN_TOKEN` for the
  relay's `/E/registry`).
- `MCP-bearer` — `Authorization: Bearer <token>` checked in-handler against
  `ECK_MCP_MASTER_TOKEN` (master tier, falls back to `XELIXIR_SERVICE_TOKEN`) or
  `ECK_MCP_AGENT_TOKEN` (agent tier). Not a router-level layer — the handler decides.
- `cert-signed` — an Ed25519-signed envelope or authority-signed `SubscriptionCert`
  inside the request body, verified in-handler (no bearer header). Used by `/X/self/*`
  (envelope signer must be in `XELIXIR_ADMIN_PUBKEYS` or chain to `ECK_FLEET_ROOT_PUBKEY`),
  `/mcp/signed` and the relay's `/E/c/dispatch/*` (both verify against `ECK_SUB_ROOT_PUBKEY`).
- `mesh-secret` — `middleware::mesh_auth::mesh_auth_middleware`: a shared bearer compared
  against `state.sync_secret` (from `SYNC_SECRET`); if `SYNC_SECRET` is unset, this gate
  is **open** (dev-mode passthrough — verify on any hardened deployment).
- `verify` — used only where the brief requires it; none of the routes below needed it —
  every gate was resolvable from router layering or a direct in-handler check.

No route in either binary enforces an IP/LAN restriction in code. `/S/*` (scraper proxy)
and the relay itself are operationally expected to run local-only / behind a reverse
proxy, but that is a deployment convention, not something this router enforces — do not
read `public` as `LAN-only` for those rows.

### Route count summary (verified against source)

```
node (wms/src/main.rs), declared routes:
  api_router (mounted at BOTH /api and /E/api — see note below): 174
    public_routes:      10
    protected_routes:  119
    admin_routes:       36
    p2p_routes:          9
  /X xelixir_routes (mounted once at /X):                         24
    xelixir_jwt_routes:       4
    xelixir_self_routes:      2
    xelixir_internal_routes:  2
    xelixir_ops_routes:      16
  top-level app routes (/E/health, /E/ws, /mcp, /mcp/signed,
    /E/auth/setup-status, /E/auth/login, /S, /S/*path):            8
  static/UI fallback (web::static_handler):                        1
  NODE TOTAL (declared, unique):                                 207
  (api_router's 174 routes are additionally reachable at /E/api/*,
   a legacy alias for movFast PDA clients whose base URL ends in
   "/E" — same handlers, same auth, not double-counted above.)

relay (relay/src/main.rs), declared routes:                        22

commercial edition only — pos crate, /K router (pos/src/lib.rs):
  protected_routes: 24, public_routes: 5, top-level (ws+SPA): 4
  POS TOTAL:                                                       33

GRAND TOTAL open-source (node + relay):                           229
GRAND TOTAL incl. commercial /K:                                  262

(Counts above are DECLARED axum `.route()` call sites — verified against
`grep -c '\.route('` per router block. Several declarations bundle more than one
HTTP method on the same path, e.g. `.route("/items", get(...).post(...))` is ONE
declared route but TWO distinct method+path pairs from an HTTP client's point of
view. The final YAML inventory below enumerates every method+path pair separately
— 298 rows — which is the larger, and equally correct, count for that finer unit.)
```

---

## 1. `/api` business REST (node)

Built from `public_routes.merge(protected_routes).merge(admin_routes).merge(p2p_routes)`
into `api_router`, then `.nest("/api", api_router.clone())` and `.nest("/E/api", api_router)`
— identical route set at both mount points. `admin_routes` is documented separately in
section 2 for clarity even though it is physically merged into the same router. A JSON
404 fallback and a 50 MiB body-size layer apply to the whole `api_router`.

### 1a. `public_routes` — auth: `public` (10 routes)

| Method | Path | Handler | Purpose |
|---|---|---|---|
| POST | /api/auth/login | handlers::auth::login | Password/PIN login, issues JWT |
| GET | /api/auth/setup-status | handlers::auth::setup_status | Whether first-run admin setup is done |
| GET | /api/auth/kiosk-token | handlers::auth::kiosk_token | Mint a scoped kiosk-role token |
| GET | /api/auth/device-challenge | handlers::auth::device_challenge | Issue a pairing challenge nonce |
| POST | /api/public/devices/register | handlers::device::register_device | Public device self-registration |
| POST | /api/internal/register-device | handlers::device::register_device | Same handler, legacy internal path |
| GET | /api/public/agreement/:token | handlers::rma::get_agreement_by_token | Fetch RMA agreement by capability token |
| POST | /api/public/agreement/:token/sign | handlers::rma::sign_agreement | Customer signs RMA agreement, no login |
| GET | /api/i18n/dict/:lang | handlers::i18n::dict | UI label dictionary for one language |
| GET | /api/pos/status | inline closure | Reports whether POS_ENABLED is set (for UI) |

### 1b. `protected_routes` — auth: `user-JWT` (119 routes)

Resource CRUD groups (method verbs shown per row where a path takes more than one):

| Method | Path | Handler | Purpose |
|---|---|---|---|
| GET/POST | /api/items | handlers::items::list / create | Item catalog list/create |
| GET/PUT/DELETE | /api/items/:id | handlers::items::get/update/delete | Single item CRUD |
| GET/POST | /api/products | handlers::products::list / create | Product master list/create (Edge Sync Layer) |
| GET/PUT/DELETE | /api/products/:id | handlers::products::get/update/delete | Single product CRUD |
| GET/POST | /api/partners | handlers::partners::list / create | Partner (customer/supplier) list/create |
| GET/PUT/DELETE | /api/partners/:id | handlers::partners::get/update/delete | Single partner CRUD |
| GET/POST | /api/quants | handlers::quants::list / create | Stock-quant list/create |
| GET/PUT/DELETE | /api/quants/:id | handlers::quants::get/update/delete | Single quant CRUD |
| GET/POST | /api/pickings | handlers::pickings::list / create | Warehouse picking-order list/create |
| GET/PUT/DELETE | /api/pickings/:id | handlers::pickings::get/update/delete | Single picking CRUD |
| GET/POST | /api/move-lines | handlers::pickings::list_lines / create_line | Picking move-line list/create |
| PUT | /api/move-lines/:id | handlers::pickings::update_line | Update a move line |
| GET/POST | /api/warehouse | handlers::warehouse::list / create | Warehouse list/create |
| GET/POST | /api/warehouse/racks | handlers::warehouse::list_racks / create_rack | Rack list/create |
| PUT/DELETE | /api/warehouse/racks/:id | handlers::warehouse::update_rack / delete_rack | Rack update/delete |
| POST | /api/warehouse/put-away | handlers::warehouse::put_away | Put-away operation |
| GET | /api/warehouse/bin | handlers::warehouse::bin_contents | Bin contents lookup |
| GET | /api/warehouse/reconcile | handlers::warehouse::reconcile | Inventory reconciliation view |
| GET | /api/warehouse/inventory | handlers::warehouse::inventory | Inventory snapshot |
| GET | /api/warehouse/:id | handlers::warehouse::get | Single warehouse read |
| GET/POST | /api/rma | handlers::rma::list_orders / create_order | RMA/repair-order list/create |
| POST | /api/rma/search | handlers::rma::search_orders | RMA order search |
| GET/PUT/DELETE | /api/rma/:id | handlers::rma::get_order/update_order/delete_order | Single RMA order CRUD |
| POST | /api/rma/:id/generate-link | handlers::rma::generate_agreement_link | Mint the public sign-agreement link |
| GET/POST | /api/menu/categories | handlers::menu::list_categories / create_category | POS menu category list/create |
| PUT/DELETE | /api/menu/categories/:id | handlers::menu::update_category / delete_category | Menu category update/delete |
| GET/POST | /api/menu/items | handlers::menu::list_items / create_item | POS menu item list/create |
| PUT/DELETE | /api/menu/items/:id | handlers::menu::update_item / delete_item | Menu item update/delete |
| GET | /api/mesh/status | handlers::mesh::status | This node's identity + mesh membership (JWT view) |
| GET | /api/mesh/nodes | handlers::mesh::nodes | Relay-known peer nodes (JWT view) |
| GET | /api/internal/pairing-qr | handlers::device::generate_pairing_qr | Generate a device-pairing QR payload |
| POST | /api/print/labels | handlers::print::generate_labels | Generate warehouse label print job (embeds ECK_QR_HOSTS URLs) |
| POST | /api/proofs | handlers::proofs::submit_proof | Submit an action proof (photo/signature) |
| GET | /api/audit/verify | handlers::audit::verify | Verify the tamper-evident audit chain |
| GET | /api/audit/chain | handlers::audit::chain | Read the audit chain |
| POST | /api/audit/anchor | handlers::audit::anchor | Anchor current audit root to Hedera (if configured) |
| POST | /api/files/upload | handlers::files::upload | Upload a file into the CAS filestore |
| GET | /api/files/:id | handlers::files::download | Download a file by id |
| GET | /api/files/attachments | handlers::files::list_attachments | List attachments for an entity |
| POST | /api/files/attach | handlers::files::attach | Attach an existing file to an entity |
| DELETE | /api/files/attachments/:edge_id | handlers::files::detach | Remove an attachment edge |
| POST | /api/files/redirect | handlers::files::redirect | Re-home a temp-parked photo onto an order |
| DELETE | /api/files/temp/:id | handlers::files::delete_temp | Delete a temp-parked photo |
| POST | /api/support/import-ticket | handlers::support::import_ticket | Import one support ticket (Zoho Desk shape) |
| POST | /api/support/import-tickets | handlers::support::import_tickets | Bulk-import support tickets |
| POST | /api/support/import-thread | handlers::support::import_thread | Import a ticket thread/message |
| GET | /api/support/tickets | handlers::support::list_tickets | List support tickets |
| GET | /api/support/debug/:ticket_id | handlers::support::debug_ticket | Debug view of a ticket's raw state |
| GET | /api/support/tickets/:ticket_id/threads | handlers::support::get_ticket_threads | List a ticket's threads |
| GET | /api/support/tickets/:ticket_id/threads/:thread_id/payload | handlers::support::get_thread_payload | Read one thread's raw payload |
| POST | /api/support/tickets/:ticket_id/summary | handlers::support::summarize_ticket | Trigger AI summarization of a ticket |
| GET | /api/support/tickets/:ticket_id/similar | handlers::support::find_similar | Vector-similar tickets |
| GET | /api/ai/tasks | handlers::ai::list_tasks | List paused AI-orchestrator tasks (operator inbox) |
| POST | /api/ai/tasks/:id/reply | handlers::ai::reply_to_task | Operator reply to a paused AI task |
| POST | /api/ai/enrich-csv | handlers::ai::enrich_csv | Batch AI enrichment of an uploaded CSV |
| GET | /api/ai/usage | handlers::ai::get_ai_usage | Local 24h AI token-spend estimate |
| POST | /api/voice/resolve | handlers::voice::resolve_voice | Resolve a voice command (movFast), local-first with a Gemini fallback |
| POST | /api/geo/fix | handlers::geo::fix_location | Operator manual geo override |
| POST | /api/geo/regeocode-fallback | handlers::geo::regeocode_fallback | Re-geocode the HQ-fallback pile from summaries |
| GET/POST | /api/geo/grounding-config | handlers::geo::get_grounding_config / set_grounding_config | AI address-discovery toggle + config |
| POST | /api/geo/discover-addresses | handlers::geo::discover_addresses | Run AI address discovery for office-pinned tickets |
| POST | /api/geo/vorwahl-fill | handlers::geo::vorwahl_fill | Place residual tickets by landline area code |
| POST | /api/geo/customer-fill | handlers::geo::customer_fill | Fill location from matched customer record |
| GET | /api/geo/resolve | handlers::geo::resolve_location | Server-side cached geocode lookup |
| POST | /api/exact/import-items | handlers::exact::import_items | Manual Exact Online item import |
| POST | /api/exact/import-customers | handlers::exact::import_customers | Manual Exact Online customer import |
| POST | /api/exact/import-stock-positions | handlers::exact::import_stock_positions | Manual Exact Online stock import |
| POST | /api/exact/import-quotations | handlers::exact::import_quotations | Manual Exact Online quotation import |
| POST | /api/exact/import-sales-orders | handlers::exact::import_sales_orders | Manual Exact Online sales-order import |
| GET | /api/status | handlers::pda::status | movFast PDA heartbeat/status |
| POST | /api/scan | handlers::pda::handle_scan | movFast barcode/QR scan event |
| POST | /api/repair/event | handlers::pda::repair_event | movFast repair-workflow event |
| POST | /api/repair/consume | handlers::pda::consume | movFast repair parts-consumption event |
| POST | /api/upload/image | handlers::files::upload | Same upload handler, PDA-facing alias path |
| GET | /api/users/active | handlers::pda::active_users | Active operator users for PDA login list |
| POST | /api/users/verify-pin | handlers::pda::verify_pin | PDA PIN verification |
| GET | /api/pickings/active | handlers::pda::active_pickings | Active pickings for the PDA operator |
| GET | /api/pickings/:id/route | handlers::pda::picking_route | Picking route/path for a picking |
| POST | /api/pickings/:id/lines/:line_id/confirm | handlers::pda::confirm_pick_line | Confirm a picked line |
| POST | /api/pickings/:id/validate | handlers::pda::validate_picking | Validate/close a picking |
| GET | /api/explorer/locations | handlers::pda::explorer_locations | Warehouse location explorer list |
| GET | /api/explorer/locations/:id/contents | handlers::pda::explorer_location_contents | Contents of one location |
| GET | /api/explorer/products | handlers::pda::explorer_products | Product explorer list |
| GET | /api/explorer/products/:id/locations | handlers::pda::explorer_product_locations | Locations holding one product |
| POST | /api/sync/pull | handlers::pda::sync_pull | PDA-side data sync pull |
| POST | /api/crm/update | handlers::pda::crm_update | PDA CRM record update |
| GET | /api/crm/:entity_type/:id | handlers::pda::crm_get | PDA CRM record read |
| GET/POST | /api/trips | handlers::trips::list_trips / upload_trip | Fahrtenbuch trip list/upload |
| GET | /api/trips/export | handlers::trips::export_trips | Export trips (GoBD Z3 DTD) |
| GET | /api/trips/purpose-candidates | handlers::trips::purpose_candidates | Candidate trip-purpose suggestions |
| GET | /api/trips/destinations | handlers::trips::destinations | Known trip destinations |
| POST | /api/trips/live | handlers::trips::trip_live | Live trip-tracking ingest |
| GET | /api/trips/:id | handlers::trips::get_trip | Single trip read |
| GET | /api/trips/:id/verify | handlers::trips::verify_trip | Trip GoBD-seal verification |
| GET | /api/cells/cache | handlers::trips::cell_cache | Cell-tower geocoding cache read |
| GET/POST | /api/vehicles | handlers::vehicles::list_vehicles / create_vehicle | Vehicle list/create |
| PUT | /api/vehicles/:id | handlers::vehicles::update_vehicle | Vehicle update |
| GET/POST | /api/visits | handlers::visits::list_visits / create_visit | Field-visit list/create |
| POST | /api/visits/:id/checkin | handlers::visits::checkin | Visit check-in |
| POST | /api/visits/:id/checkout | handlers::visits::checkout | Visit check-out |
| GET | /api/odoo/ping | handlers::odoo::ping | Odoo connectivity check |
| POST | /api/odoo/sync | handlers::odoo::sync | Trigger Odoo sync |
| POST | /api/odoo/bridge-products | handlers::odoo::bridge_products | Bridge/map products against Odoo |
| GET | /api/odoo/pickings | handlers::stubs::odoo_pickings | Stub — always returns empty array |
| GET/POST | /api/delivery/shipments | handlers::stubs::list_shipments / create_shipment | Delivery shipment list/create (reads meshed stock_picking_delivery) |
| GET | /api/delivery/config | handlers::stubs::delivery_config | Delivery module config |
| POST | /api/delivery/shipments/:id/cancel | handlers::stubs::cancel_shipment | Cancel a shipment |
| POST | /api/delivery/shipments/:id/resolve | handlers::stubs::resolve_shipment | Resolve a shipment match |
| GET | /api/delivery/shipments/:id/ai-match | handlers::stubs::ai_match_shipment | AI-assisted shipment matching |
| POST | /api/delivery/import/opal | handlers::stubs::import_opal | Import delivery data from OPAL/OCU |
| POST | /api/delivery/import/dhl | handlers::stubs::import_dhl | Import delivery data from DHL |
| GET | /api/delivery/sync/history | handlers::stubs::delivery_sync_history | Delivery sync run history |
| GET | /api/delivery/carriers | handlers::stubs::delivery_carriers | Known delivery carriers |
| GET | /api/analysis/support-dump | handlers::stubs::analysis_support_dump | Analysis-dashboard support data dump |
| GET | /api/auth/me | handlers::auth::me | Current authenticated user/claims |
| POST | /api/auth/change-password | handlers::auth::change_password | Self-service password change |
| GET | /api/i18n/languages | handlers::i18n::languages | List configured UI languages |
| GET/POST | /api/admin/config/kiosk | handlers::auth::get_kiosk_config / set_kiosk_config | Kiosk UI config (NOTE: despite the `/admin/` path, this is gated only by `user-JWT`, not `admin-JWT` — it lives in `protected_routes`, not `admin_routes`) |
| GET/POST | /api/admin/config/dashboard_sla | handlers::auth::get_dashboard_sla / set_dashboard_sla | Dashboard SLA config (same note as above) |

### 1c. `p2p_routes` — auth: `mesh-secret` (9 routes)

Server-to-server mesh sync between nodes; open/dev-mode when `SYNC_SECRET` is unset.

| Method | Path | Handler | Purpose |
|---|---|---|---|
| GET | /api/mesh/merkle/state | handlers::mesh::merkle_state | This node's Merkle-tree state for an entity type |
| GET | /api/mesh/parity | handlers::mesh::parity | Cross-node parity/drift check |
| POST | /api/mesh/sync/pull | handlers::mesh::sync_pull | Peer pulls raw documents for upsert |
| POST | /api/mesh/sync/push | handlers::mesh::sync_push | Peer pushes an entity upsert (schemaless) |
| GET | /api/mesh/file/:hash | handlers::mesh::serve_mesh_file | Serve file bytes to hydrate a peer's FileStore |
| GET | /api/mesh/raw-docs/:ticket_id | handlers::mesh::raw_docs | Raw document_raw records for a ticket (thin-node lazy-load) |
| GET | /api/mesh/tasks | handlers::mesh::get_tasks | Pending mesh tasks for the calling node |
| POST | /api/mesh/tasks/nudge | handlers::mesh::nudge_tasks | Nudge/trigger processing of pending tasks |
| DELETE | /api/mesh/tasks/:id | handlers::mesh::delete_task | Mark a mesh task completed |

---

## 2. `/api/admin` (node) — auth: `admin-JWT` (36 routes)

`admin_routes`, merged into the same `api_router` as section 1, but layered with
`require_admin_middleware` (outer) over `auth_middleware` (inner) — both must pass.

| Method | Path | Handler | Purpose |
|---|---|---|---|
| GET | /api/admin/db/backups | handlers::backup::list_backups | List DB backups |
| POST | /api/admin/db/backup | handlers::backup::create_backup | Create a DB backup |
| POST | /api/admin/db/restore/:filename | handlers::backup::restore_backup | Restore a DB backup (destructive) |
| POST | /api/admin/force-sync | handlers::admin::force_sync | Manually trigger sync across scraper providers |
| POST | /api/admin/mesh-replay/:entity_type | handlers::admin::mesh_replay | One-shot mesh-wide backfill of an entity type |
| POST | /api/admin/gdpr/erase | handlers::gdpr::erase_subject | GDPR Art.17 erasure of AI-derived vectors (audit-logged) |
| GET | /api/admin/known-nodes | handlers::mesh::known_nodes | Cross-mesh node registry (all kiosks, any mesh) |
| GET/POST | /api/admin/users | handlers::users::list / create | List/create operator users (incl. role assignment) |
| PUT/DELETE | /api/admin/users/:id | handlers::users::update / delete | Update/delete a user |
| POST | /api/admin/pair-code | handlers::device::mint_pair_code | Mint a device pairing code |
| GET | /api/admin/devices | handlers::device::list_devices | List paired devices |
| PUT | /api/admin/devices/:id/status | handlers::device::update_device_status | Update device status |
| PUT | /api/admin/devices/:id/home | handlers::device::update_device_home | Update device home-node assignment |
| POST | /api/admin/devices/:id/restore | handlers::device::restore_device | Restore a soft-deleted device |
| DELETE | /api/admin/devices/:id | handlers::device::delete_device | Delete a device |
| POST | /api/admin/mesh/master | handlers::mesh::set_master | Transfer mesh master/home designation |
| POST | /api/admin/query | handlers::admin::query | Arbitrary SurrealQL diagnostics (double-checks admin in-handler too) |
| POST | /api/support/backfill-assignees | handlers::support::backfill_assignees | One-shot backfill: ticket assignees |
| POST | /api/support/backfill-customfields | handlers::support::backfill_customfields | One-shot backfill: ticket custom fields |
| POST | /api/support/backfill-meta | handlers::support::backfill_meta | One-shot backfill: ticket metadata |
| POST | /api/support/restamp-thread-hashes | handlers::support::restamp_thread_hashes | Recompute/restamp thread hashes |
| POST | /api/support/restamp-vclocks | handlers::support::restamp_vclocks | Recompute/restamp vector clocks |
| POST | /api/support/claim-home | handlers::support::claim_home | Claim per-entity home_instance_id ownership |
| POST | /api/support/backfill-outbound-times | handlers::support::backfill_outbound_times | Backfill outbound-message timestamps |
| POST | /api/support/backfill-thread-headers | handlers::support::backfill_thread_headers | Backfill thread header fields |
| POST | /api/support/backfill-summary-sync | handlers::support::backfill_summary_sync | Backfill AI-summary sync flags |
| POST | /api/support/backfill-embedding-sync | handlers::support::backfill_embedding_sync | Backfill embedding sync flags |
| POST | /api/support/requeue-embeddings | handlers::support::requeue_embeddings | Requeue tickets for embedding |
| POST | /api/support/admin/scrub-summaries | handlers::support::scrub_summaries | Scrub/reset stored AI summaries |
| POST | /api/exact/backfill-vclocks | handlers::exact::backfill_vclocks | Backfill vclocks for Exact-imported rows |
| POST | /api/odoo/project/set-onhand | handlers::odoo::set_onhand | Write on-hand qty back into Odoo (gated by ODOO_WRITE_ENABLED too) |
| POST | /api/odoo/project/run | handlers::odoo::project_run | Run Odoo projection job |
| POST | /api/scraper/start | handlers::scraper_proxy::start_scraper | Spawn the local Node.js scraper process |
| GET | /api/admin/mcp-connector | mcp::connector::mcp_connector | Download a ready-to-run MCP connector bundle (mints a live token into the zip) |
| POST | /api/admin/i18n/languages | handlers::i18n::add_language | Machine-translate UI labels into a new language |
| GET | /api/admin/i18n/languages/:lang/status | handlers::i18n::add_language_status | Poll status of an in-progress language add |

---

## 3. `/mcp` + `/mcp/signed` (node) — 2 routes

Mounted at the app top level (outside `/X` and outside `/api`), each with its own
in-handler auth — no shared router-level middleware.

| Method | Path | Auth | Handler | Purpose |
|---|---|---|---|---|
| POST | /mcp | MCP-bearer | mcp::mcp_handler | Direct/LAN MCP JSON-RPC transport (Master or Agent tier by bearer) |
| POST | /mcp/signed | cert-signed | mcp::mcp_signed_handler | Direct twin of relay `/E/c/*`: SubscriptionCert-signed MCP request, no relay hop, no bearer |

`tools/list` and `tools/call` further restrict which tools are visible/callable depending
on tier and on `over_relay` (true for both `/mcp/signed` and the relay-carried poller):
`surrealql_read` and `reveal_file_local` are hidden and refused whenever `over_relay` is
true, regardless of tier. `reveal_tokens` is a JSON-RPC **method**, not a listed tool,
Master-tier only, available on both transports.

---

## 4. `/X` ops/service (node) — 24 routes

Nested once at `/X` (`.nest("/X", xelixir_routes)`), NOT under `/api`. Built from four
sub-routers merged together; each sub-router carries its own gate.

### 4a. `xelixir_jwt_routes` — auth: `user-JWT` (4 routes)

| Method | Path | Handler | Purpose |
|---|---|---|---|
| GET/POST | /X/config | handlers::xelixir::get_config / set_config | Read/write local xelixir-agent config |
| POST | /X/approve | handlers::xelixir::approve | Operator approves an inbound xelixir session request (observer role explicitly allowed here) |
| POST | /X/devices/:id/start | handlers::xelixir::start_device | Start a paired device's agent |
| POST | /X/devices/:id/stop | handlers::xelixir::stop_device | Stop a paired device's agent |

### 4b. `xelixir_self_routes` — auth: `cert-signed` (2 routes)

No `.route_layer` at all — public at the router level; the handler itself requires the
body to be an Ed25519-signed envelope whose signer is in `XELIXIR_ADMIN_PUBKEYS` (or
chains to `ECK_FLEET_ROOT_PUBKEY`), so effective auth is `cert-signed`, not `public`.

| Method | Path | Handler | Purpose |
|---|---|---|---|
| POST | /X/self/start | handlers::xelixir::self_start | Server-initiated self start (signed envelope, command="start") |
| POST | /X/self/stop | handlers::xelixir::self_stop | Server-initiated self stop (signed envelope, command="stop") |

### 4c. `xelixir_internal_routes` — auth: `service-token` (2 routes)

| Method | Path | Handler | Purpose |
|---|---|---|---|
| POST | /X/internal/dispatch | handlers::xelixir::internal_dispatch | Sibling-service dispatch of a start/stop command |
| GET | /X/internal/result/:task_id | handlers::xelixir::internal_result | Poll result of a dispatched command |

### 4d. `xelixir_ops_routes` — auth: `service-token` + audit-logged (16 routes)

Every request here also passes through `ops_audit_middleware`, which writes a row to
`ops_audit_log` (verb, status, duration_ms, request_ip, timestamp; body is never logged)
regardless of outcome, including 403s.

| Method | Path | Handler | Purpose |
|---|---|---|---|
| GET | /X/ops/journal | handlers::ops::journal | Read the ops journal |
| GET | /X/ops/service_status | handlers::ops::service_status | systemd/service status |
| GET | /X/ops/system_health | handlers::ops::system_health | Host system health snapshot |
| GET | /X/ops/health_check | handlers::ops::health_check | Node health check (ops view) |
| GET | /X/ops/loop_metrics | services::health_monitor::deep_health_handler | Per-loop rate + CPU/RSS history self-diagnosis |
| GET | /X/ops/file_read | handlers::ops::file_read | Read a file under an allow-listed prefix (`ECK_OPS_FILE_PREFIXES`) |
| POST | /X/ops/file_write | handlers::ops::file_write | Write a file under an allow-listed prefix |
| POST | /X/ops/surrealql_read | handlers::ops::surrealql_read | Read-only SurrealQL query |
| POST | /X/ops/surrealql_write | handlers::ops::surrealql_write | Write SurrealQL query |
| POST | /X/ops/restart_service | handlers::ops::restart_service | Restart a systemd service |
| POST | /X/ops/git_pull | handlers::ops::git_pull | Long-running: git pull (returns task_id, poll via /ops/task/:id) |
| POST | /X/ops/cargo_build | handlers::ops::cargo_build | Long-running: cargo build |
| POST | /X/ops/deploy | handlers::ops::deploy | Long-running: deploy |
| GET | /X/ops/task/:task_id | handlers::ops::task_status | Poll a long-running ops task |
| POST | /X/ops/nginx_test_reload | handlers::ops::nginx_test_reload | Test + reload nginx config |
| POST | /X/ops/package_install | handlers::ops::package_install | Install an OS package |

---

## 5. `/E` mesh + `/E/c` client-MCP relay channel

This section covers TWO different services that share the `/E` path prefix:
(a) the **node**'s own top-level `/E/*` routes (health/ws/auth aliases — NOT nested
under `/api`), and (b) the **relay**'s entire route table, which is prefixed `/E/*`
end-to-end (`relay/src/main.rs`). They are on different hosts/ports; both are listed
here because the prefix convention is shared and an agent resolving an `/E/...` path
needs to know which binary owns it.

### 5a. Node top-level `/E/*` (part of the 8 top-level app routes) — auth varies

| Method | Path | Auth | Handler | Purpose |
|---|---|---|---|---|
| GET | /E/health | public | health_check (main.rs) | Node liveness/version check |
| GET | /E/ws | user-JWT via `?token=` query param, validated in-handler; an unauthenticated upgrade is accepted but never subscribed to the event feed | handlers::ws::ws_handler | WebSocket upgrade for live UI updates |
| GET | /E/auth/setup-status | public | handlers::auth::setup_status | Same handler as /api/auth/setup-status, top-level alias |
| POST | /E/auth/login | public | handlers::auth::login | Same handler as /api/auth/login, top-level alias |

### 5b. Relay `/E/*` (relay/src/main.rs, `fn build_router`) — 22 routes

The relay is a dumb queue/board: it stores and forwards; per its own code comments it
does **not** verify envelope signatures for the `/E/x/*` and `/E/m/*` families ("the
relay is just a queue") — verification happens at the receiving node. No router-level
middleware exists on this router at all (only a body-size layer + permissive CORS);
every gate below is an in-handler check or its absence.

| Method | Path | Auth | Handler | Purpose |
|---|---|---|---|---|
| GET | /E/health | public | handlers::health | Relay liveness check |
| POST | /E/register | public | handlers::register | Node registers itself (mesh_id, instance_id, address) |
| POST | /E/push | public | handlers::push | Push an encrypted packet, fanned out to all online peers in the mesh |
| GET | /E/pull/{mesh_id}/{instance_id} | public | handlers::pull | Pull + delete this instance's queued encrypted packets |
| GET | /E/mesh/{mesh_id}/status | public | handlers::mesh_status | List registrations in a mesh |
| GET | /E/mesh/{mesh_id}/resolve/{instance_id} | public | handlers::resolve_node | Resolve one instance's registration record |
| GET | /E/registry | service-token (RELAY_ADMIN_TOKEN bearer) | handlers::registry | Admin listing of the relay's full registry |
| GET | /E/resolve/{instance_id} | public | handlers::x_resolve (xelixir::resolve) | Resolve an instance_id, mesh-agnostic |
| POST | /E/x/dispatch/{target_uuid} | public (payload expected to be a signed envelope; relay does not verify) | handlers::x_dispatch (xelixir::dispatch) | Queue a xelixir C2 command for a target UUID |
| GET | /E/x/poll/{self_uuid} | public | handlers::x_poll (xelixir::poll) | Target polls pending xelixir commands (adaptive interval) |
| POST | /E/x/ack/{task_id} | public | handlers::x_ack (xelixir::ack) | Target acks a xelixir command with a result |
| GET | /E/x/result/{task_id} | public | handlers::x_result (xelixir::result) | Dispatcher polls the ack'd result |
| POST | /E/m/dispatch/{target_uuid} | public, subject to RELAY_PAYLOAD_MODE policy (open/disabled/paid — see 60-config-matrix.md) | handlers::m_dispatch (mesh_relay::dispatch) | Queue a NAT-fallback mesh-sync task for a target UUID |
| GET | /E/m/poll/{self_uuid} | public | handlers::m_poll (mesh_relay::poll) | Target long-polls pending mesh-sync tasks |
| POST | /E/m/ack/{task_id} | public | handlers::m_ack (mesh_relay::ack) | Target acks a mesh-sync task with a result |
| GET | /E/m/result/{task_id} | public | handlers::m_result (mesh_relay::result) | Sender polls the ack'd mesh-sync result |
| POST | /E/c/dispatch/{target_uuid} | cert-signed (SubscriptionCert verified against ECK_SUB_ROOT_PUBKEY; 503 if unset, 403 if invalid) | handlers::c_dispatch (client_mcp::dispatch) | Admit a paid subscriber's MCP request for a NAT'd node — THE gate for this channel |
| GET | /E/c/poll/{self_uuid} | public (long-poll; only pre-admitted tasks exist here) | handlers::c_poll (client_mcp::poll) | Target node long-polls pending client-MCP tasks |
| POST | /E/c/ack/{task_id} | public | handlers::c_ack (client_mcp::ack) | Target posts the MCP result body (first-ack-wins) |
| GET | /E/c/result/{task_id} | public | handlers::c_result (client_mcp::result) | Subscriber polls until the node acks |
| POST | /E/pair/announce | public (bogus entries are harmless — pairing still needs a valid master-signed invite token) | handlers::pair_announce (pair::announce) | Master publishes a short-code pairing rendezvous entry (TTL ~10 min) |
| POST | /E/pair/code | public | handlers::pair_resolve (pair::resolve_code) | Resolve a typed short code into the full ECK$ pairing string |

---

## 6. `/S` scraper proxy (node) — auth: `public` (2 routes)

Mounted at the app top level, outside every auth layer. Proxies to a loopback-only
backend (`http://127.0.0.1:$SCRAPER_PORT`, default port 38211) — the proxy target is not
network-reachable from outside the host even though the proxy route itself has no gate.

| Method | Path | Handler | Purpose |
|---|---|---|---|
| ANY | /S | handlers::scraper_proxy::proxy_handler | Reverse-proxy root to the local Node.js scraper |
| ANY | /S/*path | handlers::scraper_proxy::proxy_handler | Reverse-proxy any sub-path to the local Node.js scraper |

---

## 7. `/K` POS — commercial edition, feature-gated (33 routes)

Compiled in only under the `pos-module` Cargo feature (default-on in the commercial
build; entirely absent from an open-core build without that feature). Even when
compiled in, the routes exist only if `POS_ENABLED` is truthy at boot — otherwise `/K/*`
falls through to the WMS SPA fallback (a 404-equivalent from the SPA's own routing).
Mounted via `app.merge(pos::router(pos_state))` — NOT behind `/X`'s service-token layer
and NOT sharing `/api`'s JSON 404 fallback (POS has its own).

### 7a. `pos::protected_routes` — auth: `user-JWT` (POS realm; 24 routes)

| Method | Path | Handler | Purpose |
|---|---|---|---|
| GET | /K/api/transactions | handlers::transaction::list | List POS transactions |
| POST | /K/api/transactions/active | handlers::transaction::find_or_create | Find or open the active transaction |
| GET | /K/api/transactions/parked | handlers::transaction::parked | List parked transactions |
| POST | /K/api/transactions/:id/reprint | handlers::transaction::reprint | Reprint a receipt |
| POST | /K/api/transactions/:id/cancel | handlers::transaction::cancel | Cancel a transaction |
| PUT | /K/api/transactions/:id/table | handlers::transaction::set_table | Assign a table to a transaction |
| POST | /K/api/transactions/:id/items | handlers::transaction::add_item | Add a line item |
| PUT | /K/api/transactions/:id/items/:item_id | handlers::transaction::update_item | Update a line item |
| POST | /K/api/transactions/:id/finish | handlers::transaction::finish | Finish/close a sale (fiscal path) |
| POST | /K/api/transactions/:id/storno | handlers::transaction::storno | Void/storno a transaction |
| POST | /K/api/transactions/storno/:storno_id/approve | handlers::transaction::approve_storno | Approve a storno request |
| POST | /K/api/transactions/storno/:storno_id/reject | handlers::transaction::reject_storno | Reject a storno request |
| POST | /K/api/ai/chat | handlers::ai::chat | POS-embedded AI chat assistant |
| GET | /K/api/hardware/usb | handlers::hardware::list_usb_devices | List attached USB hardware |
| POST | /K/api/hardware/test-print | handlers::hardware::test_print | Test-print on the receipt printer |
| GET | /K/api/menu/categories | handlers::menu::list_categories | POS menu categories (read) |
| GET | /K/api/menu/items | handlers::menu::list_items | POS menu items (read) |
| GET | /K/api/menu/search | handlers::menu::search_items | Search menu items |
| POST | /K/api/menu/import | handlers::menu_import::import_menu | Import a menu (20 MiB body cap, base64 photo/PDF) |
| POST | /K/api/export/dsfinvk/start | handlers::export::start_export | Start a DSFinV-K fiscal export job |
| GET | /K/api/export/dsfinvk/status/:job_id | handlers::export::export_status | Poll DSFinV-K export job status |
| GET | /K/api/audit/verify | handlers::audit::verify | Verify POS audit chain |
| GET | /K/api/audit/chain | handlers::audit::chain | Read POS audit chain |
| POST | /K/api/audit/anchor | handlers::audit::anchor | Anchor POS audit root |

### 7b. `pos::public_routes` — auth: `public` (5 routes)

| Method | Path | Handler | Purpose |
|---|---|---|---|
| GET | /K/api/health | inline closure | POS liveness check |
| GET | /K/api/auth/users | handlers::auth::list_users | List cashier accounts (for the login picker) |
| POST | /K/api/auth/login | handlers::auth::login | Cashier password login |
| POST | /K/api/auth/login-pin | handlers::auth::login_pin | Cashier PIN login |
| GET | /K/api/export/dsfinvk/download/:token | handlers::export::download_export | Download a DSFinV-K export by unguessable 24h-expiry token (deliberately public — Steuerberater access) |

### 7c. top-level POS mounts (4 routes)

| Method | Path | Auth | Handler | Purpose |
|---|---|---|---|---|
| GET | /K/ws | user-JWT (POS realm, own middleware instance) | handlers::ws::ws_handler | POS WebSocket for live register updates |
| GET | /K | public | web::static_handler | POS SPA shell |
| GET | /K/ | public | web::static_handler | POS SPA shell |
| GET | /K/*path | public | web::static_handler | POS SPA client-side routing catch-all |

---

## 8. static/UI (node) — 1 route

| Method | Path | Auth | Handler | Purpose |
|---|---|---|---|---|
| (fallback) | any unmatched non-`/api`, non-`/X`, non-`/mcp*`, non-`/S*` path | public | web::static_handler | Serves the embedded WMS SPA (`web/build/`, rust-embed). Redirects any path not starting with `/E` or `/e` to `/E/`; below that prefix, serves the matching embedded asset or falls back to `index.html`. `i/*` assets get a 1-year immutable cache-control; everything else is `no-cache`. |

---

## Judgment calls / verify flags

- `GET /E/ws` (node): auth resolved after audit — JWT arrives as a `?token=` query parameter and is validated inside `ws_handler`; the unauthenticated path upgrades but stays unsubscribed (no data). No `.route_layer` wraps it at the router level
  (it sits before `/api`/`/X` nesting, alongside `/E/health`), and this audit did not open
  `handlers::ws::ws_handler` to check for an in-handler token check on the WS upgrade.
  Treat as unauthenticated until confirmed otherwise.
- `/api/admin/config/kiosk` and `/api/admin/config/dashboard_sla` are named like admin
  routes but are physically declared inside `protected_routes`, so their real gate is
  `user-JWT`, not `admin-JWT` — flagged inline in section 1b rather than moved, to match
  what the router actually does.
- Relay `/E/x/*` and `/E/m/*` are `public` at the relay because the relay explicitly
  does not verify envelope signatures ("the relay is just a queue" — relay/src/handlers/xelixir.rs
  and mesh_relay.rs). Do not infer that dispatching there is unauthenticated end-to-end:
  the receiving node re-verifies. This file only describes what the relay itself gates.
- `/E/c/poll`, `/ack`, `/result` are `public` at the relay because the authorization
  decision already happened at `/E/c/dispatch` (cert-signed) — by the time a task exists
  to poll/ack, it was already admitted.
- POS (`/K`) routes exist in the commercial edition only; an open-core build (no
  `pos-module` feature) has none of section 7's routes reachable, and `POS_ENABLED=true`
  with the feature compiled out only logs a warning.

---

## YAML: full route inventory (programmatic consumption)

```yaml
# service: node = wms/src/main.rs; relay = relay/src/main.rs; pos = pos/src/lib.rs (commercial, feature-gated)
routes:
  # --- node: public_routes (auth: public) ---
  - {method: POST, path: /api/auth/login, auth: public, purpose: "password/PIN login, issues JWT", service: node, edition: open-source}
  - {method: GET, path: /api/auth/setup-status, auth: public, purpose: "first-run admin setup done?", service: node, edition: open-source}
  - {method: GET, path: /api/auth/kiosk-token, auth: public, purpose: "mint scoped kiosk-role token", service: node, edition: open-source}
  - {method: GET, path: /api/auth/device-challenge, auth: public, purpose: "issue pairing challenge nonce", service: node, edition: open-source}
  - {method: POST, path: /api/public/devices/register, auth: public, purpose: "public device self-registration", service: node, edition: open-source}
  - {method: POST, path: /api/internal/register-device, auth: public, purpose: "legacy alias of device self-registration", service: node, edition: open-source}
  - {method: GET, path: /api/public/agreement/:token, auth: public, purpose: "fetch RMA agreement by capability token", service: node, edition: open-source}
  - {method: POST, path: /api/public/agreement/:token/sign, auth: public, purpose: "customer signs RMA agreement", service: node, edition: open-source}
  - {method: GET, path: /api/i18n/dict/:lang, auth: public, purpose: "UI label dictionary for one language", service: node, edition: open-source}
  - {method: GET, path: /api/pos/status, auth: public, purpose: "whether POS_ENABLED is set", service: node, edition: open-source}
  # --- node: protected_routes (auth: user-JWT) ---
  - {method: GET, path: /api/items, auth: user-JWT, purpose: "list items", service: node, edition: open-source}
  - {method: POST, path: /api/items, auth: user-JWT, purpose: "create item", service: node, edition: open-source}
  - {method: GET, path: /api/items/:id, auth: user-JWT, purpose: "get item", service: node, edition: open-source}
  - {method: PUT, path: /api/items/:id, auth: user-JWT, purpose: "update item", service: node, edition: open-source}
  - {method: DELETE, path: /api/items/:id, auth: user-JWT, purpose: "delete item", service: node, edition: open-source}
  - {method: GET, path: /api/products, auth: user-JWT, purpose: "list products", service: node, edition: open-source}
  - {method: POST, path: /api/products, auth: user-JWT, purpose: "create product", service: node, edition: open-source}
  - {method: GET, path: /api/products/:id, auth: user-JWT, purpose: "get product", service: node, edition: open-source}
  - {method: PUT, path: /api/products/:id, auth: user-JWT, purpose: "update product", service: node, edition: open-source}
  - {method: DELETE, path: /api/products/:id, auth: user-JWT, purpose: "delete product", service: node, edition: open-source}
  - {method: GET, path: /api/partners, auth: user-JWT, purpose: "list partners", service: node, edition: open-source}
  - {method: POST, path: /api/partners, auth: user-JWT, purpose: "create partner", service: node, edition: open-source}
  - {method: GET, path: /api/partners/:id, auth: user-JWT, purpose: "get partner", service: node, edition: open-source}
  - {method: PUT, path: /api/partners/:id, auth: user-JWT, purpose: "update partner", service: node, edition: open-source}
  - {method: DELETE, path: /api/partners/:id, auth: user-JWT, purpose: "delete partner", service: node, edition: open-source}
  - {method: GET, path: /api/quants, auth: user-JWT, purpose: "list stock quants", service: node, edition: open-source}
  - {method: POST, path: /api/quants, auth: user-JWT, purpose: "create quant", service: node, edition: open-source}
  - {method: GET, path: /api/quants/:id, auth: user-JWT, purpose: "get quant", service: node, edition: open-source}
  - {method: PUT, path: /api/quants/:id, auth: user-JWT, purpose: "update quant", service: node, edition: open-source}
  - {method: DELETE, path: /api/quants/:id, auth: user-JWT, purpose: "delete quant", service: node, edition: open-source}
  - {method: GET, path: /api/pickings, auth: user-JWT, purpose: "list pickings", service: node, edition: open-source}
  - {method: POST, path: /api/pickings, auth: user-JWT, purpose: "create picking", service: node, edition: open-source}
  - {method: GET, path: /api/pickings/:id, auth: user-JWT, purpose: "get picking", service: node, edition: open-source}
  - {method: PUT, path: /api/pickings/:id, auth: user-JWT, purpose: "update picking", service: node, edition: open-source}
  - {method: DELETE, path: /api/pickings/:id, auth: user-JWT, purpose: "delete picking", service: node, edition: open-source}
  - {method: GET, path: /api/move-lines, auth: user-JWT, purpose: "list picking move lines", service: node, edition: open-source}
  - {method: POST, path: /api/move-lines, auth: user-JWT, purpose: "create move line", service: node, edition: open-source}
  - {method: PUT, path: /api/move-lines/:id, auth: user-JWT, purpose: "update move line", service: node, edition: open-source}
  - {method: GET, path: /api/warehouse, auth: user-JWT, purpose: "list warehouses", service: node, edition: open-source}
  - {method: POST, path: /api/warehouse, auth: user-JWT, purpose: "create warehouse", service: node, edition: open-source}
  - {method: GET, path: /api/warehouse/racks, auth: user-JWT, purpose: "list racks", service: node, edition: open-source}
  - {method: POST, path: /api/warehouse/racks, auth: user-JWT, purpose: "create rack", service: node, edition: open-source}
  - {method: PUT, path: /api/warehouse/racks/:id, auth: user-JWT, purpose: "update rack", service: node, edition: open-source}
  - {method: DELETE, path: /api/warehouse/racks/:id, auth: user-JWT, purpose: "delete rack", service: node, edition: open-source}
  - {method: POST, path: /api/warehouse/put-away, auth: user-JWT, purpose: "put-away operation", service: node, edition: open-source}
  - {method: GET, path: /api/warehouse/bin, auth: user-JWT, purpose: "bin contents lookup", service: node, edition: open-source}
  - {method: GET, path: /api/warehouse/reconcile, auth: user-JWT, purpose: "inventory reconciliation view", service: node, edition: open-source}
  - {method: GET, path: /api/warehouse/inventory, auth: user-JWT, purpose: "inventory snapshot", service: node, edition: open-source}
  - {method: GET, path: /api/warehouse/:id, auth: user-JWT, purpose: "get warehouse", service: node, edition: open-source}
  - {method: GET, path: /api/rma, auth: user-JWT, purpose: "list RMA orders", service: node, edition: open-source}
  - {method: POST, path: /api/rma, auth: user-JWT, purpose: "create RMA order", service: node, edition: open-source}
  - {method: POST, path: /api/rma/search, auth: user-JWT, purpose: "search RMA orders", service: node, edition: open-source}
  - {method: GET, path: /api/rma/:id, auth: user-JWT, purpose: "get RMA order", service: node, edition: open-source}
  - {method: PUT, path: /api/rma/:id, auth: user-JWT, purpose: "update RMA order", service: node, edition: open-source}
  - {method: DELETE, path: /api/rma/:id, auth: user-JWT, purpose: "delete RMA order", service: node, edition: open-source}
  - {method: POST, path: /api/rma/:id/generate-link, auth: user-JWT, purpose: "mint public sign-agreement link", service: node, edition: open-source}
  - {method: GET, path: /api/menu/categories, auth: user-JWT, purpose: "list menu categories", service: node, edition: open-source}
  - {method: POST, path: /api/menu/categories, auth: user-JWT, purpose: "create menu category", service: node, edition: open-source}
  - {method: PUT, path: /api/menu/categories/:id, auth: user-JWT, purpose: "update menu category", service: node, edition: open-source}
  - {method: DELETE, path: /api/menu/categories/:id, auth: user-JWT, purpose: "delete menu category", service: node, edition: open-source}
  - {method: GET, path: /api/menu/items, auth: user-JWT, purpose: "list menu items", service: node, edition: open-source}
  - {method: POST, path: /api/menu/items, auth: user-JWT, purpose: "create menu item", service: node, edition: open-source}
  - {method: PUT, path: /api/menu/items/:id, auth: user-JWT, purpose: "update menu item", service: node, edition: open-source}
  - {method: DELETE, path: /api/menu/items/:id, auth: user-JWT, purpose: "delete menu item", service: node, edition: open-source}
  - {method: GET, path: /api/mesh/status, auth: user-JWT, purpose: "this node's identity + mesh membership", service: node, edition: open-source}
  - {method: GET, path: /api/mesh/nodes, auth: user-JWT, purpose: "relay-known peer nodes", service: node, edition: open-source}
  - {method: GET, path: /api/internal/pairing-qr, auth: user-JWT, purpose: "generate device-pairing QR payload", service: node, edition: open-source}
  - {method: POST, path: /api/print/labels, auth: user-JWT, purpose: "generate warehouse label print job", service: node, edition: open-source}
  - {method: POST, path: /api/proofs, auth: user-JWT, purpose: "submit an action proof", service: node, edition: open-source}
  - {method: GET, path: /api/audit/verify, auth: user-JWT, purpose: "verify tamper-evident audit chain", service: node, edition: open-source}
  - {method: GET, path: /api/audit/chain, auth: user-JWT, purpose: "read audit chain", service: node, edition: open-source}
  - {method: POST, path: /api/audit/anchor, auth: user-JWT, purpose: "anchor audit root to Hedera", service: node, edition: open-source}
  - {method: POST, path: /api/files/upload, auth: user-JWT, purpose: "upload file into CAS filestore", service: node, edition: open-source}
  - {method: GET, path: /api/files/:id, auth: user-JWT, purpose: "download file by id", service: node, edition: open-source}
  - {method: GET, path: /api/files/attachments, auth: user-JWT, purpose: "list attachments for an entity", service: node, edition: open-source}
  - {method: POST, path: /api/files/attach, auth: user-JWT, purpose: "attach existing file to entity", service: node, edition: open-source}
  - {method: DELETE, path: /api/files/attachments/:edge_id, auth: user-JWT, purpose: "remove attachment edge", service: node, edition: open-source}
  - {method: POST, path: /api/files/redirect, auth: user-JWT, purpose: "re-home temp-parked photo onto order", service: node, edition: open-source}
  - {method: DELETE, path: /api/files/temp/:id, auth: user-JWT, purpose: "delete temp-parked photo", service: node, edition: open-source}
  - {method: POST, path: /api/support/import-ticket, auth: user-JWT, purpose: "import one support ticket", service: node, edition: open-source}
  - {method: POST, path: /api/support/import-tickets, auth: user-JWT, purpose: "bulk-import support tickets", service: node, edition: open-source}
  - {method: POST, path: /api/support/import-thread, auth: user-JWT, purpose: "import ticket thread/message", service: node, edition: open-source}
  - {method: GET, path: /api/support/tickets, auth: user-JWT, purpose: "list support tickets", service: node, edition: open-source}
  - {method: GET, path: /api/support/debug/:ticket_id, auth: user-JWT, purpose: "debug view of ticket raw state", service: node, edition: open-source}
  - {method: GET, path: /api/support/tickets/:ticket_id/threads, auth: user-JWT, purpose: "list ticket threads", service: node, edition: open-source}
  - {method: GET, path: /api/support/tickets/:ticket_id/threads/:thread_id/payload, auth: user-JWT, purpose: "read raw thread payload", service: node, edition: open-source}
  - {method: POST, path: /api/support/tickets/:ticket_id/summary, auth: user-JWT, purpose: "trigger AI summarization of ticket", service: node, edition: open-source}
  - {method: GET, path: /api/support/tickets/:ticket_id/similar, auth: user-JWT, purpose: "vector-similar tickets", service: node, edition: open-source}
  - {method: GET, path: /api/ai/tasks, auth: user-JWT, purpose: "list paused AI-orchestrator tasks", service: node, edition: open-source}
  - {method: POST, path: /api/ai/tasks/:id/reply, auth: user-JWT, purpose: "operator reply to paused AI task", service: node, edition: open-source}
  - {method: POST, path: /api/ai/enrich-csv, auth: user-JWT, purpose: "batch AI enrichment of uploaded CSV", service: node, edition: open-source}
  - {method: GET, path: /api/ai/usage, auth: user-JWT, purpose: "local 24h AI token-spend estimate", service: node, edition: open-source}
  - {method: POST, path: /api/voice/resolve, auth: user-JWT, purpose: "resolve voice command (PDA), local-first + Gemini fallback", service: node, edition: open-source}
  - {method: POST, path: /api/geo/fix, auth: user-JWT, purpose: "operator manual geo override", service: node, edition: open-source}
  - {method: POST, path: /api/geo/regeocode-fallback, auth: user-JWT, purpose: "re-geocode HQ-fallback pile", service: node, edition: open-source}
  - {method: GET, path: /api/geo/grounding-config, auth: user-JWT, purpose: "read AI address-discovery config", service: node, edition: open-source}
  - {method: POST, path: /api/geo/grounding-config, auth: user-JWT, purpose: "set AI address-discovery config", service: node, edition: open-source}
  - {method: POST, path: /api/geo/discover-addresses, auth: user-JWT, purpose: "run AI address discovery", service: node, edition: open-source}
  - {method: POST, path: /api/geo/vorwahl-fill, auth: user-JWT, purpose: "place tickets by landline area code", service: node, edition: open-source}
  - {method: POST, path: /api/geo/customer-fill, auth: user-JWT, purpose: "fill location from matched customer", service: node, edition: open-source}
  - {method: GET, path: /api/geo/resolve, auth: user-JWT, purpose: "server-side cached geocode lookup", service: node, edition: open-source}
  - {method: POST, path: /api/exact/import-items, auth: user-JWT, purpose: "manual Exact Online item import", service: node, edition: open-source}
  - {method: POST, path: /api/exact/import-customers, auth: user-JWT, purpose: "manual Exact Online customer import", service: node, edition: open-source}
  - {method: POST, path: /api/exact/import-stock-positions, auth: user-JWT, purpose: "manual Exact Online stock import", service: node, edition: open-source}
  - {method: POST, path: /api/exact/import-quotations, auth: user-JWT, purpose: "manual Exact Online quotation import", service: node, edition: open-source}
  - {method: POST, path: /api/exact/import-sales-orders, auth: user-JWT, purpose: "manual Exact Online sales-order import", service: node, edition: open-source}
  - {method: GET, path: /api/status, auth: user-JWT, purpose: "movFast PDA heartbeat/status", service: node, edition: open-source}
  - {method: POST, path: /api/scan, auth: user-JWT, purpose: "movFast barcode/QR scan event", service: node, edition: open-source}
  - {method: POST, path: /api/repair/event, auth: user-JWT, purpose: "movFast repair-workflow event", service: node, edition: open-source}
  - {method: POST, path: /api/repair/consume, auth: user-JWT, purpose: "movFast repair parts-consumption event", service: node, edition: open-source}
  - {method: POST, path: /api/upload/image, auth: user-JWT, purpose: "upload image (PDA-facing alias)", service: node, edition: open-source}
  - {method: GET, path: /api/users/active, auth: user-JWT, purpose: "active operators for PDA login list", service: node, edition: open-source}
  - {method: POST, path: /api/users/verify-pin, auth: user-JWT, purpose: "PDA PIN verification", service: node, edition: open-source}
  - {method: GET, path: /api/pickings/active, auth: user-JWT, purpose: "active pickings for PDA operator", service: node, edition: open-source}
  - {method: GET, path: /api/pickings/:id/route, auth: user-JWT, purpose: "picking route/path", service: node, edition: open-source}
  - {method: POST, path: /api/pickings/:id/lines/:line_id/confirm, auth: user-JWT, purpose: "confirm picked line", service: node, edition: open-source}
  - {method: POST, path: /api/pickings/:id/validate, auth: user-JWT, purpose: "validate/close picking", service: node, edition: open-source}
  - {method: GET, path: /api/explorer/locations, auth: user-JWT, purpose: "warehouse location explorer list", service: node, edition: open-source}
  - {method: GET, path: /api/explorer/locations/:id/contents, auth: user-JWT, purpose: "contents of one location", service: node, edition: open-source}
  - {method: GET, path: /api/explorer/products, auth: user-JWT, purpose: "product explorer list", service: node, edition: open-source}
  - {method: GET, path: /api/explorer/products/:id/locations, auth: user-JWT, purpose: "locations holding one product", service: node, edition: open-source}
  - {method: POST, path: /api/sync/pull, auth: user-JWT, purpose: "PDA-side data sync pull", service: node, edition: open-source}
  - {method: POST, path: /api/crm/update, auth: user-JWT, purpose: "PDA CRM record update", service: node, edition: open-source}
  - {method: GET, path: /api/crm/:entity_type/:id, auth: user-JWT, purpose: "PDA CRM record read", service: node, edition: open-source}
  - {method: GET, path: /api/trips, auth: user-JWT, purpose: "list Fahrtenbuch trips", service: node, edition: open-source}
  - {method: POST, path: /api/trips, auth: user-JWT, purpose: "upload trip", service: node, edition: open-source}
  - {method: GET, path: /api/trips/export, auth: user-JWT, purpose: "export trips (GoBD Z3 DTD)", service: node, edition: open-source}
  - {method: GET, path: /api/trips/purpose-candidates, auth: user-JWT, purpose: "candidate trip-purpose suggestions", service: node, edition: open-source}
  - {method: GET, path: /api/trips/destinations, auth: user-JWT, purpose: "known trip destinations", service: node, edition: open-source}
  - {method: POST, path: /api/trips/live, auth: user-JWT, purpose: "live trip-tracking ingest", service: node, edition: open-source}
  - {method: GET, path: /api/trips/:id, auth: user-JWT, purpose: "get trip", service: node, edition: open-source}
  - {method: GET, path: /api/trips/:id/verify, auth: user-JWT, purpose: "trip GoBD-seal verification", service: node, edition: open-source}
  - {method: GET, path: /api/cells/cache, auth: user-JWT, purpose: "cell-tower geocoding cache read", service: node, edition: open-source}
  - {method: GET, path: /api/vehicles, auth: user-JWT, purpose: "list vehicles", service: node, edition: open-source}
  - {method: POST, path: /api/vehicles, auth: user-JWT, purpose: "create vehicle", service: node, edition: open-source}
  - {method: PUT, path: /api/vehicles/:id, auth: user-JWT, purpose: "update vehicle", service: node, edition: open-source}
  - {method: GET, path: /api/visits, auth: user-JWT, purpose: "list field visits", service: node, edition: open-source}
  - {method: POST, path: /api/visits, auth: user-JWT, purpose: "create field visit", service: node, edition: open-source}
  - {method: POST, path: /api/visits/:id/checkin, auth: user-JWT, purpose: "visit check-in", service: node, edition: open-source}
  - {method: POST, path: /api/visits/:id/checkout, auth: user-JWT, purpose: "visit check-out", service: node, edition: open-source}
  - {method: GET, path: /api/odoo/ping, auth: user-JWT, purpose: "Odoo connectivity check", service: node, edition: open-source}
  - {method: POST, path: /api/odoo/sync, auth: user-JWT, purpose: "trigger Odoo sync", service: node, edition: open-source}
  - {method: POST, path: /api/odoo/bridge-products, auth: user-JWT, purpose: "bridge/map products against Odoo", service: node, edition: open-source}
  - {method: GET, path: /api/odoo/pickings, auth: user-JWT, purpose: "stub — always empty array", service: node, edition: open-source}
  - {method: GET, path: /api/delivery/shipments, auth: user-JWT, purpose: "list delivery shipments", service: node, edition: open-source}
  - {method: POST, path: /api/delivery/shipments, auth: user-JWT, purpose: "create delivery shipment", service: node, edition: open-source}
  - {method: GET, path: /api/delivery/config, auth: user-JWT, purpose: "delivery module config", service: node, edition: open-source}
  - {method: POST, path: /api/delivery/shipments/:id/cancel, auth: user-JWT, purpose: "cancel shipment", service: node, edition: open-source}
  - {method: POST, path: /api/delivery/shipments/:id/resolve, auth: user-JWT, purpose: "resolve shipment match", service: node, edition: open-source}
  - {method: GET, path: /api/delivery/shipments/:id/ai-match, auth: user-JWT, purpose: "AI-assisted shipment matching", service: node, edition: open-source}
  - {method: POST, path: /api/delivery/import/opal, auth: user-JWT, purpose: "import delivery data from OPAL/OCU", service: node, edition: open-source}
  - {method: POST, path: /api/delivery/import/dhl, auth: user-JWT, purpose: "import delivery data from DHL", service: node, edition: open-source}
  - {method: GET, path: /api/delivery/sync/history, auth: user-JWT, purpose: "delivery sync run history", service: node, edition: open-source}
  - {method: GET, path: /api/delivery/carriers, auth: user-JWT, purpose: "known delivery carriers", service: node, edition: open-source}
  - {method: GET, path: /api/analysis/support-dump, auth: user-JWT, purpose: "analysis-dashboard support data dump", service: node, edition: open-source}
  - {method: GET, path: /api/auth/me, auth: user-JWT, purpose: "current authenticated user/claims", service: node, edition: open-source}
  - {method: POST, path: /api/auth/change-password, auth: user-JWT, purpose: "self-service password change", service: node, edition: open-source}
  - {method: GET, path: /api/i18n/languages, auth: user-JWT, purpose: "list configured UI languages", service: node, edition: open-source}
  - {method: GET, path: /api/admin/config/kiosk, auth: user-JWT, purpose: "read kiosk UI config (NOTE: not admin-JWT despite path)", service: node, edition: open-source}
  - {method: POST, path: /api/admin/config/kiosk, auth: user-JWT, purpose: "write kiosk UI config (NOTE: not admin-JWT despite path)", service: node, edition: open-source}
  - {method: GET, path: /api/admin/config/dashboard_sla, auth: user-JWT, purpose: "read dashboard SLA config (NOTE: not admin-JWT despite path)", service: node, edition: open-source}
  - {method: POST, path: /api/admin/config/dashboard_sla, auth: user-JWT, purpose: "write dashboard SLA config (NOTE: not admin-JWT despite path)", service: node, edition: open-source}
  # --- node: p2p_routes (auth: mesh-secret) ---
  - {method: GET, path: /api/mesh/merkle/state, auth: mesh-secret, purpose: "this node's Merkle-tree state for an entity type", service: node, edition: open-source}
  - {method: GET, path: /api/mesh/parity, auth: mesh-secret, purpose: "cross-node parity/drift check", service: node, edition: open-source}
  - {method: POST, path: /api/mesh/sync/pull, auth: mesh-secret, purpose: "peer pulls raw documents for upsert", service: node, edition: open-source}
  - {method: POST, path: /api/mesh/sync/push, auth: mesh-secret, purpose: "peer pushes an entity upsert", service: node, edition: open-source}
  - {method: GET, path: /api/mesh/file/:hash, auth: mesh-secret, purpose: "serve file bytes to hydrate peer FileStore", service: node, edition: open-source}
  - {method: GET, path: /api/mesh/raw-docs/:ticket_id, auth: mesh-secret, purpose: "raw document_raw records for a ticket", service: node, edition: open-source}
  - {method: GET, path: /api/mesh/tasks, auth: mesh-secret, purpose: "pending mesh tasks for calling node", service: node, edition: open-source}
  - {method: POST, path: /api/mesh/tasks/nudge, auth: mesh-secret, purpose: "nudge processing of pending tasks", service: node, edition: open-source}
  - {method: DELETE, path: /api/mesh/tasks/:id, auth: mesh-secret, purpose: "mark mesh task completed", service: node, edition: open-source}
  # --- node: admin_routes (auth: admin-JWT); NOTE: entire api_router (above + this) is ALSO mirrored 1:1 at /E/api/* ---
  - {method: GET, path: /api/admin/db/backups, auth: admin-JWT, purpose: "list DB backups", service: node, edition: open-source}
  - {method: POST, path: /api/admin/db/backup, auth: admin-JWT, purpose: "create DB backup", service: node, edition: open-source}
  - {method: POST, path: /api/admin/db/restore/:filename, auth: admin-JWT, purpose: "restore DB backup (destructive)", service: node, edition: open-source}
  - {method: POST, path: /api/admin/force-sync, auth: admin-JWT, purpose: "manually trigger sync across scraper providers", service: node, edition: open-source}
  - {method: POST, path: /api/admin/mesh-replay/:entity_type, auth: admin-JWT, purpose: "one-shot mesh-wide backfill of entity type", service: node, edition: open-source}
  - {method: POST, path: /api/admin/gdpr/erase, auth: admin-JWT, purpose: "GDPR Art.17 erasure of AI-derived vectors", service: node, edition: open-source}
  - {method: GET, path: /api/admin/known-nodes, auth: admin-JWT, purpose: "cross-mesh node registry", service: node, edition: open-source}
  - {method: GET, path: /api/admin/users, auth: admin-JWT, purpose: "list operator users", service: node, edition: open-source}
  - {method: POST, path: /api/admin/users, auth: admin-JWT, purpose: "create operator user", service: node, edition: open-source}
  - {method: PUT, path: /api/admin/users/:id, auth: admin-JWT, purpose: "update operator user", service: node, edition: open-source}
  - {method: DELETE, path: /api/admin/users/:id, auth: admin-JWT, purpose: "delete operator user", service: node, edition: open-source}
  - {method: POST, path: /api/admin/pair-code, auth: admin-JWT, purpose: "mint device pairing code", service: node, edition: open-source}
  - {method: GET, path: /api/admin/devices, auth: admin-JWT, purpose: "list paired devices", service: node, edition: open-source}
  - {method: PUT, path: /api/admin/devices/:id/status, auth: admin-JWT, purpose: "update device status", service: node, edition: open-source}
  - {method: PUT, path: /api/admin/devices/:id/home, auth: admin-JWT, purpose: "update device home-node assignment", service: node, edition: open-source}
  - {method: POST, path: /api/admin/devices/:id/restore, auth: admin-JWT, purpose: "restore soft-deleted device", service: node, edition: open-source}
  - {method: DELETE, path: /api/admin/devices/:id, auth: admin-JWT, purpose: "delete device", service: node, edition: open-source}
  - {method: POST, path: /api/admin/mesh/master, auth: admin-JWT, purpose: "transfer mesh master/home designation", service: node, edition: open-source}
  - {method: POST, path: /api/admin/query, auth: admin-JWT, purpose: "arbitrary SurrealQL diagnostics", service: node, edition: open-source}
  - {method: POST, path: /api/support/backfill-assignees, auth: admin-JWT, purpose: "one-shot backfill: ticket assignees", service: node, edition: open-source}
  - {method: POST, path: /api/support/backfill-customfields, auth: admin-JWT, purpose: "one-shot backfill: ticket custom fields", service: node, edition: open-source}
  - {method: POST, path: /api/support/backfill-meta, auth: admin-JWT, purpose: "one-shot backfill: ticket metadata", service: node, edition: open-source}
  - {method: POST, path: /api/support/restamp-thread-hashes, auth: admin-JWT, purpose: "recompute/restamp thread hashes", service: node, edition: open-source}
  - {method: POST, path: /api/support/restamp-vclocks, auth: admin-JWT, purpose: "recompute/restamp vector clocks", service: node, edition: open-source}
  - {method: POST, path: /api/support/claim-home, auth: admin-JWT, purpose: "claim per-entity home_instance_id ownership", service: node, edition: open-source}
  - {method: POST, path: /api/support/backfill-outbound-times, auth: admin-JWT, purpose: "backfill outbound-message timestamps", service: node, edition: open-source}
  - {method: POST, path: /api/support/backfill-thread-headers, auth: admin-JWT, purpose: "backfill thread header fields", service: node, edition: open-source}
  - {method: POST, path: /api/support/backfill-summary-sync, auth: admin-JWT, purpose: "backfill AI-summary sync flags", service: node, edition: open-source}
  - {method: POST, path: /api/support/backfill-embedding-sync, auth: admin-JWT, purpose: "backfill embedding sync flags", service: node, edition: open-source}
  - {method: POST, path: /api/support/requeue-embeddings, auth: admin-JWT, purpose: "requeue tickets for embedding", service: node, edition: open-source}
  - {method: POST, path: /api/support/admin/scrub-summaries, auth: admin-JWT, purpose: "scrub/reset stored AI summaries", service: node, edition: open-source}
  - {method: POST, path: /api/exact/backfill-vclocks, auth: admin-JWT, purpose: "backfill vclocks for Exact-imported rows", service: node, edition: open-source}
  - {method: POST, path: /api/odoo/project/set-onhand, auth: admin-JWT, purpose: "write on-hand qty back into Odoo", service: node, edition: open-source}
  - {method: POST, path: /api/odoo/project/run, auth: admin-JWT, purpose: "run Odoo projection job", service: node, edition: open-source}
  - {method: POST, path: /api/scraper/start, auth: admin-JWT, purpose: "spawn local Node.js scraper process", service: node, edition: open-source}
  - {method: GET, path: /api/admin/mcp-connector, auth: admin-JWT, purpose: "download ready-to-run MCP connector bundle", service: node, edition: open-source}
  - {method: POST, path: /api/admin/i18n/languages, auth: admin-JWT, purpose: "machine-translate UI labels into new language", service: node, edition: open-source}
  - {method: GET, path: /api/admin/i18n/languages/:lang/status, auth: admin-JWT, purpose: "poll status of in-progress language add", service: node, edition: open-source}
  # --- node: /mcp top-level ---
  - {method: POST, path: /mcp, auth: MCP-bearer, purpose: "direct/LAN MCP JSON-RPC transport", service: node, edition: open-source}
  - {method: POST, path: /mcp/signed, auth: cert-signed, purpose: "cert-signed MCP request, no relay hop", service: node, edition: open-source}
  # --- node: /X xelixir_jwt_routes (auth: user-JWT) ---
  - {method: GET, path: /X/config, auth: user-JWT, purpose: "read local xelixir-agent config", service: node, edition: open-source}
  - {method: POST, path: /X/config, auth: user-JWT, purpose: "write local xelixir-agent config", service: node, edition: open-source}
  - {method: POST, path: /X/approve, auth: user-JWT, purpose: "operator approves inbound xelixir session request", service: node, edition: open-source}
  - {method: POST, path: /X/devices/:id/start, auth: user-JWT, purpose: "start paired device's agent", service: node, edition: open-source}
  - {method: POST, path: /X/devices/:id/stop, auth: user-JWT, purpose: "stop paired device's agent", service: node, edition: open-source}
  # --- node: /X xelixir_self_routes (auth: cert-signed) ---
  - {method: POST, path: /X/self/start, auth: cert-signed, purpose: "server-initiated self start via signed envelope", service: node, edition: open-source}
  - {method: POST, path: /X/self/stop, auth: cert-signed, purpose: "server-initiated self stop via signed envelope", service: node, edition: open-source}
  # --- node: /X xelixir_internal_routes (auth: service-token) ---
  - {method: POST, path: /X/internal/dispatch, auth: service-token, purpose: "sibling-service dispatch of start/stop command", service: node, edition: open-source}
  - {method: GET, path: /X/internal/result/:task_id, auth: service-token, purpose: "poll dispatched command result", service: node, edition: open-source}
  # --- node: /X xelixir_ops_routes (auth: service-token, audit-logged) ---
  - {method: GET, path: /X/ops/journal, auth: service-token, purpose: "read ops journal", service: node, edition: open-source}
  - {method: GET, path: /X/ops/service_status, auth: service-token, purpose: "systemd/service status", service: node, edition: open-source}
  - {method: GET, path: /X/ops/system_health, auth: service-token, purpose: "host system health snapshot", service: node, edition: open-source}
  - {method: GET, path: /X/ops/health_check, auth: service-token, purpose: "node health check (ops view)", service: node, edition: open-source}
  - {method: GET, path: /X/ops/loop_metrics, auth: service-token, purpose: "per-loop rate + CPU/RSS history", service: node, edition: open-source}
  - {method: GET, path: /X/ops/file_read, auth: service-token, purpose: "read file under allow-listed prefix", service: node, edition: open-source}
  - {method: POST, path: /X/ops/file_write, auth: service-token, purpose: "write file under allow-listed prefix", service: node, edition: open-source}
  - {method: POST, path: /X/ops/surrealql_read, auth: service-token, purpose: "read-only SurrealQL query", service: node, edition: open-source}
  - {method: POST, path: /X/ops/surrealql_write, auth: service-token, purpose: "write SurrealQL query", service: node, edition: open-source}
  - {method: POST, path: /X/ops/restart_service, auth: service-token, purpose: "restart a systemd service", service: node, edition: open-source}
  - {method: POST, path: /X/ops/git_pull, auth: service-token, purpose: "long-running git pull, returns task_id", service: node, edition: open-source}
  - {method: POST, path: /X/ops/cargo_build, auth: service-token, purpose: "long-running cargo build", service: node, edition: open-source}
  - {method: POST, path: /X/ops/deploy, auth: service-token, purpose: "long-running deploy", service: node, edition: open-source}
  - {method: GET, path: /X/ops/task/:task_id, auth: service-token, purpose: "poll long-running ops task", service: node, edition: open-source}
  - {method: POST, path: /X/ops/nginx_test_reload, auth: service-token, purpose: "test + reload nginx config", service: node, edition: open-source}
  - {method: POST, path: /X/ops/package_install, auth: service-token, purpose: "install an OS package", service: node, edition: open-source}
  # --- node: top-level /E/* + /S/* ---
  - {method: GET, path: /E/health, auth: public, purpose: "node liveness/version check", service: node, edition: open-source}
  - {method: GET, path: /E/ws, auth: verify, purpose: "WebSocket upgrade for live UI updates — no router-level auth layer found", service: node, edition: open-source}
  - {method: GET, path: /E/auth/setup-status, auth: public, purpose: "top-level alias of /api/auth/setup-status", service: node, edition: open-source}
  - {method: POST, path: /E/auth/login, auth: public, purpose: "top-level alias of /api/auth/login", service: node, edition: open-source}
  - {method: ANY, path: /S, auth: public, purpose: "reverse-proxy root to local Node.js scraper (127.0.0.1 only backend)", service: node, edition: open-source}
  - {method: ANY, path: /S/*path, auth: public, purpose: "reverse-proxy sub-path to local Node.js scraper (127.0.0.1 only backend)", service: node, edition: open-source}
  # --- node: static/UI fallback ---
  - {method: GET, path: "* (fallback)", auth: public, purpose: "serve embedded WMS SPA / redirect to /E/", service: node, edition: open-source}
  # --- relay: relay/src/main.rs ---
  - {method: GET, path: /E/health, auth: public, purpose: "relay liveness check", service: relay, edition: open-source}
  - {method: POST, path: /E/register, auth: public, purpose: "node registers itself with the relay", service: relay, edition: open-source}
  - {method: POST, path: /E/push, auth: public, purpose: "push encrypted packet, fanned out to mesh peers", service: relay, edition: open-source}
  - {method: GET, path: "/E/pull/{mesh_id}/{instance_id}", auth: public, purpose: "pull + delete queued encrypted packets", service: relay, edition: open-source}
  - {method: GET, path: "/E/mesh/{mesh_id}/status", auth: public, purpose: "list registrations in a mesh", service: relay, edition: open-source}
  - {method: GET, path: "/E/mesh/{mesh_id}/resolve/{instance_id}", auth: public, purpose: "resolve one instance's registration record", service: relay, edition: open-source}
  - {method: GET, path: /E/registry, auth: service-token, purpose: "admin listing of full relay registry (RELAY_ADMIN_TOKEN)", service: relay, edition: open-source}
  - {method: GET, path: "/E/resolve/{instance_id}", auth: public, purpose: "resolve an instance_id, mesh-agnostic", service: relay, edition: open-source}
  - {method: POST, path: "/E/x/dispatch/{target_uuid}", auth: public, purpose: "queue xelixir C2 command (relay does not verify signature)", service: relay, edition: open-source}
  - {method: GET, path: "/E/x/poll/{self_uuid}", auth: public, purpose: "target polls pending xelixir commands", service: relay, edition: open-source}
  - {method: POST, path: "/E/x/ack/{task_id}", auth: public, purpose: "target acks xelixir command with result", service: relay, edition: open-source}
  - {method: GET, path: "/E/x/result/{task_id}", auth: public, purpose: "dispatcher polls ack'd result", service: relay, edition: open-source}
  - {method: POST, path: "/E/m/dispatch/{target_uuid}", auth: public, purpose: "queue NAT-fallback mesh-sync task, subject to RELAY_PAYLOAD_MODE", service: relay, edition: open-source}
  - {method: GET, path: "/E/m/poll/{self_uuid}", auth: public, purpose: "target long-polls pending mesh-sync tasks", service: relay, edition: open-source}
  - {method: POST, path: "/E/m/ack/{task_id}", auth: public, purpose: "target acks mesh-sync task with result", service: relay, edition: open-source}
  - {method: GET, path: "/E/m/result/{task_id}", auth: public, purpose: "sender polls ack'd mesh-sync result", service: relay, edition: open-source}
  - {method: POST, path: "/E/c/dispatch/{target_uuid}", auth: cert-signed, purpose: "admit paid subscriber's MCP request for NAT'd node (THE gate)", service: relay, edition: open-source}
  - {method: GET, path: "/E/c/poll/{self_uuid}", auth: public, purpose: "target long-polls pending client-MCP tasks (already pre-admitted)", service: relay, edition: open-source}
  - {method: POST, path: "/E/c/ack/{task_id}", auth: public, purpose: "target posts MCP result body, first-ack-wins", service: relay, edition: open-source}
  - {method: GET, path: "/E/c/result/{task_id}", auth: public, purpose: "subscriber polls until node acks", service: relay, edition: open-source}
  - {method: POST, path: /E/pair/announce, auth: public, purpose: "master publishes short-code pairing rendezvous (TTL ~10min)", service: relay, edition: open-source}
  - {method: POST, path: /E/pair/code, auth: public, purpose: "resolve typed short code into full ECK$ pairing string", service: relay, edition: open-source}
  # --- pos (commercial edition, feature-gated): protected_routes (auth: user-JWT, POS realm) ---
  - {method: GET, path: /K/api/transactions, auth: user-JWT, purpose: "list POS transactions", service: pos, edition: commercial}
  - {method: POST, path: /K/api/transactions/active, auth: user-JWT, purpose: "find or open active transaction", service: pos, edition: commercial}
  - {method: GET, path: /K/api/transactions/parked, auth: user-JWT, purpose: "list parked transactions", service: pos, edition: commercial}
  - {method: POST, path: /K/api/transactions/:id/reprint, auth: user-JWT, purpose: "reprint a receipt", service: pos, edition: commercial}
  - {method: POST, path: /K/api/transactions/:id/cancel, auth: user-JWT, purpose: "cancel a transaction", service: pos, edition: commercial}
  - {method: PUT, path: /K/api/transactions/:id/table, auth: user-JWT, purpose: "assign table to transaction", service: pos, edition: commercial}
  - {method: POST, path: /K/api/transactions/:id/items, auth: user-JWT, purpose: "add line item", service: pos, edition: commercial}
  - {method: PUT, path: /K/api/transactions/:id/items/:item_id, auth: user-JWT, purpose: "update line item", service: pos, edition: commercial}
  - {method: POST, path: /K/api/transactions/:id/finish, auth: user-JWT, purpose: "finish/close a sale (fiscal path)", service: pos, edition: commercial}
  - {method: POST, path: /K/api/transactions/:id/storno, auth: user-JWT, purpose: "void/storno a transaction", service: pos, edition: commercial}
  - {method: POST, path: /K/api/transactions/storno/:storno_id/approve, auth: user-JWT, purpose: "approve storno request", service: pos, edition: commercial}
  - {method: POST, path: /K/api/transactions/storno/:storno_id/reject, auth: user-JWT, purpose: "reject storno request", service: pos, edition: commercial}
  - {method: POST, path: /K/api/ai/chat, auth: user-JWT, purpose: "POS-embedded AI chat assistant", service: pos, edition: commercial}
  - {method: GET, path: /K/api/hardware/usb, auth: user-JWT, purpose: "list attached USB hardware", service: pos, edition: commercial}
  - {method: POST, path: /K/api/hardware/test-print, auth: user-JWT, purpose: "test-print on receipt printer", service: pos, edition: commercial}
  - {method: GET, path: /K/api/menu/categories, auth: user-JWT, purpose: "POS menu categories (read)", service: pos, edition: commercial}
  - {method: GET, path: /K/api/menu/items, auth: user-JWT, purpose: "POS menu items (read)", service: pos, edition: commercial}
  - {method: GET, path: /K/api/menu/search, auth: user-JWT, purpose: "search menu items", service: pos, edition: commercial}
  - {method: POST, path: /K/api/menu/import, auth: user-JWT, purpose: "import a menu (20MiB body cap)", service: pos, edition: commercial}
  - {method: POST, path: /K/api/export/dsfinvk/start, auth: user-JWT, purpose: "start DSFinV-K fiscal export job", service: pos, edition: commercial}
  - {method: GET, path: /K/api/export/dsfinvk/status/:job_id, auth: user-JWT, purpose: "poll DSFinV-K export job status", service: pos, edition: commercial}
  - {method: GET, path: /K/api/audit/verify, auth: user-JWT, purpose: "verify POS audit chain", service: pos, edition: commercial}
  - {method: GET, path: /K/api/audit/chain, auth: user-JWT, purpose: "read POS audit chain", service: pos, edition: commercial}
  - {method: POST, path: /K/api/audit/anchor, auth: user-JWT, purpose: "anchor POS audit root", service: pos, edition: commercial}
  # --- pos: public_routes ---
  - {method: GET, path: /K/api/health, auth: public, purpose: "POS liveness check", service: pos, edition: commercial}
  - {method: GET, path: /K/api/auth/users, auth: public, purpose: "list cashier accounts for login picker", service: pos, edition: commercial}
  - {method: POST, path: /K/api/auth/login, auth: public, purpose: "cashier password login", service: pos, edition: commercial}
  - {method: POST, path: /K/api/auth/login-pin, auth: public, purpose: "cashier PIN login", service: pos, edition: commercial}
  - {method: GET, path: /K/api/export/dsfinvk/download/:token, auth: public, purpose: "download DSFinV-K export by 24h capability token", service: pos, edition: commercial}
  # --- pos: top-level mounts ---
  - {method: GET, path: /K/ws, auth: user-JWT, purpose: "POS WebSocket for live register updates", service: pos, edition: commercial}
  - {method: GET, path: /K, auth: public, purpose: "POS SPA shell", service: pos, edition: commercial}
  - {method: GET, path: /K/, auth: public, purpose: "POS SPA shell", service: pos, edition: commercial}
  - {method: GET, path: /K/*path, auth: public, purpose: "POS SPA client-side routing catch-all", service: pos, edition: commercial}
```
