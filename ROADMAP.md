# eckWMS Project Roadmap

## 🟢 Phase 1: Connectivity & Transport (COMPLETED)
- [x] **Hybrid Transport Layer**: WebSocket (Fast) + HTTP (Reliable) implemented.
- [x] **Self-Healing**: Android client automatically re-registers if keys/db mismatch.
- [x] **Deduplication**: Server handles duplicate messages from hybrid transport.
- [x] **Real-Time Push**: Server can push commands to specific devices.

## 🟢 Phase 2: Role-Based Access Control (RBAC) (COMPLETED)
- [x] **Database Schema**: Roles, Permissions, RolePermissions tables created.
- [x] **Dynamic API**: Agent can create roles and assign permissions programmatically.
- [x] **Instant Sync**: Changing a role instantly updates Android UI via WebSocket.
- [x] **Offline Support**: Permissions are persisted locally on the device.

## 🟢 Phase 3: AI-Driven Interface (SDUI) (COMPLETED)
- [x] **Dynamic UI Engine**: Android renders native UI from JSON layouts.
- [x] **Real-Time Layout Push**: Server can change app interface instantly via WebSocket.
- [x] **Hybrid Wiring**: `LAYOUT_UPDATE` events trigger ViewModel updates.

## 🟡 Phase 4: Business Logic & Workflows (NEXT)
- [ ] **AI Workflow Generation**: Allow Agent to generate task-specific UIs dynamically.
- [ ] **Action Handling**: Connect Dynamic UI buttons (e.g., 'start_scan') to actual hardware triggers.
- [ ] **Input Feedback**: Allow Dynamic UI to send form data back to the Agent.

## 🔵 Phase 5: Production Readiness
- [ ] **Logging**: Centralized logging for all devices.
- [ ] **Security**: Audit of API endpoints and WebSocket auth.
