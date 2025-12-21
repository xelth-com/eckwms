# eckWMS - Android Client Integration Contract

**Version: 1.0**

This document outlines the technical requirements and API contract for the `eckwms-movfast` Android application to interact with the `eckwms` server ecosystem.

## 1. Core Architectural Concept: Local-First, Global-Fallback

The entire system is designed for maximum reliability and performance. The Android client **MUST** operate in an **offline-first** manner and follow this connectivity strategy:

1.  **Attempt Local Connection:** By default, all API requests should be sent directly to the **Local Server** within the facility's network. This ensures the fastest possible response times.
2.  **Fallback to Global Proxy:** If the Local Server is unreachable (e.g., user is outside the local Wi-Fi, server is down), the client **MUST NOT** show an error. Instead, it must **automatically** retry the same request by sending it to the **Global Server's** proxy endpoint.
3.  **Offline Queue:** If both servers are unreachable, the client **MUST** queue the action (e.g., scan, image upload) in its local database (`SyncQueue`). A background service (`WorkManager`) will periodically attempt to send the queued data once connectivity is restored.

## 2. Client Configuration

The Android application must provide a settings screen where the user can configure two separate URLs:

-   **`LOCAL_SERVER_URL`**: The IP address and port of the server running on the local network (e.g., `http://192.168.1.100:3100`).
-   **`GLOBAL_SERVER_URL`**: The public domain and port of the globally accessible server (e.g., `http://your-domain.com:8080`).

## 3. Key API Endpoints & Payloads

### 3.1. Scan Data Submission

This is the primary endpoint for sending barcode data.

-   **Local Endpoint:** `POST /eckwms/api/scan`
-   **Global Proxy Endpoint:** `POST /api/proxy/eckwms/api/scan`

**Headers:**

-   `Content-Type: application/json`
-   `X-API-Key: {INSTANCE_API_KEY}` (For authentication with the server)

**JSON Payload Example:**

```json
{
  "deviceId": "a1b2c3d4e5f6g7h8",
  "payload": "I700000000002113897",
  "type": "CODE_128",
  "checksum": "a4f3b2c1",
  "priority": 0,
  "orderId": "CS-DE-251108-1" // Optional: If a specific order is active
}
```

-   `deviceId`: Unique identifier of the Android device.
-   `payload`: The raw string data from the scanned barcode.
-   `type`: The barcode symbology (e.g., `CODE_128`, `QR_CODE`).
-   `checksum`: A CRC32 checksum of the `payload` string to ensure data integrity.

### 3.2. Image Upload

Used for uploading photos associated with scans or workflows.

-   **Endpoint:** `POST /api/upload/image` (Works for both local and global servers, as the global server will proxy it if needed).

**Request Type:** `multipart/form-data`

**Form Fields:**

-   `image`: The image file data (e.g., in WebP or JPEG format).
-   `deviceId`: Unique identifier of the Android device.
-   `scanMode`: Context of the capture (e.g., `direct_upload`, `workflow_capture`).
-   `barcodeData`: (Optional) Any barcode data extracted from the image by ML Kit.
-   `imageChecksum`: A CRC32 checksum of the image file bytes to ensure integrity.
-   `orderId`: (Optional) The active order ID to associate the image with.

## 4. Android Application Architecture Requirements

The `eckwms-movfast` application must be structured to support the described functionality and future expansion.

### 4.1. AI-Driven Core

An **`AndroidAgent.kt`** singleton must act as the central orchestrator ('brain') of the application. It is responsible for:
-   Managing and executing workflows.
-   Making decisions about connectivity strategies.
-   Coordinating between different components (UI, Data, Hardware).
-   Providing a clear interface for future integration with a real LLM (like Gemini API).

### 4.2. Local Database (Offline-First)

A local database (using **AndroidX Room**) is mandatory. It must contain at least:
-   **Data Cache:** Tables for caching essential WMS data (`ItemEntity`, `BoxEntity`, etc.) to provide an instant UI experience.
-   **Sync Queue:** A `SyncQueueEntity` table to store all actions performed while offline. Each entry represents a task (e.g., an API call) that needs to be executed once the network is available.

### 4.3. Data & Connectivity Layer

-   A **`WarehouseRepository.kt`** must be created to abstract data sources. UI components should only interact with the repository, which will decide whether to fetch data from the local Room cache or from the network via `ScanApiService`.
-   A **`SyncWorker.kt`** (using `WorkManager`) must be implemented to reliably process the `SyncQueue` in the background, even if the app is closed.

### 4.4. Modular Scanner Drivers

