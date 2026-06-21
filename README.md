# eckWMS 📦🤖
**Next-Generation, AI-Native Warehouse & Repair Management System**

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
* **Local PII Extraction:** Names and addresses are isolated before vectorization.
* **Keyed SimHash:** PII is passed through a deterministic, collision-resistant LSH algorithm peppered with a cryptographic `SYNC_SECRET` (SHA-256 over character bigrams).
* **Secure Embeddings:** Google Gemini processes clean text with obfuscated tokens (e.g., `Name_CC0068898836CB06`). The real identity never leaves your server.

This mathematically guarantees that customer identities never reach third-party AI providers in plaintext, while preserving the AI's ability to perform semantic vector search and match entities even with typos. See the math behind it in [PPRL_ARCHITECTURE.md](.eck/PPRL_ARCHITECTURE.md).

### 5. The Enterprise Vision: Extending Twenty CRM to a Full ERP
While eckWMS serves as the operational high-speed edge, it is built to integrate. Our strategic vision is to bidirectionally sync with modern, open-source CRMs like **Twenty CRM**. 
By handling the heavy lifting of inventory (quants, pickings, move lines), hardware repair tracking, and fiscal compliance, **eckWMS effectively extends Twenty CRM into a complete, enterprise-grade ERP ecosystem** without the bloat of legacy systems.

---

## ⚡ Current Features

* **Advanced Warehouse Management:** Full CRUD for Products, Locations, Racks, Quants, Pickings, and Move Lines.
* **Repair & RMA Management:** Comprehensive lifecycle tracking for hardware repairs. Includes AI-powered "Find Similar Historical Repairs" to speed up technician diagnostics.
* **Content-Addressable FileStore (CAS):** 128-bit MurmurHash3 UUID storage for attachments and avatars. Identical files are automatically deduplicated.
* **Smart Barcodes:** Native support for SmartTag V2 (19-byte compact binary barcodes) with URL-safe Base64 encoding.
* **Hardware Integration:** Built-in ESC/POS printer drivers (TCP/USB) and PDF label sheet generation.
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
