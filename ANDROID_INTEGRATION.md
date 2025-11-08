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

-   **`LOCAL_SERVER_URL`**: The IP address and port of the server running on the local network (e.g., `http://192.168.1.100:3000`).
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