The application must support multiple hardware scanner vendors. This must be achieved through a modular driver architecture:
-   An interface **`ScannerDriver.kt`** will define a common contract for all scanners (`initialize`, `startScan`, `isSupported`, etc.).
-   The existing `XCScannerWrapper.kt` will be refactored into `XCScannerDriver.kt` to implement this interface.
-   A **`ScannerDriverFactory.kt`** will be responsible for detecting the device's hardware at runtime and selecting the appropriate driver. This allows new hardware support (e.g., for Seuic, iData) to be added simply by creating a new driver class.

### 3.3. AI Interactive Responses (Hybrid Identification)

**Version:** 1.1 (Added 2024-12-21)

Starting with version 1.1, the server implements an **AI-powered Hybrid Identification system**. When the server encounters an unknown barcode, it uses AI (Gemini) to analyze the code and determine appropriate actions. The server may respond with **structured AI interaction data** that the client must handle to provide an interactive user experience.

#### 3.3.1. Response Structure

When a scan is processed, the response may now include an `ai_interaction` object within the `data` field:

**Enhanced Response Example:**

```json
{
  "success": true,
  "type": "item_barcode",
  "message": "I don't recognize 'ABC123'",
  "data": {
    "barcode": "ABC123",
    "aiAnalysis": "Full AI response text here...",
    "ai_interaction": {
      "type": "question",
      "message": "I don't recognize 'ABC123'. Is this a new product code, or did you mean to scan something else?",
      "requiresResponse": true,
      "suggestedActions": ["yes", "no", "cancel"],
      "toolCallsMade": 1,
      "summary": "I don't recognize 'ABC123'"
    }
  },
  "buffers": {
    "items": ["i7abc123"],
    "boxes": [],
    "places": []
  }
}
```

#### 3.3.2. AI Interaction Object Fields

| Field | Type | Description |
|-------|------|-------------|
| `type` | String | Interaction type: `"question"`, `"action_taken"`, `"confirmation"`, or `"info"` |
| `message` | String | Full AI response text for display |
| `requiresResponse` | Boolean | Whether user input is required to proceed |
| `suggestedActions` | Array<String> | Suggested action buttons (e.g., `["yes", "no", "cancel"]`) |
| `toolCallsMade` | Number | Number of database operations AI performed (0 = no action taken) |
| `summary` | String | Short version of message (first sentence, max 100 chars) for compact UI |

#### 3.3.3. Interaction Types & Expected UI Behavior

##### Type: `question`

**When:** AI asks for user clarification about an unknown code.

**Example Scenario:** User scans barcode "ABC123" during internal inventory movement (not receiving).

**Sample Response:**
```json
{
  "type": "question",
  "message": "I don't recognize 'ABC123'. Is this a new product code, or did you mean to scan something else?",
  "requiresResponse": true,
  "suggestedActions": ["yes", "no", "cancel"],
  "toolCallsMade": 0,
  "summary": "I don't recognize 'ABC123'"
}
```

**Expected UI:**
- Show alert dialog with the AI's question
- Display action buttons: "Yes" / "No" / "Cancel"
- Block further scans until user responds
- Send user response back to server if needed (future endpoint)

**UI Mockup:**
```
┌─────────────────────────────────────┐
│  🤖 AI Assistant                    │
├─────────────────────────────────────┤
│                                     │
│  I don't recognize 'ABC123'.        │
│  Is this a new product code, or     │
│  did you mean to scan something     │
│  else?                              │
│                                     │
├─────────────────────────────────────┤
│  [Yes]    [No]    [Cancel]          │
└─────────────────────────────────────┘
```

---

##### Type: `action_taken`

**When:** AI successfully linked an external code to an internal ID automatically.

**Example Scenario:** User scans EAN barcode during receiving (high-trust context).

**Sample Response:**
```json
{
  "type": "action_taken",
  "message": "Linked EAN 9780123456789 to i7abc123. This barcode is now associated with the current item.",
  "requiresResponse": false,
  "suggestedActions": [],
  "toolCallsMade": 2,
  "summary": "Linked EAN 9780123456789 to i7abc123"
}
```

**Expected UI:**
- Show success toast notification (green)
- Display checkmark icon ✅
- Auto-dismiss after 3-5 seconds
- Do NOT block further scans

**UI Mockup:**
```
┌────────────────────────────────┐
│ ✅ Linked EAN 9780123456789    │
│    to i7abc123                 │
└────────────────────────────────┘
```

---

##### Type: `confirmation`

**When:** AI suggests an action but needs user approval before executing.

**Example Scenario:** AI detected a DHL tracking number and wants to link it.

**Sample Response:**
```json
{
  "type": "confirmation",
  "message": "This appears to be a DHL tracking number. Do you want to confirm linking it to box b888?",
  "requiresResponse": true,
  "suggestedActions": ["confirm", "cancel"],
  "toolCallsMade": 1,
  "summary": "This appears to be a DHL tracking number"
}
```

**Expected UI:**
- Show confirmation dialog
- Display action buttons: "Confirm" / "Cancel"
- If confirmed, send acknowledgment to server (future endpoint)
- If canceled, discard the suggestion

