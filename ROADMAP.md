# eckWMS Project Roadmap

## 🟢 Phase 1: Connectivity & Transport (COMPLETED)
- [x] **Hybrid Transport Layer**: WebSocket (Fast) + HTTP (Reliable).
- [x] **Self-Healing**: Android client auto-registration.
- [x] **Deduplication**: Server handles duplicate messages.

## 🟢 Phase 2: Role-Based Access Control (COMPLETED)
- [x] **Dynamic Roles**: Database schema and API.
- [x] **Instant Sync**: WebSocket push for permissions.

## 🟢 Phase 3: AI-Driven Interface (COMPLETED)
- [x] **Dynamic UI Engine**: Server Driven UI (SDUI).
- [x] **Layout Push**: Real-time interface updates.

## 🟢 Phase 4: Server-Side AI Agent (COMPLETED)
- [x] **Gemini Integration**: Service upgraded to @google/genai.
- [x] **Inventory Tools**: Search and Link tools created.
- [x] **Persistent Memory**: `product_aliases` table implemented.
- [x] **Context Awareness**: Agent distinguishes Receiving vs Internal Ops.
- [x] **Feedback Loop**: `ai_interaction` protocol defined for client.

## 🟡 Phase 5: Android Client AI Integration (NEXT)
- [ ] **Protocol Implementation**: Handle `ai_interaction` JSON in `ScanRecoveryViewModel`.
- [ ] **UI Components**: Build QuestionDialog, ConfirmationDialog, SuccessToast.
- [ ] **Interactivity**: Wire buttons to send responses back to server.

## 🔵 Phase 6: Production Readiness
- [ ] **Logging**: Centralized logging (ELK/Loki).
- [ ] **Security Audit**: API endpoints and WebSocket auth.
