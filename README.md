# eckWMS 📦🤖
**Next-Generation, AI-Native Warehouse & Repair Management System**

> **Machine readers start at [`llms.txt`](llms.txt).** This project's detailed
> documentation is machine-first — written for AI agents evaluating,
> integrating with, or operating the system (`docs/machine/`). The prose below
> is the short human overview. License: **AGPL-3.0** ([open-core boundary](docs/machine/80-boundaries.md)).

![Rust](https://img.shields.io/badge/Built%20with-Rust-orange.svg)
![SurrealDB](https://img.shields.io/badge/Database-SurrealDB-purple.svg)
![Architecture](https://img.shields.io/badge/Architecture-Offline--First%20Edge-success.svg)
![AI](https://img.shields.io/badge/AI-Gemini%20Hybrid%20Search-blue.svg)

**eckWMS** is a blazing-fast, offline-capable, and AI-integrated Warehouse Management System (WMS) and Repair/RMA ERP. Built entirely in Rust and powered by embedded SurrealDB, it is designed to run seamlessly on edge devices (PDAs, registers, warehouse terminals) with zero latency, syncing across a decentralized P2P mesh network.

---

## 🌟 Core Principles & Strengths

### 1. Zero-Latency Edge Architecture
Traditional ERPs rely on a central cloud database, making warehouse scanners painfully slow. eckWMS runs an **embedded SurrealKV database directly on the edge node**. Operations like barcode scanning, picking, and RMA updates happen in single-digit milliseconds. The entire backend ships as a single static Rust binary.

### 2. Offline-First P2P Mesh Sync
Warehouses and repair shops shouldn't stop working when the internet goes down. 
* **Decentralized:** Nodes sync directly with each other (Peer-to-Peer) via a blind relay tracker.
* **Smart Diffing:** Uses a two-level **Merkle Tree** to identify exact changes, ensuring $O(\log n)$ sync efficiency.
* **Conflict Resolution:** Built-in Vector Clocks track causality across all distributed nodes.

### 3. AI with Legal Accountability (GoBD-Ready)
We use AI (Google Gemini) extensively, but in enterprise environments, AI needs an audit trail.
* **Hybrid Search:** Combines BM25 full-text search with Gemini 768-dim Vector Embeddings for unparalleled RMA/repair issue matching.
* **Immutable AI Actions:** Every autonomous AI decision (prompt, reasoning, output) is SHA-256 hashed and sealed on the **Hedera Hashgraph Consensus Service (HCS)**. This provides cryptographic proof of the AI's actions, satisfying strict German fiscal compliance (GoBD / Festschreibung).

### 4. Zero-Knowledge AI Search (GDPR & Privacy-Preserving)
Sending raw customer data (PII) to cloud AI models violates strict European privacy laws. eckWMS implements a mathematical **Privacy-Preserving Record Linkage (PPRL)** pipeline to achieve "Zero-Knowledge" semantic search.
* **Local PII Extraction:** Names and addresses are isolated (lexicon + optional on-device or peer-served NER) before vectorization.
* **Keyed pseudonym tokens:** PII becomes a deterministic keyed token peppered with a cryptographic `SYNC_SECRET` (`Name_<16 hex>`), stable across every node and record so agents can correlate and pivot on it — without ever seeing the clear value.
* **Typo-tolerant search, still masked:** a separate per-word Bloom-256 + Kölner-Phonetik signature enables fuzzy name/address matching over masked data.
* **Secure Embeddings:** Google Gemini processes clean text with obfuscated tokens. The real identity never leaves your server; reveal is master-tier only and only into a local file.
* **Optional zero-knowledge vault:** persistent `token → clear` material can be sealed under a per-node key and Shamir-split 2-of-3 across your own nodes, so no single host can read it.

This keeps customer identities out of third-party AI providers in plaintext while preserving semantic vector search and typo-tolerant matching. Full model: [`docs/machine/30-pprl.md`](docs/machine/30-pprl.md).

### 5. The Enterprise Vision: Extending Twenty CRM to a Full ERP
While eckWMS serves as the operational high-speed edge, it is built to integrate. Our strategic vision is to bidirectionally sync with modern, open-source CRMs like **Twenty CRM**. 
By handling the heavy lifting of inventory (quants, pickings, move lines), hardware repair tracking, and fiscal compliance, **eckWMS effectively extends Twenty CRM into a complete, enterprise-grade ERP ecosystem** without the bloat of legacy systems.

---

## ⚡ Current Features

* **Advanced Warehouse Management:** Full CRUD for Products, Locations, Racks, Quants, Pickings, and Move Lines, plus unit intake/receiving.
* **Repair & RMA Management:** Comprehensive lifecycle tracking for hardware repairs. Includes AI-powered "Find Similar Historical Repairs" to speed up technician diagnostics.
* **Agent-native MCP surface:** an embedded Model Context Protocol server (`POST /mcp`) exposes the business graph as tools behind the PII masking layer — the distinguishing feature (see [`docs/machine/40-mcp-tools.md`](docs/machine/40-mcp-tools.md)).
* **AI brain & cheap-tier routing:** live + Vertex-batch summarization, 768-dim hybrid search, an agentic Brain-Chat/orchestrator, and a task router that can send selected work to an OpenRouter-compatible model with a time-budgeted hedge back to Gemini.
* **Content-Addressable FileStore (CAS):** 128-bit MurmurHash3 UUID storage for attachments and avatars. Identical files are automatically deduplicated.
* **Hardware Integration:** CUPS label printing (agent-first, browser fallback), plus SmartTag V2 compact binary barcodes.
* **Extensibility & governance:** a sandboxed WASM (extism) plugin runtime on the ingest seam, and Ed25519 license gates with a CRL kill switch.
* **Clickwrap Agreements:** Legally binding digital signatures for cost estimates and data privacy (AVV), sealed via Hedera HCS.
* **Device Pairing:** Secure ECK-P1-ALPHA protocol utilizing Ed25519 cryptography and QR code handshakes.

---

## 🗺️ Roadmap & Planned Features

* **Phase 1: Feature Parity & Stability (Completed)**
  * Full migration from legacy PostgreSQL/Node.js to Rust/SurrealDB.
  * P2P Merkle Sync and CAS FileStore.
* **Phase 2: Immutable Audit Trail (Completed)**
  * Hedera HCS deep integration for financial transactions and AI actions.
* **Phase 3: Compliance & Tax Module (In Progress)**
  * Isolated C/FFI worker for ELSTER (ERiC) German tax submissions.
  * EU VIES VAT validation endpoint (Completed).
* **Phase 4: Autonomous AI Agent**
  * Gemini-powered "AI Accountant" to autonomously categorize expenses.
  * Natural language hardware troubleshooting and auto-recovery for POS/WMS edge devices.
* **Phase 5: Enterprise Edge Mode**
  * Full bidirectional REST/JSON-RPC synchronization with **Twenty CRM** and Odoo.

---

## 🛠️ Getting Started

### Prerequisites
* Rust Toolchain (stable, edition 2021)
* *Note: No external database server is required. SurrealDB runs embedded (SurrealKV).*

### Running the Server
Clone the repository and run the WMS binary. It will automatically initialize the embedded database in the `data/wms.db` folder.

```bash
git clone https://github.com/xelth-com/eckWMS.git
cd eckWMS
cargo run -p wms
```

**Environment Variables (Optional):**
* `PORT`: HTTP listen port (default: `3210`)
* `SURREAL_DB_PATH`: Path to the database folder (default: `data/wms.db`)
* `GEMINI_API_KEY`: Required for Hybrid Vector Search & AI features.
* `SYNC_SECRET`: 32-byte hex key for P2P mesh network joining. **Also the PPRL
  pepper for PII anonymisation — it is therefore REQUIRED when AI features are
  enabled.** The server refuses to start AI without it rather than fall back to a
  public default pepper (which would make the anonymised PII tokens reversible).

*On first startup, the system will automatically generate a temporary `setup-admin` account and print the credentials to the console.*

---
*Built with passion for high-performance industrial tech.*