**UI Mockup:**
```
┌─────────────────────────────────────┐
│  ⚠️  Confirm Action                 │
├─────────────────────────────────────┤
│                                     │
│  This appears to be a DHL tracking  │
│  number. Do you want to confirm     │
│  linking it to box b888?            │
│                                     │
├─────────────────────────────────────┤
│      [Confirm]    [Cancel]          │
└─────────────────────────────────────┘
```

---

##### Type: `info`

**When:** AI provides informational feedback without requiring action.

**Example Scenario:** AI analyzed the barcode but couldn't determine its type.

**Sample Response:**
```json
{
  "type": "info",
  "message": "This code format doesn't match any known patterns. It might be a custom internal code.",
  "requiresResponse": false,
  "suggestedActions": [],
  "toolCallsMade": 0,
  "summary": "This code format doesn't match any known patterns"
}
```

**Expected UI:**
- Show info toast notification (blue/gray)
- Display info icon ℹ️
- Auto-dismiss after 4-6 seconds
- Allow further scans immediately

**UI Mockup:**
```
┌────────────────────────────────┐
│ ℹ️  This code format doesn't   │
│    match any known patterns    │
└────────────────────────────────┘
```

---

#### 3.3.4. Implementation Checklist for Android Client

**Mandatory Requirements:**

- [ ] Check for `ai_interaction` field in scan response `data`
- [ ] Handle all 4 interaction types: `question`, `action_taken`, `confirmation`, `info`
- [ ] Respect `requiresResponse` flag (block/allow further scans accordingly)
- [ ] Implement dialog UI for `question` and `confirmation` types
- [ ] Implement toast/snackbar UI for `action_taken` and `info` types
- [ ] Use `summary` field for compact notifications, `message` for full dialogs
- [ ] Display suggested action buttons from `suggestedActions` array
- [ ] Log `toolCallsMade` for analytics/debugging
- [ ] Gracefully handle missing `ai_interaction` field (backward compatibility)

**Recommended Kotlin Implementation Pattern:**

```kotlin
data class AiInteraction(
    val type: String, // "question" | "action_taken" | "confirmation" | "info"
    val message: String,
    val requiresResponse: Boolean,
    val suggestedActions: List<String>,
    val toolCallsMade: Int,
    val summary: String
)

fun handleScanResponse(response: ScanResponse) {
    val aiInteraction = response.data?.ai_interaction

    if (aiInteraction != null) {
        when (aiInteraction.type) {
            "question" -> showQuestionDialog(aiInteraction)
            "action_taken" -> showSuccessToast(aiInteraction.summary)
            "confirmation" -> showConfirmationDialog(aiInteraction)
            "info" -> showInfoToast(aiInteraction.summary)
            else -> Log.w("AI", "Unknown interaction type: ${aiInteraction.type}")
        }

        // Block further scans if response required
        if (aiInteraction.requiresResponse) {
            scannerDriver.pause()
        }
    } else {
        // Legacy behavior: No AI interaction, handle normally
        handleLegacyScanResponse(response)
    }
}
```

#### 3.3.5. Context Awareness: Trust Levels

The AI uses **context-aware logic** to determine trust levels for unknown codes:

**Receiving Context (High Trust):**
- Multiple unknown codes scanned in quick succession
- Active box buffer (`bOx` array not empty)
- Codes match shipping/manufacturer patterns (DHL, EAN, UPC)
- **Behavior:** AI automatically links codes without asking (responds with `action_taken`)

**Internal Operations Context (Low Trust):**
- Single unknown code scanned
- Active item buffer (`iTem` array not empty)
- Code doesn't match known patterns
- **Behavior:** AI questions the code before linking (responds with `question` or `confirmation`)

**Client Implication:**
- The client does NOT need to detect context
- The server AI determines context and returns appropriate interaction type
- Client simply renders UI based on `ai_interaction.type`

---

#### 3.3.6. Testing AI Interactions

**Test Scripts Available:**

```bash
# Test AI response parsing logic
node tests/ai/test-ai-response-parsing.js

# Full integration test with database
node tests/ai/test-ai-aliasing.js

# Basic Gemini API connectivity test
node tests/ai/test-gemini-integration.js
```

**Manual Test Scenarios:**

1. **Receiving Scenario:**
   - Scan a box barcode (creates active box buffer)
   - Scan unknown code "DHL123456789"
   - Expected: `action_taken` response, automatic linking

2. **Internal Ops Scenario:**
   - Scan an item barcode (creates active item buffer)
   - Scan unknown code "UNKNOWN999"
   - Expected: `question` response, request for clarification

3. **No Context Scenario:**
   - Scan unknown code with empty buffers
   - Expected: `info` response with analysis

---

**End of AI Interactive Responses Documentation**
