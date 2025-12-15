# Changelog

All notable changes to the eckWMS Global Server will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added

#### 2025-12-15 - Implement dynamic port synchronization

**Type:** Feature
**Scope:** Core - Instance Registration & Discovery
**Task ID:** implement_dynamic_port_sync

**Summary:**
Implemented dynamic port synchronization between Local and Global servers. Local servers now report their actual listening port during registration, and the Global server uses this port when generating connection candidates for Android clients.

**Details:**
- Local Server changes:
  - `src/server/local/utils/startupDiagnostics.js`: Added port reporting in registration payload
  - Port is read from `LOCAL_SERVER_PORT`, `PORT` env vars, or defaults to 3100
- Global Server changes:
  - `services/eckwms-global/src/server.js`: Registration endpoint now accepts `port` parameter
  - Port is saved in `server_url` field during instance creation and updates
  - Both discovery endpoints extract port from saved `server_url` and use it for all connection candidates
  - Removed hardcoded port assumptions from Global Server

**Impact:**
- Eliminates port mismatches between server configuration and discovery responses
- Supports flexible port configurations across different instances
- Android clients receive accurate connection URLs with correct ports
- Backward compatible: Falls back to port 3100 if not provided

**Files Modified:**
- `src/server/local/utils/startupDiagnostics.js` (lines 26-32)
- `services/eckwms-global/src/server.js` (lines 155, 175, 186, 230-237, 243, 254, 319-326, 332, 343)

**Deployment:**
- Global Server restarted: 2025-12-15 01:07:00 UTC
- Local servers will report ports on next registration/restart
- Existing instances will update to correct port on next heartbeat

### Fixed

#### 2025-12-15 - Fix JSON key for instance discovery candidates

**Type:** API Contract Fix
**Scope:** Instance Discovery Endpoint
**Task ID:** fix_discovery_candidates

**Summary:**
Fixed Android client crash caused by JSON key mismatch in instance discovery response. The server was sending `connectionCandidates` but the Android client expected `candidates`.

**Details:**
- Changed JSON response key from `connectionCandidates` to `candidates` in both discovery endpoints
- Affected endpoints:
  - `GET /ECK/api/internal/get-instance-info/:id` (server.js:264)
  - `POST /ECK/API/INTERNAL/GET-INSTANCE-INFO` (server.js:344)
- Error message from Android: "Error no value for candidates"
- Root cause: API contract mismatch between server and client

**Impact:**
- Resolves Android client parsing error during QR code pairing
- Restores instance discovery functionality for mobile devices
- No breaking changes for other clients (key name change only)

**Files Modified:**
- `services/eckwms-global/src/server.js`

**Deployment:**
- Service restarted via PM2
- Change applied to production: 2025-12-15 01:02:20 UTC
