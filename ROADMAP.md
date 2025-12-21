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

## 🟡 Phase 3: Workflows & Business Logic (NEXT)
- [ ] **Workflow Engine**: Finalize JSON-based workflow execution on Android.
- [ ] **Inventory Actions**: Implement `Manual Restock` logic using new permissions.
- [ ] **AI Workflow Generation**: Allow Agent to generate workflow JSONs dynamically.

## 🔵 Phase 4: Production Readiness
- [ ] **Logging**: Centralized logging for all devices.
- [ ] **Security**: Audit of API endpoints and WebSocket auth.
