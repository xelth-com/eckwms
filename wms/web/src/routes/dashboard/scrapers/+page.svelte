<script>
    import { onMount } from "svelte";
    import { api } from "$lib/api";
    import { toastStore } from "$lib/stores/toastStore.js";

    export let data;

    let syncHistory = data.syncHistory || [];
    let loading = false;
    let error = data.error || null;
    let activeTab = "scraper"; // 'scraper', 'sync', 'database'

    // ── Scraper Admin state ──────────────────────────────────────────────────
    let scraperStatus = null;
    let scraperOnline = null;
    let scraperStarting = false;
    let scraperStartError = null;

    let opalDebug = false;
    let opalLimit = 10;
    let opalRunning = false;
    let opalResult = null;

    let dhlDebug = false;
    let dhlLimit = 10;
    let dhlRunning = false;
    let dhlResult = null;

    let opalJsonOpen = false;
    let dhlJsonOpen = false;

    let exactEntityType = 'items';
    let exactLimit = 0;
    let exactDebug = false;
    let exactStartPage = 1;
    let exactDelayMs = 3000;
    let exactRunning = false;
    let exactResult = null;
    let exactJsonOpen = false;
    let exactImportRunning = false;
    let exactImportResult = null;

    let zohoDebug = false;
    let zohoRunning = false;
    let zohoLimit = 10;
    let zohoResult = null;
    let zohoJsonOpen = false;

    let zohoThreadTicketId = '';
    let zohoThreadRunning = false;
    let zohoThreadResult = null;
    let zohoThreadJsonOpen = false;

    let zohoImportRunning = false;
    let zohoImportResult = null;

    let zohoImportAllRunning = false;
    let zohoImportAllProgress = '';
    let zohoImportAllStats = { imported: 0, skipped: 0, errors: 0, current: 0, total: 0 };
    let zohoImportAllResult = null;
    let zohoImportDelay = 3000;

    let zohoSaveTicketsRunning = false;
    let zohoSaveTicketsResult = null;
    let zohoSyncRunning = false;
    let zohoSyncResult = null;

    let expandedSyncLogs = new Set();

    // ── Database Backup state ────────────────────────────────────────────────
    let dbBackups = [];
    let dbBackupsLoading = false;
    let dbBackupRunning = false;
    let dbRestoreRunning = null; // filename being restored, or null

    // ── Excel Sync state ───────────────────────────────────────────────────
    let excelInfo = null;
    let excelInfoLoading = false;
    let excelRepairs = [];
    let excelTotal = 0;
    let excelLoading = false;
    let excelError = null;
    let excelLimit = 30;
    let excelJsonOpen = false;
    let excelSelected = new Set();
    let excelImporting = false;
    let excelImportResult = null;
    let excelDbRepairs = [];
    let excelDbLoading = false;
    let excelDbSelected = new Set();
    let excelExporting = false;
    let excelExportResult = null;
    let excelMode = 'import'; // 'import' or 'export'

    let conflictModalOpen = false;
    let currentConflict = null;
    let excelSourcePath = '';
    let excelExportPath = '';
    let excelConfigLoaded = false;
    let excelConfigSaving = false;
    let excelImportAllRunning = false;
    let excelImportAllProgress = '';

    onMount(async () => {
        if (scraperOnline === null) {
            await loadScraperStatus();
        }
    });

    async function loadData() {
        loading = true;
        error = null;
        try {
            syncHistory = await api.get("/api/delivery/sync/history") || [];
            if (activeTab === 'scraper') {
                await loadScraperStatus();
            }
        } catch (e) {
            console.error(e);
            error = e.message;
        } finally {
            loading = false;
        }
    }

    async function loadScraperStatus() {
        try {
            scraperStatus = await api.get('/S/debug');
            scraperOnline = true;
        } catch {
            scraperOnline = false;
            scraperStatus = null;
        }
    }

    async function startScraper() {
        scraperStarting = true;
        scraperStartError = null;
        try {
            const res = await api.post('/api/scraper/start', {});
            if (res.success) {
                toastStore.add(res.message, 'success');
                // Poll until scraper is reachable
                for (let i = 0; i < 10; i++) {
                    await new Promise(r => setTimeout(r, 2000));
                    try {
                        scraperStatus = await api.get('/S/debug');
                        scraperOnline = true;
                        scraperStarting = false;
                        return;
                    } catch {}
                }
                scraperStartError = 'Process started but scraper did not become reachable within 20s. Check server logs.';
                scraperOnline = false;
            } else {
                scraperStartError = res.error || 'Unknown error';
            }
        } catch (e) {
            scraperStartError = e.message || 'Failed to call start endpoint';
        } finally {
            scraperStarting = false;
        }
    }

    async function copyStartError() {
        const txt = `# eckWMS Scraper Start Error
You are a technical assistant for eckWMS (Rust warehouse management system).
The user tried to start the Playwright scraper from the admin UI.

## System
- eckWMS: Rust (axum) + SvelteKit + PostgreSQL on port 3210
- Scraper: Node.js + Playwright, expected on port 3211
- Start: node scraper/server.js (from project root)
- Scraper waits for main server /E/health before listening

## Error
${scraperStartError}

## Possible causes
- Node.js not in PATH
- scraper/server.js not found (wrong working directory)
- Port 3211 already in use
- Main server /E/health not responding (scraper exits after 60s)
- Missing deps (cd scraper && npm install)

Analyze and suggest a fix. Be concise.`.trim();
        try {
            await navigator.clipboard.writeText(txt);
            toastStore.add('Error copied for AI analysis', 'success');
        } catch (err) {
            toastStore.add('Failed to copy: ' + err.message, 'error');
        }
    }

    async function testOpalFetch() {
        opalRunning = true;
        opalResult = null;
        opalJsonOpen = false;
        const t0 = Date.now();
        try {
            const res = await api.post('/S/api/opal/fetch', {
                username: '',
                password: '',
                limit: opalLimit,
                debug: opalDebug,
                _from_env: true
            });
            opalResult = { ...res, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } catch (e) {
            opalResult = { success: false, error: e.message, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } finally {
            opalRunning = false;
        }
    }

    async function testDhlFetch() {
        dhlRunning = true;
        dhlResult = null;
        dhlJsonOpen = false;
        const t0 = Date.now();
        try {
            const res = await api.post('/S/api/dhl/fetch', {
                username: '',
                password: '',
                limit: dhlLimit,
                debug: dhlDebug,
                _from_env: true
            });
            dhlResult = { ...res, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } catch (e) {
            dhlResult = { success: false, error: e.message, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } finally {
            dhlRunning = false;
        }
    }

    async function testExactFetch() {
        exactRunning = true;
        exactResult = null;
        exactJsonOpen = false;
        exactImportResult = null;
        const t0 = Date.now();
        let endpoint = `/S/api/exact/${exactEntityType}/fetch`;
        if (exactEntityType === 'quotations-lines') endpoint = '/S/api/exact/quotations/fetch-with-lines';

        try {
            const res = await api.post(endpoint, { _from_env: true, debug: exactDebug, limit: exactLimit || undefined, startPage: exactStartPage, delayMs: exactDelayMs });
            exactResult = { ...res, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } catch (e) {
            exactResult = { success: false, error: e.message, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } finally {
            exactRunning = false;
        }
    }

    async function importExactData() {
        if (!exactResult?.rows?.length) return;
        exactImportRunning = true;
        exactImportResult = null;
        try {
            let endpoint = '';
            let payload = {};
            if (exactEntityType === 'items') {
                endpoint = '/api/exact/import-items';
                payload = { items: exactResult.rows };
            } else if (exactEntityType === 'stock-positions') {
                endpoint = '/api/exact/import-stock-positions';
                payload = { positions: exactResult.rows, fullSync: false };
            } else if (exactEntityType === 'quotations' || exactEntityType === 'quotations-lines') {
                endpoint = '/api/exact/import-quotations';
                payload = { quotations: exactResult.rows };
            } else if (exactEntityType === 'sales-orders') {
                endpoint = '/api/exact/import-sales-orders';
                payload = { orders: exactResult.rows };
            } else if (exactEntityType === 'customers') {
                endpoint = '/api/exact/import-customers';
                payload = { customers: exactResult.rows };
            }

            const res = await api.post(endpoint, payload);
            exactImportResult = res;
            toastStore.add(`Imported ${res.imported} records to DB`, 'success');
        } catch (e) {
            toastStore.add('Import failed: ' + e.message, 'error');
        } finally {
            exactImportRunning = false;
        }
    }

    async function testZohoFetch() {
        zohoRunning = true;
        zohoResult = null;
        zohoJsonOpen = false;
        const t0 = Date.now();
        try {
            const res = await api.post('/S/api/zoho/tickets', { limit: zohoLimit, _from_env: true, debug: zohoDebug });
            zohoResult = { ...res, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } catch (e) {
            zohoResult = { success: false, error: e.message, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } finally {
            zohoRunning = false;
        }
    }

    async function importThreadsToSystem() {
        if (!zohoThreadResult?.threads?.length) return;
        zohoImportRunning = true;
        zohoImportResult = null;
        try {
            // Pass ticket metadata from thread fetch or from ticket list
            const ticket = zohoThreadResult.ticket
                || zohoResult?.tickets?.find(t => t.id === zohoThreadTicketId)
                || null;
            const res = await api.post('/api/support/import-thread', {
                ticketId: zohoThreadTicketId,
                threads: zohoThreadResult.threads,
                ticket,
            });
            zohoImportResult = res;
            if (res.imported > 0) {
                toastStore.add(`Imported ${res.imported} thread(s) to system`, 'success');
            } else {
                toastStore.add('Import finished with errors', 'error');
            }
        } catch (e) {
            zohoImportResult = { success: false, imported: 0, errors: [e.message] };
            toastStore.add('Import failed: ' + e.message, 'error');
        } finally {
            zohoImportRunning = false;
        }
    }

    async function importAllTickets() {
        const tickets = zohoResult?.tickets;
        if (!tickets?.length) return;
        zohoImportAllRunning = true;
        zohoImportAllResult = null;
        zohoImportAllStats = { imported: 0, skipped: 0, errors: 0, current: 0, total: tickets.length };

        let threadCount = 0;
        const errorList = [];
        const delay = zohoImportDelay;

        // Phase 1: save all ticket metadata to DB + get sync statuses
        zohoImportAllProgress = `Saving ${tickets.length} ticket metadata…`;
        let syncedIds = new Set();
        try {
            const metaRes = await api.post('/api/support/import-tickets', { tickets });
            if (metaRes.statuses) {
                for (const [tid, status] of Object.entries(metaRes.statuses)) {
                    if (status === 'synced') syncedIds.add(tid);
                }
            }
        } catch (e) {
            errorList.push(`Metadata save failed: ${e.message}`);
            zohoImportAllStats.errors++;
        }

        const needsSync = tickets.filter(t => !syncedIds.has(t.id));
        zohoImportAllStats = { ...zohoImportAllStats, total: needsSync.length,
            skipped: tickets.length - needsSync.length };
        if (syncedIds.size > 0) {
            zohoImportAllProgress = `Skipping ${syncedIds.size} already synced, ${needsSync.length} to process…`;
            await new Promise(r => setTimeout(r, 1000));
        }

        // Phase 2: for each unsynced ticket, fetch threads via scraper → save to DB
        for (let i = 0; i < needsSync.length; i++) {
            const t = needsSync[i];
            const ticketNum = t.ticketNumber || t.id;
            zohoImportAllStats = { ...zohoImportAllStats, current: i + 1 };
            zohoImportAllProgress = `#${ticketNum}: fetching threads…`;

            try {
                const threadRes = await api.post('/S/api/zoho/ticket-threads', {
                    ticketId: t.id,
                    _from_env: true,
                });

                if (!threadRes.success || !threadRes.threads?.length) {
                    zohoImportAllStats = { ...zohoImportAllStats, skipped: zohoImportAllStats.skipped + 1 };
                    zohoImportAllProgress = `#${ticketNum}: no threads, skipped`;
                } else {
                    zohoImportAllProgress = `#${ticketNum}: saving ${threadRes.threads.length} threads…`;
                    const saveRes = await api.post('/api/support/import-thread', {
                        ticketId: t.id,
                        threads: threadRes.threads,
                        ticket: threadRes.ticket || t,
                    });

                    if (saveRes.imported > 0) {
                        zohoImportAllStats = { ...zohoImportAllStats, imported: zohoImportAllStats.imported + 1 };
                        threadCount += saveRes.imported;
                    } else {
                        errorList.push(`#${ticketNum}: 0 threads saved`);
                        zohoImportAllStats = { ...zohoImportAllStats, errors: zohoImportAllStats.errors + 1 };
                    }

                    if (saveRes.errors?.length) {
                        for (const err of saveRes.errors) errorList.push(`#${ticketNum}: ${err}`);
                    }
                }
            } catch (e) {
                errorList.push(`#${ticketNum}: ${e.message}`);
                zohoImportAllStats = { ...zohoImportAllStats, errors: zohoImportAllStats.errors + 1 };
            }

            if (i + 1 < needsSync.length) {
                await new Promise(r => setTimeout(r, delay));
            }
        }

        zohoImportAllProgress = '';
        zohoImportAllResult = { success: errorList.length === 0, imported: zohoImportAllStats.imported, skipped: zohoImportAllStats.skipped, threadCount, total: tickets.length, errors: errorList };
        zohoImportAllRunning = false;

        if (zohoImportAllStats.imported > 0) {
            toastStore.add(`Imported ${threadCount} threads from ${zohoImportAllStats.imported} tickets (${syncedIds.size} already synced)`, 'success');
        } else if (zohoImportAllStats.skipped === tickets.length) {
            toastStore.add('All tickets skipped (no threads found)', 'warning');
        } else {
            toastStore.add('Import finished with errors', 'error');
        }
    }

    async function saveTicketsToDB() {
        const tickets = zohoResult?.tickets;
        if (!tickets?.length) return;
        zohoSaveTicketsRunning = true;
        zohoSaveTicketsResult = null;
        try {
            const res = await api.post('/api/support/import-tickets', { tickets });
            zohoSaveTicketsResult = res;
            toastStore.add(`Saved ${res.created} new, updated ${res.updated} tickets`, 'success');
        } catch (e) {
            zohoSaveTicketsResult = { success: false, created: 0, updated: 0, errors: [e.message] };
            toastStore.add('Save tickets failed: ' + e.message, 'error');
        } finally {
            zohoSaveTicketsRunning = false;
        }
    }

    async function syncMissingThreads() {
        zohoSyncRunning = true;
        zohoSyncResult = null;
        zohoImportAllStats = { imported: 0, skipped: 0, errors: 0, current: 0, total: 0 };
        zohoImportAllProgress = '';

        let threadCount = 0;
        const errorList = [];
        const delay = zohoImportDelay;

        try {
            // If we have fetched tickets, use them; otherwise load from DB
            let tickets;
            if (zohoResult?.tickets?.length) {
                // Save metadata first, get statuses
                zohoImportAllProgress = `Saving ${zohoResult.tickets.length} ticket metadata…`;
                const metaRes = await api.post('/api/support/import-tickets', { tickets: zohoResult.tickets });
                const syncedIds = new Set();
                if (metaRes.statuses) {
                    for (const [tid, status] of Object.entries(metaRes.statuses)) {
                        if (status === 'synced') syncedIds.add(tid);
                    }
                }
                tickets = zohoResult.tickets
                    .filter(t => !syncedIds.has(t.id))
                    .map(t => ({ id: t.id, ticketNumber: t.ticketNumber || t.id }));
                zohoImportAllStats.skipped = syncedIds.size;
            } else {
                // No fetch — load unsynced tickets from DB via import-tickets with empty array
                zohoImportAllProgress = 'Loading unsynced tickets from DB…';
                const metaRes = await api.post('/api/support/import-tickets', { tickets: [] });
                tickets = [];
                if (metaRes.statuses) {
                    for (const [tid, status] of Object.entries(metaRes.statuses)) {
                        if (status === 'synced') {
                            zohoImportAllStats.skipped++;
                        } else {
                            tickets.push({ id: tid, ticketNumber: tid });
                        }
                    }
                }
            }

            zohoImportAllStats = { ...zohoImportAllStats, total: tickets.length };

            if (tickets.length === 0) {
                zohoSyncResult = { success: true, tickets_synced: 0, threads_imported: 0, tickets_remaining: 0, errors: [] };
                toastStore.add('All tickets are fully synced!', 'success');
                zohoSyncRunning = false;
                zohoImportAllProgress = '';
                return;
            }

            zohoImportAllProgress = `${tickets.length} tickets to sync (${zohoImportAllStats.skipped} already done)…`;
            await new Promise(r => setTimeout(r, 1000));

            for (let i = 0; i < tickets.length; i++) {
                const t = tickets[i];
                const ticketNum = t.ticketNumber;
                zohoImportAllStats = { ...zohoImportAllStats, current: i + 1 };
                zohoImportAllProgress = `#${ticketNum}: fetching threads…`;

                try {
                    const threadRes = await api.post('/S/api/zoho/ticket-threads', {
                        ticketId: t.id,
                        _from_env: true,
                    });

                    if (!threadRes.success || !threadRes.threads?.length) {
                        zohoImportAllStats = { ...zohoImportAllStats, skipped: zohoImportAllStats.skipped + 1 };
                    } else {
                        zohoImportAllProgress = `#${ticketNum}: saving ${threadRes.threads.length} threads…`;
                        const saveRes = await api.post('/api/support/import-thread', {
                            ticketId: t.id,
                            threads: threadRes.threads,
                            ticket: threadRes.ticket || null,
                        });

                        if (saveRes.imported > 0) {
                            zohoImportAllStats = { ...zohoImportAllStats, imported: zohoImportAllStats.imported + 1 };
                            threadCount += saveRes.imported;
                        } else {
                            errorList.push(`#${ticketNum}: 0 threads saved`);
                            zohoImportAllStats = { ...zohoImportAllStats, errors: zohoImportAllStats.errors + 1 };
                        }

                        if (saveRes.errors?.length) {
                            for (const err of saveRes.errors) errorList.push(`#${ticketNum}: ${err}`);
                        }
                    }
                } catch (e) {
                    errorList.push(`#${ticketNum}: ${e.message}`);
                    zohoImportAllStats = { ...zohoImportAllStats, errors: zohoImportAllStats.errors + 1 };
                }

                if (i + 1 < tickets.length) {
                    await new Promise(r => setTimeout(r, delay));
                }
            }
        } catch (e) {
            errorList.push(e.message);
        }

        zohoImportAllProgress = '';
        zohoSyncResult = {
            success: errorList.length === 0,
            tickets_synced: zohoImportAllStats.imported,
            threads_imported: threadCount,
            tickets_remaining: 0,
            errors: errorList,
        };
        zohoSyncRunning = false;

        if (zohoImportAllStats.imported > 0) {
            toastStore.add(`Synced ${zohoImportAllStats.imported} tickets (${threadCount} threads)`, 'success');
        } else {
            toastStore.add('Sync finished — nothing new to sync', 'success');
        }
    }

    async function testZohoFetchThreads() {
        if (!zohoThreadTicketId) return;
        zohoThreadRunning = true;
        zohoThreadResult = null;
        zohoThreadJsonOpen = false;
        zohoImportResult = null;
        const t0 = Date.now();
        try {
            const res = await api.post('/S/api/zoho/ticket-threads', { ticketId: zohoThreadTicketId, _from_env: true });
            zohoThreadResult = { ...res, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } catch (e) {
            zohoThreadResult = { success: false, error: e.message, duration: ((Date.now() - t0) / 1000).toFixed(1) };
        } finally {
            zohoThreadRunning = false;
        }
    }

    function formatDate(dateStr) {
        if (!dateStr) return "-";
        return new Date(dateStr).toLocaleDateString("de-DE", {
            day: "2-digit",
            month: "2-digit",
            year: "numeric",
            hour: "2-digit",
            minute: "2-digit",
        });
    }

    function toggleSyncDetails(id) {
        if (expandedSyncLogs.has(id)) {
            expandedSyncLogs.delete(id);
        } else {
            expandedSyncLogs.add(id);
        }
        expandedSyncLogs = expandedSyncLogs;
    }

    function summarizeError(error) {
        if (!error) return 'Unknown error';
        const e = String(error).toLowerCase();
        if (e.includes('timeout') || e.includes('timed out')) return 'Timeout';
        if (e.includes('econnrefused') || e.includes('connection refused')) return 'Connection refused';
        if (e.includes('navigation') || e.includes('goto')) return 'Navigation failed';
        if (e.includes('selector') || e.includes('locator')) return 'Element not found';
        if (e.includes('login') || e.includes('auth') || e.includes('session')) return 'Auth failed';
        if (e.includes('captcha') || e.includes('2fa')) return '2FA/Captcha';
        if (e.includes('network') || e.includes('dns') || e.includes('fetch')) return 'Network error';
        if (e.includes('certificate') || e.includes('ssl')) return 'SSL error';
        if (e.includes('403') || e.includes('forbidden')) return 'Forbidden';
        if (e.includes('404') || e.includes('not found')) return 'Not found';
        if (e.includes('500') || e.includes('server error')) return 'Server error';
        if (e.includes('rate') || e.includes('limit') || e.includes('throttl')) return 'Rate limited';
        const short = String(error).split('\n')[0].substring(0, 60);
        return short.length < String(error).length ? short + '...' : short;
    }

    async function copyScraperError(provider, result) {
        const debugText = `
# eckWMS Scraper Error — ${provider}
You are a technical assistant for eckWMS (warehouse management system). This is an error from the Playwright-based scraper service.

## System
- eckWMS: Rust (axum) + SvelteKit + PostgreSQL
- Scraper: Node.js + Playwright on port 3211, proxied at /E/S/*
- Providers: OPAL (courier), DHL (shipping), Zoho Desk (tickets), Exact Online (ERP)

## Error
**Provider:** ${provider}
**Short:** ${summarizeError(result.error)}
**Full message:** ${result.error || 'No error message'}

## Result JSON
${JSON.stringify(result, null, 2)}

---
Analyze this error and suggest a fix. Be concise.
`.trim();

        try {
            await navigator.clipboard.writeText(debugText);
            toastStore.add('Error copied for AI analysis', 'success');
        } catch (err) {
            toastStore.add('Failed to copy: ' + err.message, 'error');
        }
    }

    // ── Excel Sync functions ─────────────────────────────────────────────
    async function loadExcelInfo() {
        excelInfoLoading = true;
        try {
            excelInfo = await api.get('/S/api/excel/info');
        } catch (e) {
            excelInfo = { success: false, error: e.message };
        } finally {
            excelInfoLoading = false;
        }
        // Also load config if not yet loaded
        if (!excelConfigLoaded) loadExcelConfig();
    }

    async function loadExcelConfig() {
        try {
            const cfg = await api.get('/S/api/excel/config');
            if (cfg.success) {
                excelSourcePath = cfg.sourcePath || '';
                excelExportPath = cfg.exportPath || '';
                excelConfigLoaded = true;
            }
        } catch {}
    }

    async function saveExcelConfig() {
        excelConfigSaving = true;
        try {
            const res = await api.put('/S/api/excel/config', {
                sourcePath: excelSourcePath,
                exportPath: excelExportPath,
            });
            if (res.success) {
                excelSourcePath = res.sourcePath;
                excelExportPath = res.exportPath;
                toastStore.add('Excel paths saved', 'success');
            } else {
                toastStore.add('Failed to save: ' + res.error, 'error');
            }
        } catch (e) {
            toastStore.add('Failed to save config: ' + e.message, 'error');
        } finally {
            excelConfigSaving = false;
        }
    }

    function getConflicts(dbR, exR) {
        const conflicts = [];
        const safeUpdates = [];

        const checkField = (name, dbVal, exVal, payloadKey) => {
            const dbStr = (dbVal === null || dbVal === undefined ? '' : dbVal).toString().trim();
            const exStr = (exVal === null || exVal === undefined ? '' : exVal).toString().trim();

            if (exStr) {
                if (!dbStr || dbStr === 'in_progress') {
                    safeUpdates.push({ key: payloadKey, val: exVal });
                } else if (dbStr !== exStr) {
                    conflicts.push({ field: name, db: dbStr, ex: exStr, key: payloadKey, exRaw: exVal });
                }
            }
        };

        checkField('Issue Description', dbR.issueDescription, exR.errorDescription, 'issueDescription');
        checkField('Resolution', dbR.resolution, exR.troubleshooting, 'resolution');
        checkField('Status', dbR.status, exR.status, 'status');
        checkField('Product Model', dbR.productName, exR.model, 'productName');
        checkField('Serial Number', dbR.serialNumber, exR.serialNumber, 'serialNumber');
        checkField('Customer Name', dbR.customerName, exR.customerName, 'customerName');

        const dbDate = dbR.startedAt ? dbR.startedAt.slice(0, 10) : '';
        const exDate = exR.dateOfReceipt || '';
        checkField('Date of Receipt', dbDate, exDate, 'startedAt');

        return { conflicts, safeUpdates };
    }

    async function readExcelRepairs() {
        excelLoading = true;
        excelError = null;
        excelRepairs = [];
        excelSelected = new Set();
        excelImportResult = null;
        try {
            const dbList = await api.get('/api/rma?type=repair');
            const dbMap = new Map((dbList || []).map(r => [r.orderNumber, r]));

            const res = await api.post('/S/api/excel/read', { limit: excelLimit, offset: 0 });
            if (res.success) {
                const enriched = [];
                for (const exR of res.repairs) {
                    const dbR = dbMap.get(exR.repairNumber);
                    if (!dbR) {
                        enriched.push({ ...exR, _importStatus: 'New', _conflicts: [], _safeUpdates: [] });
                    } else {
                        const { conflicts, safeUpdates } = getConflicts(dbR, exR);
                        if (conflicts.length > 0) {
                            enriched.push({ ...exR, _importStatus: 'Conflict', _conflicts: conflicts, _safeUpdates: safeUpdates, _dbR: dbR });
                        } else if (safeUpdates.length > 0) {
                            enriched.push({ ...exR, _importStatus: 'Auto-fill', _conflicts: [], _safeUpdates: safeUpdates, _dbR: dbR });
                        } else {
                            enriched.push({ ...exR, _importStatus: 'Unchanged', _conflicts: [], _safeUpdates: [], _dbR: dbR });
                        }
                    }
                }
                excelRepairs = enriched;
                excelTotal = res.total;

                excelRepairs.forEach(r => {
                    if (r._importStatus === 'New' || r._importStatus === 'Auto-fill') {
                        excelSelected.add(r.repairNumber);
                    }
                });
                excelSelected = excelSelected;
            } else {
                excelError = res.error;
            }
        } catch (e) {
            excelError = e.message;
        } finally {
            excelLoading = false;
        }
    }

    async function scanDbChangesForExcel() {
        excelDbLoading = true;
        excelDbSelected = new Set();
        excelExportResult = null;
        try {
            // 1. Fetch all DB repairs
            const dbRes = await api.get('/api/rma?type=repair');
            const dbRepairs = (dbRes || []).filter(r => r.orderNumber && r.orderNumber.startsWith('CS-'));

            // 2. Fetch Excel repairs (high limit to get all)
            const exRes = await api.post('/S/api/excel/read', { limit: 10000, offset: 0 });
            const exRepairs = exRes.success ? exRes.repairs : [];
            const exMap = new Map(exRepairs.map(r => [r.repairNumber, r]));

            const changes = [];

            for (const dbR of dbRepairs) {
                const exR = exMap.get(dbR.orderNumber);

                if (!exR) {
                    changes.push({ ...dbR, _changeType: 'New', _diffs: ['Record missing in Excel'] });
                    continue;
                }

                // 3. Compare fields
                const diffs = [];
                const dbStatus = dbR.status === 'completed' ? 'completed' : 'in_progress';
                if (dbStatus !== exR.status) {
                    diffs.push(`Status: ${exR.status === 'completed' ? 'Done' : 'WIP'} ➔ ${dbStatus === 'completed' ? 'Done' : 'WIP'}`);
                }

                const dbReso = (dbR.resolution || '').trim();
                const exReso = (exR.troubleshooting || '').trim();
                if (dbReso !== exReso && dbReso !== '') {
                    diffs.push('Resolution updated');
                }

                const dbDesc = (dbR.issueDescription || '').trim();
                const exDesc = (exR.errorDescription || '').trim();
                if (dbDesc !== exDesc && dbDesc !== '') {
                    diffs.push('Issue updated');
                }

                if (diffs.length > 0) {
                    changes.push({ ...dbR, _changeType: 'Update', _diffs: diffs });
                }
            }

            excelDbRepairs = changes;
            // Auto-select all changes by default
            excelDbSelected = new Set(changes.map(r => r.orderNumber));
        } catch (e) {
            excelDbRepairs = [];
            toastStore.add('Failed to scan changes: ' + e.message, 'error');
        } finally {
            excelDbLoading = false;
        }
    }

    function toggleExcelSelect(repairNumber) {
        if (excelSelected.has(repairNumber)) {
            excelSelected.delete(repairNumber);
        } else {
            excelSelected.add(repairNumber);
        }
        excelSelected = excelSelected;
    }

    function toggleExcelSelectAll() {
        const selectable = excelRepairs.filter(r => r._importStatus !== 'Conflict');
        if (excelSelected.size === selectable.length) {
            excelSelected = new Set();
        } else {
            excelSelected = new Set(selectable.map(r => r.repairNumber));
        }
    }

    function openConflictModal(repair) {
        currentConflict = repair;
        conflictModalOpen = true;
    }

    function resolveConflict() {
        currentConflict._importStatus = 'Resolved';
        excelSelected.add(currentConflict.repairNumber);
        excelSelected = excelSelected;
        excelRepairs = [...excelRepairs];
        conflictModalOpen = false;
    }

    function toggleDbSelect(orderNumber) {
        if (excelDbSelected.has(orderNumber)) {
            excelDbSelected.delete(orderNumber);
        } else {
            excelDbSelected.add(orderNumber);
        }
        excelDbSelected = excelDbSelected;
    }

    function toggleDbSelectAll() {
        if (excelDbSelected.size === excelDbRepairs.length) {
            excelDbSelected = new Set();
        } else {
            excelDbSelected = new Set(excelDbRepairs.map(r => r.orderNumber));
        }
    }

    async function importSelectedToDb() {
        if (excelSelected.size === 0) return;
        excelImporting = true;
        excelImportResult = null;
        let created = 0, updated = 0;
        const errors = [];

        for (const rn of excelSelected) {
            const repair = excelRepairs.find(r => r.repairNumber === rn);
            if (!repair) continue;
            try {
                let payload = {};

                if (repair._dbR) {
                    // Update existing record (Auto-fill or Resolved Conflict)
                    payload = { ...repair._dbR };

                    (repair._safeUpdates || []).forEach(su => { payload[su.key] = su.val; });

                    if (repair._importStatus === 'Resolved') {
                        (repair._conflicts || []).forEach(c => { payload[c.key] = c.exRaw; });
                    }

                    if (payload.startedAt && !payload.startedAt.includes('T')) {
                        const d = new Date(payload.startedAt);
                        if (!isNaN(d.getTime())) payload.startedAt = d.toISOString();
                    }

                    payload.metadata = {
                        ...(payload.metadata || {}),
                        ticketNumber: repair.ticketNumber,
                        excelRow: repair.excelRow,
                        importedFromExcel: true,
                    };

                    await api.put(`/api/rma/${repair._dbR.id}`, payload);
                    updated++;
                } else {
                    // Create New Record
                    payload = {
                        orderNumber: repair.repairNumber,
                        orderType: 'repair',
                        customerName: repair.customerName || '',
                        customerEmail: '',
                        customerPhone: '',
                        productSku: '',
                        productName: repair.model || '',
                        serialNumber: repair.serialNumber || '',
                        issueDescription: repair.errorDescription || '',
                        resolution: repair.troubleshooting || '',
                        status: repair.status || 'in_progress',
                        priority: 'normal',
                        repairNotes: '',
                        partsUsed: repair.defectiveParts || [],
                        laborHours: 0,
                        totalCost: 0,
                        notes: `Imported from Excel row ${repair.excelRow}. Ticket: ${repair.ticketNumber || 'N/A'}. Warranty: ${repair.warranty || 'N/A'}`,
                        metadata: {
                            ticketNumber: repair.ticketNumber,
                            warranty: repair.warranty,
                            selfRepair: repair.selfRepair,
                            fwBefore: repair.fwBefore,
                            fwAfter: repair.fwAfter,
                            productionDate: repair.productionDate,
                            purchaseDate: repair.purchaseDate,
                            repairTime: repair.repairTime,
                            excelRow: repair.excelRow,
                            importedFromExcel: true,
                        },
                    };
                    if (repair.dateOfReceipt) {
                        const d = new Date(repair.dateOfReceipt);
                        if (!isNaN(d.getTime())) payload.startedAt = d.toISOString();
                    }
                    if (repair.releaseDate) {
                        const d = new Date(repair.releaseDate);
                        if (!isNaN(d.getTime())) payload.completedAt = d.toISOString();
                    }

                    await api.post('/api/rma', payload);
                    created++;
                }
            } catch (e) {
                errors.push(`${rn}: ${e.message}`);
            }
        }

        excelImportResult = { created, updated, errors };
        excelImporting = false;
        if (created + updated > 0) {
            toastStore.add(`Imported ${created} new, updated ${updated} repairs`, 'success');
        }
        if (errors.length > 0) {
            toastStore.add(`${errors.length} error(s) during import`, 'error');
        }
    }

    async function importAllFromExcel() {
        excelImportAllRunning = true;
        excelImportAllProgress = 'Fetching all DB records...';
        let created = 0, updated = 0;
        const errors = [];

        try {
            // Fetch all existing repairs from DB for dedup
            const dbList = await api.get('/api/rma?type=repair');
            const dbMap = new Map();
            for (const r of dbList) {
                if (r.orderNumber) dbMap.set(r.orderNumber, r.id);
            }

            // Fetch all Excel records
            excelImportAllProgress = 'Fetching all Excel records...';
            const excelRes = await api.post('/S/api/excel/read', { limit: 10000, offset: 0 });
            const allRepairs = excelRes.repairs || [];
            const total = allRepairs.length;

            if (total === 0) {
                toastStore.add('No records found in Excel', 'warning');
                excelImportAllRunning = false;
                excelImportAllProgress = '';
                return;
            }

            for (let i = 0; i < total; i++) {
                const repair = allRepairs[i];
                excelImportAllProgress = `Importing ${i + 1} of ${total}...`;

                const payload = {
                    orderNumber: repair.repairNumber,
                    orderType: 'repair',
                    customerName: repair.customerName || '',
                    customerEmail: '',
                    customerPhone: '',
                    productSku: '',
                    productName: repair.model || '',
                    serialNumber: repair.serialNumber || '',
                    issueDescription: repair.errorDescription || '',
                    resolution: repair.troubleshooting || '',
                    status: repair.status || 'in_progress',
                    priority: 'normal',
                    repairNotes: '',
                    partsUsed: repair.defectiveParts || [],
                    laborHours: 0,
                    totalCost: 0,
                    notes: `Imported from Excel row ${repair.excelRow}. Ticket: ${repair.ticketNumber || 'N/A'}. Warranty: ${repair.warranty || 'N/A'}`,
                    metadata: {
                        ticketNumber: repair.ticketNumber,
                        warranty: repair.warranty,
                        selfRepair: repair.selfRepair,
                        fwBefore: repair.fwBefore,
                        fwAfter: repair.fwAfter,
                        productionDate: repair.productionDate,
                        purchaseDate: repair.purchaseDate,
                        repairTime: repair.repairTime,
                        excelRow: repair.excelRow,
                        importedFromExcel: true,
                    },
                };
                try {
                    if (repair.dateOfReceipt) {
                        const d = new Date(repair.dateOfReceipt);
                        if (!isNaN(d.getTime())) payload.startedAt = d.toISOString();
                    }
                    if (repair.releaseDate) {
                        const d = new Date(repair.releaseDate);
                        if (!isNaN(d.getTime())) payload.completedAt = d.toISOString();
                    }

                    const existingId = dbMap.get(repair.repairNumber);
                    if (existingId) {
                        await api.put(`/api/rma/${existingId}`, payload);
                        updated++;
                    } else {
                        await api.post('/api/rma', payload);
                        created++;
                    }
                } catch (e) {
                    errors.push(`${repair.repairNumber}: ${e.message}`);
                }

                // Yield to browser every 15 records
                if ((i + 1) % 15 === 0) await new Promise(r => setTimeout(r, 50));
            }

            excelImportAllProgress = `Done! Created: ${created}, Updated: ${updated}, Errors: ${errors.length}`;
            if (created + updated > 0) {
                toastStore.add(`Import All: ${created} created, ${updated} updated`, 'success');
            }
            if (errors.length > 0) {
                toastStore.add(`${errors.length} error(s) during import`, 'error');
                console.warn('Import All errors:', errors);
            }
        } catch (e) {
            excelImportAllProgress = `Failed: ${e.message}`;
            toastStore.add(`Import All failed: ${e.message}`, 'error');
        }

        excelImportAllRunning = false;
    }

    async function exportSelectedToExcel() {
        if (excelDbSelected.size === 0) return;
        excelExporting = true;
        excelExportResult = null;

        const repairsToExport = [];

        for (const on of excelDbSelected) {
            const repair = excelDbRepairs.find(r => r.orderNumber === on);
            if (!repair) continue;

            repairsToExport.push({
                repairNumber: repair.orderNumber,
                ticketNumber: repair.metadata?.ticketNumber || '',
                warranty: repair.metadata?.warranty === 'J' || repair.metadata?.warranty === 'Y',
                errorDescription: repair.issueDescription || '',
                troubleshooting: repair.resolution || '',
                model: repair.productName || '',
                serialNumber: repair.serialNumber || '',
                customerName: repair.customerName || '',
                defectiveParts: repair.partsUsed || [],
                dateOfReceipt: repair.startedAt ? repair.startedAt.slice(0, 10) : null,
                releaseDate: repair.completedAt ? repair.completedAt.slice(0, 10) : null,
                status: repair.status,
            });
        }

        try {
            const res = await api.post('/S/api/excel/export', { repairs: repairsToExport });
            if (res.success) {
                excelExportResult = { written: res.count, errors: [] };
                toastStore.add(`Exported ${res.count} repair(s) to InBody_Export.xlsx`, 'success');
            } else {
                excelExportResult = { written: 0, errors: [res.error] };
                toastStore.add(`Export failed: ${res.error}`, 'error');
            }
        } catch (e) {
            excelExportResult = { written: 0, errors: [e.message] };
            toastStore.add(`Export failed: ${e.message}`, 'error');
        } finally {
            excelExporting = false;
        }
    }

    async function copyDebugInfo(sync) {
        const debugText = `
# eckWMS Sync Error — ${sync.provider}
You are a technical assistant for eckWMS (warehouse management system). This is an error from a scheduled sync operation.

## System
- eckWMS: Rust (axum) + SvelteKit + PostgreSQL
- Scraper: Node.js + Playwright on port 3211
- Providers: OPAL (courier), DHL (shipping)

## Error
**Provider:** ${sync.provider}
**Time:** ${formatDate(sync.startedAt)}
**Status:** ${sync.status}
**Duration:** ${sync.duration ? (sync.duration / 1000).toFixed(1) + "s" : "N/A"}
**Short:** ${summarizeError(sync.errorDetail)}

## Error Message
${sync.errorDetail || "No error detail"}

## Debug Information
${sync.debugInfo ? JSON.stringify(sync.debugInfo, null, 2) : "No debug info available"}

## Statistics
- Created: ${sync.created || 0}
- Updated: ${sync.updated || 0}
- Skipped: ${sync.skipped || 0}
- Errors: ${sync.errors || 0}

---
Analyze this error and suggest a fix. Be concise.
`.trim();

        try {
            await navigator.clipboard.writeText(debugText);
            toastStore.add("Debug info copied to clipboard!", "success");
        } catch (err) {
            toastStore.add("Failed to copy: " + err.message, "error");
        }
    }

    // ── Database Backup functions ────────────────────────────────────────────
    async function loadBackups() {
        dbBackupsLoading = true;
        try {
            const res = await api.get('/api/admin/db/backups');
            dbBackups = res.backups || [];
        } catch (e) {
            toastStore.add('Failed to load backups: ' + e.message, 'error');
        } finally {
            dbBackupsLoading = false;
        }
    }

    async function createBackup() {
        dbBackupRunning = true;
        try {
            const res = await api.post('/api/admin/db/backup');
            toastStore.add(res.message, 'success');
            await loadBackups();
        } catch (e) {
            toastStore.add('Backup failed: ' + e.message, 'error');
        } finally {
            dbBackupRunning = false;
        }
    }

    async function restoreBackup(filename) {
        const yes = confirm(
            `⚠️ RESTORE DATABASE FROM BACKUP?\n\n` +
            `File: ${filename}\n\n` +
            `This will OVERWRITE all current data with the backup contents.\n` +
            `This action CANNOT be undone.\n\n` +
            `Are you absolutely sure?`
        );
        if (!yes) return;
        dbRestoreRunning = filename;
        try {
            const res = await api.post(`/api/admin/db/restore/${encodeURIComponent(filename)}`);
            toastStore.add(res.message, 'success');
        } catch (e) {
            toastStore.add('Restore failed: ' + e.message, 'error');
        } finally {
            dbRestoreRunning = null;
        }
    }

    function formatBytes(bytes) {
        if (bytes < 1024) return bytes + ' B';
        if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
        return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    }
</script>

<div class="scrapers-page">
    <header>
        <h1>🤖 Scrapers & Integrations</h1>
        <div class="header-actions">
            <button class="refresh-btn" on:click={loadData} disabled={loading}>
                {loading ? "↻ Loading..." : "↻ Refresh"}
            </button>
        </div>
    </header>

    <div class="tabs">
        <button
            class="tab"
            class:active={activeTab === "scraper"}
            on:click={() => { activeTab = "scraper"; if (scraperOnline === null) loadScraperStatus(); }}
        >
            🎛️ Scraper Admin
        </button>
        <button
            class="tab"
            class:active={activeTab === "sync"}
            on:click={() => (activeTab = "sync")}
        >
            🔄 Sync History
        </button>
        <button
            class="tab"
            class:active={activeTab === "database"}
            on:click={() => { activeTab = "database"; if (dbBackups.length === 0) loadBackups(); }}
        >
            🗄️ Database
        </button>
    </div>

    {#if error}
        <div class="error">Failed to load data: {error}</div>
    {:else if activeTab === "scraper"}
        <div class="scraper-section">
            <div class="scraper-status-bar">
                <div class="status-left">
                    <span class="status-dot"
                        class:online={scraperOnline === true}
                        class:offline={scraperOnline === false}
                        class:unknown={scraperOnline === null}
                        class:starting={scraperStarting}
                    ></span>
                    <span class="status-label">
                        {#if scraperStarting}
                            Starting scraper...
                        {:else if scraperOnline === true}
                            Playwright Scraper — running on port {scraperStatus?.port ?? 3211}
                        {:else if scraperOnline === false}
                            Scraper offline
                        {:else}
                            Scraper status unknown
                        {/if}
                    </span>
                </div>
                <div class="status-actions">
                    {#if scraperOnline !== true && !scraperStarting}
                        <button class="run-btn start-scraper-btn" on:click={startScraper}>
                            Start Scraper
                        </button>
                    {/if}
                    <button class="refresh-btn small" on:click={loadScraperStatus} disabled={scraperStarting}>
                        ↻ Check Status
                    </button>
                </div>
            </div>
            {#if scraperStartError}
                <div class="scraper-start-error">
                    <div class="error-row">
                        <span class="error-badge">Failed: {summarizeError(scraperStartError)}</span>
                        <button class="action-btn copy-btn" on:click={copyStartError}>Copy to AI</button>
                    </div>
                    <div class="error-detail">{scraperStartError}</div>
                </div>
            {/if}

            {#if scraperOnline === true && scraperStatus}
                <div class="endpoints-hint">
                    {#each scraperStatus.endpoints as ep}
                        <span class="ep-badge">
                            <span class="ep-method">{ep.method}</span>
                            <span class="ep-path">{ep.path}</span>
                        </span>
                    {/each}
                </div>
            {/if}

            <div class="provider-cards">
                <!-- OPAL card -->
                <div class="provider-card opal-card">
                    <div class="card-header">
                        <span class="card-title">🟢 OPAL Kurier</span>
                        <span class="card-hint">opal-kurier.de</span>
                    </div>
                    <div class="card-controls">
                        <label class="control-row">
                            <span>Limit</span>
                            <select bind:value={opalLimit} disabled={opalRunning}>
                                <option value={5}>5</option>
                                <option value={10}>10</option>
                                <option value={25}>25</option>
                                <option value={50}>50</option>
                            </select>
                        </label>
                        <label class="toggle-row">
                            <input type="checkbox" bind:checked={opalDebug} disabled={opalRunning} />
                            <span class="toggle-label" class:debug-on={opalDebug}>
                                {opalDebug ? '🔍 Debug (headed)' : 'Headless'}
                            </span>
                        </label>
                    </div>
                    {#if opalDebug}
                        <div class="debug-hint">Browser window will open with 600ms slow-motion.</div>
                    {/if}
                    <button class="run-btn opal-run" on:click={testOpalFetch} disabled={opalRunning || scraperOnline !== true}>
                        {#if opalRunning}<span class="spinner">⏳</span> Running{opalDebug ? ' (watch browser)' : '...'}
                        {:else}🚀 Run Fetch{/if}
                    </button>
                    {#if opalResult}
                        <div class="result-box" class:result-ok={opalResult.success} class:result-err={!opalResult.success}>
                            {#if opalResult.success}
                                <div class="result-summary">✅ {opalResult.count} orders fetched in {opalResult.duration}s</div>
                            {:else}
                                <div class="error-row">
                                    <span class="error-badge">❌ {summarizeError(opalResult.error)}</span>
                                    <button class="action-btn copy-btn" on:click={() => copyScraperError('OPAL', opalResult)}>🤖 Copy for AI</button>
                                </div>
                                <div class="error-detail">{opalResult.error}</div>
                            {/if}
                            {#if opalResult.orders?.length}
                                <button class="toggle-json" on:click={() => opalJsonOpen = !opalJsonOpen}>
                                    {opalJsonOpen ? '▼' : '▶'} View JSON ({opalResult.orders.length} orders)
                                </button>
                                {#if opalJsonOpen}<pre class="result-json">{JSON.stringify(opalResult.orders, null, 2)}</pre>{/if}
                            {/if}
                        </div>
                    {/if}
                </div>

                <!-- DHL card -->
                <div class="provider-card dhl-card">
                    <div class="card-header">
                        <span class="card-title">🟡 DHL</span>
                        <span class="card-hint">geschaeftskunden.dhl.de</span>
                    </div>
                    <div class="card-controls">
                        <label class="control-row">
                            <span>Limit</span>
                            <select bind:value={dhlLimit} disabled={dhlRunning}>
                                <option value={5}>5</option>
                                <option value={10}>10</option>
                                <option value={25}>25</option>
                                <option value={50}>50</option>
                            </select>
                        </label>
                        <label class="toggle-row">
                            <input type="checkbox" bind:checked={dhlDebug} disabled={dhlRunning} />
                            <span class="toggle-label" class:debug-on={dhlDebug}>
                                {dhlDebug ? '🔍 Debug (headed)' : 'Headless'}
                            </span>
                        </label>
                    </div>
                    {#if dhlDebug}
                        <div class="debug-hint">Browser window will open with 600ms slow-motion.</div>
                    {/if}
                    <button class="run-btn dhl-run" on:click={testDhlFetch} disabled={dhlRunning || scraperOnline !== true}>
                        {#if dhlRunning}<span class="spinner">⏳</span> Running{dhlDebug ? ' (watch browser)' : '...'}
                        {:else}🚀 Run Fetch{/if}
                    </button>
                    {#if dhlResult}
                        <div class="result-box" class:result-ok={dhlResult.success} class:result-err={!dhlResult.success}>
                            {#if dhlResult.success}
                                <div class="result-summary">✅ {dhlResult.count} shipments fetched in {dhlResult.duration}s</div>
                            {:else}
                                <div class="error-row">
                                    <span class="error-badge">❌ {summarizeError(dhlResult.error)}</span>
                                    <button class="action-btn copy-btn" on:click={() => copyScraperError('DHL', dhlResult)}>🤖 Copy for AI</button>
                                </div>
                                <div class="error-detail">{dhlResult.error}</div>
                            {/if}
                            {#if dhlResult.shipments?.length}
                                <button class="toggle-json" on:click={() => dhlJsonOpen = !dhlJsonOpen}>
                                    {dhlJsonOpen ? '▼' : '▶'} View JSON ({dhlResult.shipments.length} shipments)
                                </button>
                                {#if dhlJsonOpen}<pre class="result-json">{JSON.stringify(dhlResult.shipments, null, 2)}</pre>{/if}
                            {/if}
                        </div>
                    {/if}
                </div>

                <!-- Exact Online card (stub) -->
                <div class="provider-card exact-card">
                    <div class="card-header">
                        <span class="card-title">🔵 Exact Online</span>
                        <span class="card-hint">start.exactonline.de</span>
                    </div>
                    <div class="card-controls">
                        <label class="control-row">
                            <span>Entity</span>
                            <select bind:value={exactEntityType} disabled={exactRunning}>
                                <option value="items">Items</option>
                                <option value="stock-positions">Stock Positions</option>
                                <option value="quotations">Quotations</option>
                                <option value="customers">Customers</option>
                            </select>
                        </label>
                        <label class="control-row">
                            <span>Limit</span>
                            <select bind:value={exactLimit} disabled={exactRunning}>
                                <option value={0}>All</option>
                                <option value={50}>50</option>
                                <option value={100}>100</option>
                                <option value={200}>200</option>
                                <option value={500}>500</option>
                            </select>
                        </label>
                        <label class="control-row">
                            <span>Start Page</span>
                            <input type="number" bind:value={exactStartPage} min="1" disabled={exactRunning} style="width: 60px; padding: 0.3rem; background: #2a2a2a; color: white; border: 1px solid #444; border-radius: 4px;" />
                        </label>
                        <label class="control-row">
                            <span>Delay (ms)</span>
                            <input type="number" bind:value={exactDelayMs} min="500" step="500" disabled={exactRunning} style="width: 70px; padding: 0.3rem; background: #2a2a2a; color: white; border: 1px solid #444; border-radius: 4px;" />
                        </label>
                        <label class="toggle-row">
                            <input type="checkbox" bind:checked={exactDebug} disabled={exactRunning} />
                            <span class="toggle-label" class:debug-on={exactDebug}>
                                {exactDebug ? '🔍 Debug (headed)' : 'Headless'}
                            </span>
                        </label>
                    </div>
                    {#if exactDebug}
                        <div class="debug-hint">Browser window will open with 600ms slow-motion.</div>
                    {/if}
                    <button class="run-btn exact-run" on:click={testExactFetch} disabled={exactRunning || scraperOnline !== true}>
                        {#if exactRunning}<span class="spinner">⏳</span> Fetching...
                        {:else}🚀 Fetch from Exact{/if}
                    </button>
                    {#if exactResult}
                        <div class="result-box" class:result-ok={exactResult.success} class:result-err={!exactResult.success}>
                            {#if exactResult.success}
                                <div class="result-summary">✅ {exactResult.count ?? exactResult.rows?.length ?? 0} records fetched in {exactResult.duration}s</div>
                            {:else}
                                <div class="error-row">
                                    <span class="error-badge">❌ {summarizeError(exactResult.error)}</span>
                                    <button class="action-btn copy-btn" on:click={() => copyScraperError('Exact Online', exactResult)}>🤖 Copy for AI</button>
                                </div>
                                <div class="error-detail">{exactResult.error}</div>
                            {/if}
                            {#if exactResult.rows?.length}
                                <button class="toggle-json" on:click={() => exactJsonOpen = !exactJsonOpen}>
                                    {exactJsonOpen ? '▼' : '▶'} View JSON ({exactResult.rows.length} records)
                                </button>
                                {#if exactJsonOpen}<pre class="result-json">{JSON.stringify(exactResult.rows, null, 2)}</pre>{/if}

                                <button class="run-btn import-run" on:click={importExactData} disabled={exactImportRunning}>
                                    {#if exactImportRunning}<span class="spinner">⏳</span> Saving...
                                    {:else}💾 Save to Database{/if}
                                </button>
                            {/if}
                            {#if exactImportResult}
                                <div class="result-box result-ok" style="margin-top: 0.5rem;">
                                    <div class="result-summary">
                                        ✅ Imported: {exactImportResult.imported} | Errors: {exactImportResult.errors}
                                    </div>
                                </div>
                            {/if}
                        </div>
                    {/if}
                </div>

                <!-- Zoho Desk card -->
                <div class="provider-card zoho-card">
                    <div class="card-header">
                        <span class="card-title">🟣 Zoho Desk</span>
                        <span class="card-hint">desk.inbodysupport.eu</span>
                    </div>
                    <div class="card-controls">
                        <label class="control-row">
                            <span>Limit</span>
                            <select bind:value={zohoLimit} disabled={zohoRunning}>
                                <option value={10}>10</option>
                                <option value={50}>50</option>
                                <option value={100}>100</option>
                                <option value={500}>500</option>
                                <option value={1000}>1000</option>
                                <option value={0}>All</option>
                            </select>
                        </label>
                        <label class="toggle-row">
                            <input type="checkbox" bind:checked={zohoDebug} disabled={zohoRunning} />
                            <span class="toggle-label" class:debug-on={zohoDebug}>
                                {zohoDebug ? '🔍 Debug (headed)' : 'Headless'}
                            </span>
                        </label>
                    </div>
                    {#if zohoDebug}
                        <div class="debug-hint">Browser window will open with 600ms slow-motion.</div>
                    {/if}
                    <button class="run-btn zoho-run" on:click={testZohoFetch} disabled={zohoRunning || scraperOnline !== true}>
                        {#if zohoRunning}<span class="spinner">⏳</span> Running{zohoDebug ? ' (watch browser)' : '...'}
                        {:else}🚀 Fetch Tickets{/if}
                    </button>
                    {#if zohoResult}
                        <div class="result-box" class:result-ok={zohoResult.success} class:result-err={!zohoResult.success}>
                            {#if zohoResult.success}
                                <div class="result-summary">✅ {zohoResult.count ?? zohoResult.tickets?.length ?? 0} tickets in {zohoResult.duration}s</div>
                            {:else}
                                <div class="error-row">
                                    <span class="error-badge">❌ {summarizeError(zohoResult.error)}</span>
                                    <button class="action-btn copy-btn" on:click={() => copyScraperError('Zoho Desk', zohoResult)}>🤖 Copy for AI</button>
                                </div>
                                <div class="error-detail">{zohoResult.error}</div>
                            {/if}
                            {#if zohoResult.tickets?.length}
                                <button class="toggle-json" on:click={() => zohoJsonOpen = !zohoJsonOpen}>
                                    {zohoJsonOpen ? '▼' : '▶'} View JSON ({zohoResult.tickets.length} tickets)
                                </button>
                                {#if zohoJsonOpen}<pre class="result-json">{JSON.stringify(zohoResult.tickets, null, 2)}</pre>{/if}
                                <div style="display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-top: 6px;">
                                    <button class="run-btn import-run" on:click={saveTicketsToDB}
                                        disabled={zohoSaveTicketsRunning}>
                                        {#if zohoSaveTicketsRunning}<span class="spinner">⏳</span> Saving...
                                        {:else}💾 Save Metadata{/if}
                                    </button>
                                    {#if zohoSaveTicketsResult}
                                        <span style="font-size: 0.8em;">
                                            {zohoSaveTicketsResult.success ? '✅' : '⚠️'} {zohoSaveTicketsResult.created} new, {zohoSaveTicketsResult.updated} updated
                                        </span>
                                    {/if}
                                </div>
                                <div style="display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-top: 6px;">
                                    <button class="run-btn import-run" on:click={importAllTickets}
                                        disabled={zohoImportAllRunning || scraperOnline !== true}>
                                        {#if zohoImportAllRunning}<span class="spinner">⏳</span> Importing…
                                        {:else}📥 Import All (threads + attachments){/if}
                                    </button>
                                    <label style="font-size: 0.8em; display: flex; align-items: center; gap: 4px;">
                                        Delay
                                        <input type="number" bind:value={zohoImportDelay} min="500" step="500"
                                            disabled={zohoImportAllRunning}
                                            style="width: 70px; padding: 2px 4px; background: #1e293b; color: #e2e8f0; border: 1px solid #475569; border-radius: 4px;" />
                                        ms
                                    </label>
                                </div>
                                {#if zohoImportAllRunning}
                                    <div style="margin-top: 6px; padding: 8px; background: #1e293b; border-radius: 6px; border: 1px solid #334155; font-size: 0.85em;">
                                        <div style="display: flex; justify-content: space-between; margin-bottom: 4px;">
                                            <span>{zohoImportAllProgress}</span>
                                            <span style="font-weight: bold;">{zohoImportAllStats.current}/{zohoImportAllStats.total}</span>
                                        </div>
                                        <div style="background: #0f172a; border-radius: 4px; height: 8px; overflow: hidden; margin-bottom: 6px;">
                                            <div style="background: #3b82f6; height: 100%; transition: width 0.3s; width: {zohoImportAllStats.total ? (zohoImportAllStats.current / zohoImportAllStats.total * 100) : 0}%;"></div>
                                        </div>
                                        <div style="display: flex; gap: 12px; opacity: 0.8;">
                                            <span style="color: #4ade80;">✓ {zohoImportAllStats.imported}</span>
                                            <span style="color: #94a3b8;">⊘ {zohoImportAllStats.skipped} skipped</span>
                                            {#if zohoImportAllStats.errors > 0}<span style="color: #f87171;">✗ {zohoImportAllStats.errors} errors</span>{/if}
                                        </div>
                                    </div>
                                {/if}
                            {/if}
                        </div>
                        {#if zohoImportAllResult}
                            <div class="result-box" class:result-ok={zohoImportAllResult.success} class:result-err={!zohoImportAllResult.success}>
                                <div class="result-summary">
                                    {zohoImportAllResult.success ? '✅' : '⚠️'}
                                    {zohoImportAllResult.imported} tickets ({zohoImportAllResult.threadCount || 0} threads) from {zohoImportAllResult.total}
                                    {#if zohoImportAllResult.skipped > 0} · {zohoImportAllResult.skipped} skipped{/if}
                                </div>
                                {#if zohoImportAllResult.errors?.length}
                                    <div class="import-errors">
                                        {#each zohoImportAllResult.errors as err}
                                            <div class="import-error-line">⚠️ {err}</div>
                                        {/each}
                                    </div>
                                {/if}
                            </div>
                        {/if}
                    {/if}

                    <div class="sync-section" style="margin: 8px 0; padding: 8px; border: 1px solid #444; border-radius: 6px;">
                        <div style="display: flex; align-items: center; gap: 8px; flex-wrap: wrap;">
                            <button class="run-btn import-run" on:click={syncMissingThreads}
                                disabled={zohoSyncRunning || scraperOnline !== true}>
                                {#if zohoSyncRunning}<span class="spinner">⏳</span> Syncing…
                                {:else}🔄 Sync Missing Threads{/if}
                            </button>
                            <label style="font-size: 0.8em; display: flex; align-items: center; gap: 4px;">
                                Delay
                                <input type="number" bind:value={zohoImportDelay} min="500" step="500"
                                    disabled={zohoSyncRunning}
                                    style="width: 70px; padding: 2px 4px; background: #1e293b; color: #e2e8f0; border: 1px solid #475569; border-radius: 4px;" />
                                ms
                            </label>
                            <span style="font-size: 0.8em; opacity: 0.7;">
                                {#if zohoResult?.tickets?.length}Uses fetched tickets{:else}Uses tickets from DB{/if}
                                — skips already synced
                            </span>
                        </div>
                        {#if zohoSyncRunning && zohoImportAllStats.total > 0}
                            <div style="margin-top: 6px; padding: 8px; background: #1e293b; border-radius: 6px; border: 1px solid #334155; font-size: 0.85em;">
                                <div style="display: flex; justify-content: space-between; margin-bottom: 4px;">
                                    <span>{zohoImportAllProgress}</span>
                                    <span style="font-weight: bold;">{zohoImportAllStats.current}/{zohoImportAllStats.total}</span>
                                </div>
                                <div style="background: #0f172a; border-radius: 4px; height: 8px; overflow: hidden; margin-bottom: 6px;">
                                    <div style="background: #3b82f6; height: 100%; transition: width 0.3s; width: {zohoImportAllStats.total ? (zohoImportAllStats.current / zohoImportAllStats.total * 100) : 0}%;"></div>
                                </div>
                                <div style="display: flex; gap: 12px; opacity: 0.8;">
                                    <span style="color: #4ade80;">✓ {zohoImportAllStats.imported}</span>
                                    <span style="color: #94a3b8;">⊘ {zohoImportAllStats.skipped} synced</span>
                                    {#if zohoImportAllStats.errors > 0}<span style="color: #f87171;">✗ {zohoImportAllStats.errors} errors</span>{/if}
                                </div>
                            </div>
                        {/if}
                        {#if zohoSyncResult}
                            <div class="result-box" style="margin-top: 6px;" class:result-ok={zohoSyncResult.success} class:result-err={!zohoSyncResult.success}>
                                <div class="result-summary">
                                    {zohoSyncResult.success ? '✅' : '⚠️'}
                                    Synced {zohoSyncResult.tickets_synced} tickets ({zohoSyncResult.threads_imported} threads).
                                    {#if zohoSyncResult.tickets_remaining > 0}
                                        <strong>{zohoSyncResult.tickets_remaining} remaining.</strong>
                                    {:else}
                                        All done!
                                    {/if}
                                </div>
                                {#if zohoSyncResult.errors?.length}
                                    <div class="import-errors">
                                        {#each zohoSyncResult.errors as err}
                                            <div class="import-error-line">⚠️ {err}</div>
                                        {/each}
                                    </div>
                                {/if}
                            </div>
                        {/if}
                    </div>

                    <div class="threads-section">
                        <div class="threads-row">
                            <input
                                type="text"
                                bind:value={zohoThreadTicketId}
                                placeholder="Ticket ID for email threads"
                                disabled={zohoThreadRunning}
                                class="ticket-id-input"
                            />
                            <button class="run-btn zoho-run" on:click={testZohoFetchThreads}
                                disabled={zohoThreadRunning || !zohoThreadTicketId || scraperOnline !== true}>
                                {#if zohoThreadRunning}<span class="spinner">⏳</span> Fetching...
                                {:else}📧 Fetch Threads{/if}
                            </button>
                        </div>
                        {#if zohoThreadResult}
                            <div class="result-box" class:result-ok={zohoThreadResult.success} class:result-err={!zohoThreadResult.success}>
                                {#if zohoThreadResult.success}
                                    <div class="result-summary">✅ {zohoThreadResult.count} threads in {zohoThreadResult.duration}s</div>
                                {:else}
                                    <div class="error-row">
                                        <span class="error-badge">❌ {summarizeError(zohoThreadResult.error)}</span>
                                        <button class="action-btn copy-btn" on:click={() => copyScraperError('Zoho Threads', zohoThreadResult)}>🤖 Copy for AI</button>
                                    </div>
                                    <div class="error-detail">{zohoThreadResult.error}</div>
                                {/if}
                                {#if zohoThreadResult.threads?.length}
                                    <button class="toggle-json" on:click={() => zohoThreadJsonOpen = !zohoThreadJsonOpen}>
                                        {zohoThreadJsonOpen ? '▼' : '▶'} View threads ({zohoThreadResult.threads.length})
                                    </button>
                                    {#if zohoThreadJsonOpen}<pre class="result-json">{JSON.stringify(zohoThreadResult.threads, null, 2)}</pre>{/if}
                                    <button class="run-btn import-run" on:click={importThreadsToSystem}
                                        disabled={zohoImportRunning}>
                                        {#if zohoImportRunning}<span class="spinner">⏳</span> Saving...
                                        {:else}💾 Save to System{/if}
                                    </button>
                                {/if}
                            </div>
                        {/if}
                        {#if zohoImportResult}
                            <div class="result-box" class:result-ok={zohoImportResult.success} class:result-err={!zohoImportResult.success}>
                                {#if zohoImportResult.success}
                                    <div class="result-summary">✅ {zohoImportResult.imported} thread(s) saved to documents table</div>
                                {:else}
                                    <div class="result-summary error">❌ Import failed ({zohoImportResult.imported} saved)</div>
                                {/if}
                                {#if zohoImportResult.errors?.length}
                                    <div class="import-errors">
                                        {#each zohoImportResult.errors as err}
                                            <div class="import-error-line">⚠️ {err}</div>
                                        {/each}
                                    </div>
                                {/if}
                                {#if zohoImportResult.documents?.length}
                                    <div class="import-ids">
                                        Document IDs: {zohoImportResult.documents.join(', ')}
                                    </div>
                                {/if}
                            </div>
                        {/if}
                    </div>
                </div>
            </div>

            <!-- ── Excel Reparaturliste ─────────────────────────────────── -->
            <div class="excel-section">
                <div class="excel-header">
                    <span class="excel-title">📋 Excel Reparaturliste</span>
                    <button class="run-btn excel-info-btn" on:click={loadExcelInfo} disabled={excelInfoLoading || scraperOnline !== true}>
                        {excelInfoLoading ? '...' : 'i'} Info
                    </button>
                </div>

                {#if excelInfo}
                    <div class="excel-info-bar" class:info-ok={excelInfo.success} class:info-err={!excelInfo.success}>
                        {#if excelInfo.success}
                            <span>{excelInfo.totalRepairs} repairs | Last: {excelInfo.lastRepairNumber} | Modified: {new Date(excelInfo.lastModified).toLocaleDateString('de-DE')}</span>
                        {:else}
                            <span class="error">File error: {excelInfo.error}</span>
                        {/if}
                    </div>
                {/if}

                {#if excelConfigLoaded}
                    <div class="excel-paths">
                        <div class="excel-path-row">
                            <label class="path-label">Source file</label>
                            <input class="path-input" type="text" bind:value={excelSourcePath} placeholder="Path to .xlsm file" />
                        </div>
                        <div class="excel-path-row">
                            <label class="path-label">Export file</label>
                            <input class="path-input" type="text" bind:value={excelExportPath} placeholder="Auto: source name + _eck.xlsx" />
                        </div>
                        <button class="run-btn excel-save-paths" on:click={saveExcelConfig} disabled={excelConfigSaving}>
                            {excelConfigSaving ? '...' : '💾'} Save paths
                        </button>
                    </div>
                {/if}

                <div class="excel-mode-tabs">
                    <button class="excel-tab" class:active={excelMode === 'import'} on:click={() => excelMode = 'import'}>
                        📥 Import (Excel → DB)
                    </button>
                    <button class="excel-tab" class:active={excelMode === 'export'} on:click={() => excelMode = 'export'}>
                        📤 Export (DB → Excel)
                    </button>
                </div>

                {#if excelMode === 'import'}
                    <div class="excel-panel">
                        <div class="excel-controls-row">
                            <label class="control-row">
                                <span>Show last</span>
                                <select bind:value={excelLimit} disabled={excelLoading}>
                                    <option value={10}>10</option>
                                    <option value={30}>30</option>
                                    <option value={50}>50</option>
                                    <option value={100}>100</option>
                                    <option value={500}>500</option>
                                </select>
                            </label>
                            <button class="run-btn excel-run" on:click={readExcelRepairs} disabled={excelLoading || scraperOnline !== true}>
                                {#if excelLoading}<span class="spinner">⏳</span> Reading...
                                {:else}📖 Read Excel{/if}
                            </button>
                            <button class="run-btn excel-run" on:click={importAllFromExcel}
                                disabled={excelImportAllRunning || excelInfoLoading || excelLoading || scraperOnline !== true}>
                                {#if excelImportAllRunning}<span class="spinner">⏳</span> Importing...
                                {:else}📥 Import All to DB{/if}
                            </button>
                        </div>

                        {#if excelImportAllRunning || excelImportAllProgress}
                            <div class="result-box" class:result-ok={!excelImportAllRunning && !excelImportAllProgress.startsWith('Failed')}
                                class:result-err={excelImportAllProgress.startsWith('Failed')}>
                                <div class="result-summary">{excelImportAllProgress}</div>
                            </div>
                        {/if}

                        {#if excelError}
                            <div class="result-box result-err">
                                <div class="result-summary error">{excelError}</div>
                            </div>
                        {/if}

                        {#if excelRepairs.length > 0}
                            <div class="excel-table-info">
                                Showing {excelRepairs.length} of {excelTotal} repairs (newest first)
                            </div>
                            <div class="excel-table-wrap">
                                <table class="excel-table">
                                    <thead>
                                        <tr>
                                            <th><input type="checkbox" checked={excelSelected.size === excelRepairs.length && excelRepairs.length > 0} on:change={toggleExcelSelectAll} /></th>
                                            <th>Status</th>
                                            <th>Row</th>
                                            <th>Repair #</th>
                                            <th>Ticket</th>
                                            <th>Model</th>
                                            <th>Serial</th>
                                            <th>Customer</th>
                                            <th>Error</th>
                                            <th>Received</th>
                                            <th>Status</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {#each excelRepairs as r}
                                            <tr class:selected={excelSelected.has(r.repairNumber)}>
                                                <td><input type="checkbox" checked={excelSelected.has(r.repairNumber)} disabled={r._importStatus === 'Conflict' || r._importStatus === 'Unchanged'} on:change={() => toggleExcelSelect(r.repairNumber)} /></td>
                                                <td>
                                                    <span class="change-badge" class:new={r._importStatus === 'New'} class:update={r._importStatus === 'Auto-fill'} class:conflict={r._importStatus === 'Conflict'} class:unchanged={r._importStatus === 'Unchanged'} class:resolved={r._importStatus === 'Resolved'}>
                                                        {r._importStatus}
                                                    </span>
                                                    {#if r._importStatus === 'Conflict'}
                                                        <button class="btn-icon review-btn" title="Review Conflict" on:click={() => openConflictModal(r)}>Review</button>
                                                    {/if}
                                                </td>
                                                <td class="muted">{r.excelRow}</td>
                                                <td class="mono">{r.repairNumber}</td>
                                                <td class="muted">{r.ticketNumber || '-'}</td>
                                                <td>{r.model || '-'}</td>
                                                <td class="mono">{r.serialNumber || '-'}</td>
                                                <td class="truncate">{r.customerName || '-'}</td>
                                                <td class="truncate">{r.errorDescription || '-'}</td>
                                                <td>{r.dateOfReceipt || '-'}</td>
                                                <td>
                                                    <span class="status-dot" class:done={r.status === 'completed'} class:wip={r.status !== 'completed'}>
                                                        {r.status === 'completed' ? '✅' : '🔧'}
                                                    </span>
                                                </td>
                                            </tr>
                                        {/each}
                                    </tbody>
                                </table>
                            </div>

                            <div class="excel-actions-row">
                                <button class="run-btn excel-run" on:click={importSelectedToDb}
                                    disabled={excelImporting || excelSelected.size === 0}>
                                    {#if excelImporting}<span class="spinner">⏳</span> Importing...
                                    {:else}📥 Import {excelSelected.size} selected to DB{/if}
                                </button>
                                <button class="toggle-json" on:click={() => excelJsonOpen = !excelJsonOpen}>
                                    {excelJsonOpen ? '▼' : '▶'} Raw JSON
                                </button>
                            </div>

                            {#if excelJsonOpen}
                                <pre class="result-json">{JSON.stringify(excelRepairs.filter(r => excelSelected.has(r.repairNumber)), null, 2)}</pre>
                            {/if}

                            {#if excelImportResult}
                                <div class="result-box" class:result-ok={excelImportResult.errors.length === 0} class:result-err={excelImportResult.errors.length > 0}>
                                    <div class="result-summary">
                                        {excelImportResult.errors.length === 0 ? '✅' : '⚠️'}
                                        Created: {excelImportResult.created}, Updated: {excelImportResult.updated}
                                    </div>
                                    {#if excelImportResult.errors.length > 0}
                                        <div class="import-errors">
                                            {#each excelImportResult.errors as err}
                                                <div class="import-error-line">⚠️ {err}</div>
                                            {/each}
                                        </div>
                                    {/if}
                                </div>
                            {/if}
                        {/if}
                    </div>

                {:else}
                    <div class="excel-panel">
                        <div class="excel-controls-row">
                            <button class="run-btn excel-run" on:click={scanDbChangesForExcel} disabled={excelDbLoading}>
                                {#if excelDbLoading}<span class="spinner">⏳</span> Scanning...
                                {:else}🔍 Scan for Changes (DB vs Excel){/if}
                            </button>
                        </div>

                        {#if excelDbRepairs.length > 0}
                            <div class="excel-table-info">
                                Found {excelDbRepairs.length} change(s) ready to be applied to Excel
                            </div>
                            <div class="excel-table-wrap">
                                <table class="excel-table">
                                    <thead>
                                        <tr>
                                            <th><input type="checkbox" checked={excelDbSelected.size === excelDbRepairs.length && excelDbRepairs.length > 0} on:change={toggleDbSelectAll} /></th>
                                            <th>Repair #</th>
                                            <th>Change Type</th>
                                            <th>Differences</th>
                                            <th>Status (DB)</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {#each excelDbRepairs as r}
                                            <tr class:selected={excelDbSelected.has(r.orderNumber)}>
                                                <td><input type="checkbox" checked={excelDbSelected.has(r.orderNumber)} on:change={() => toggleDbSelect(r.orderNumber)} /></td>
                                                <td class="mono">{r.orderNumber}</td>
                                                <td>
                                                    <span class="change-badge" class:new={r._changeType === 'New'} class:update={r._changeType === 'Update'}>
                                                        {r._changeType}
                                                    </span>
                                                </td>
                                                <td>
                                                    <div class="diff-list">
                                                        {#each r._diffs as diff}
                                                            <div class="diff-item">• {diff}</div>
                                                        {/each}
                                                    </div>
                                                </td>
                                                <td>
                                                    <span class="status-dot" class:done={r.status === 'completed'} class:wip={r.status !== 'completed'}>
                                                        {r.status === 'completed' ? '✅' : '🔧'}
                                                    </span>
                                                </td>
                                            </tr>
                                        {/each}
                                    </tbody>
                                </table>
                            </div>

                            <div class="excel-actions-row">
                                <button class="run-btn excel-run" on:click={exportSelectedToExcel}
                                    disabled={excelExporting || excelDbSelected.size === 0}>
                                    {#if excelExporting}<span class="spinner">⏳</span> Writing...
                                    {:else}📤 Write {excelDbSelected.size} selected to Excel{/if}
                                </button>
                            </div>

                            {#if excelExportResult}
                                <div class="result-box" class:result-ok={excelExportResult.errors.length === 0} class:result-err={excelExportResult.errors.length > 0}>
                                    <div class="result-summary">
                                        {excelExportResult.errors.length === 0 ? '✅' : '⚠️'}
                                        Written: {excelExportResult.written}
                                    </div>
                                    {#if excelExportResult.errors.length > 0}
                                        <div class="import-errors">
                                            {#each excelExportResult.errors as err}
                                                <div class="import-error-line">⚠️ {err}</div>
                                            {/each}
                                        </div>
                                    {/if}
                                </div>
                            {/if}
                        {:else if !excelDbLoading}
                            <div class="excel-empty">No CS- repairs in database yet. Import from Excel first.</div>
                        {/if}
                    </div>
                {/if}
            </div>

            <div class="creds-note">
                Credentials are read from server <code>.env</code>
                (OPAL_USERNAME / DHL_USERNAME). Excel file path: <code>EXCEL_REPAIR_FILE</code>.
            </div>
        </div>

    {:else if activeTab === "database"}
        <div class="database-section">
            <p class="section-desc">
                Automated nightly backups run at 3:00 AM (keeps last 7). You can also create or restore backups manually.
            </p>

            <div class="db-actions">
                <button class="run-btn" on:click={createBackup} disabled={dbBackupRunning}>
                    {#if dbBackupRunning}<span class="spinner">⏳</span> Creating...
                    {:else}📦 Create Backup Now{/if}
                </button>
                <button class="run-btn refresh-btn" on:click={loadBackups} disabled={dbBackupsLoading}>
                    {#if dbBackupsLoading}<span class="spinner">⏳</span>
                    {:else}🔄{/if} Refresh
                </button>
            </div>

            {#if dbBackups.length === 0}
                <div class="empty-state">
                    <p>📭 No backups yet</p>
                    <small>Create your first backup or wait for the nightly job.</small>
                </div>
            {:else}
                <div class="table-container">
                    <table>
                        <thead>
                            <tr>
                                <th>Filename</th>
                                <th>Size</th>
                                <th>Created</th>
                                <th>Actions</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each dbBackups as backup}
                                <tr>
                                    <td class="mono">{backup.filename}</td>
                                    <td>{formatBytes(backup.sizeBytes)}</td>
                                    <td class="sync-time">{formatDate(backup.createdAt)}</td>
                                    <td>
                                        <button
                                            class="action-btn restore-btn"
                                            on:click={() => restoreBackup(backup.filename)}
                                            disabled={dbRestoreRunning !== null}
                                        >
                                            {#if dbRestoreRunning === backup.filename}
                                                <span class="spinner">⏳</span> Restoring...
                                            {:else}
                                                ♻️ Restore
                                            {/if}
                                        </button>
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </div>
            {/if}
        </div>

    {:else if activeTab === "sync"}
        <div class="sync-section">
            <p class="section-desc">
                Synchronization history with external services (OPAL, DHL, Odoo).
                OPAL syncs every hour (on the hour), DHL syncs at :30 past the hour. Active 8 AM - 6 PM.
            </p>

            {#if syncHistory.length === 0}
                <div class="empty-state">
                    <p>📭 No sync history yet</p>
                    <small>Synchronizations will appear automatically</small>
                </div>
            {:else}
                <div class="table-container">
                    <table>
                        <thead>
                            <tr>
                                <th></th>
                                <th>Time</th>
                                <th>Provider</th>
                                <th>Status</th>
                                <th>Created</th>
                                <th>Updated</th>
                                <th>Skipped</th>
                                <th>Duration</th>
                                <th>Actions</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each syncHistory as sync}
                                <tr
                                    class="sync-row"
                                    class:expanded={expandedSyncLogs.has(sync.id)}
                                    class:has-error={sync.status === "error"}
                                    on:click={() => sync.status === "error" ? toggleSyncDetails(sync.id) : null}
                                >
                                    <td class="expand-cell">
                                        {#if sync.status === "error"}
                                            <span class="expand-icon">{expandedSyncLogs.has(sync.id) ? "▼" : "▶"}</span>
                                        {:else}
                                            <span class="muted">-</span>
                                        {/if}
                                    </td>
                                    <td class="sync-time">{formatDate(sync.startedAt)}</td>
                                    <td>
                                        <span class="provider-badge" class:opal={sync.provider === "opal"} class:dhl={sync.provider === "dhl"}>
                                            {sync.provider.toUpperCase()}
                                        </span>
                                    </td>
                                    <td>
                                        <span class="sync-badge" class:success={sync.status === "success"} class:error={sync.status === "error"} class:running={sync.status === "running"}>
                                            {sync.status === "success" ? "✅ Success" : sync.status === "error" ? "❌ Error" : "⏳ Running"}
                                        </span>
                                    </td>
                                    <td class="stat-cell">{sync.created || 0}</td>
                                    <td class="stat-cell">{sync.updated || 0}</td>
                                    <td class="stat-cell muted">{sync.skipped || 0}</td>
                                    <td class="duration-cell">{sync.duration ? (sync.duration / 1000).toFixed(1) + "s" : "-"}</td>
                                    <td on:click|stopPropagation>
                                        {#if sync.status === "error" && (sync.errorDetail || sync.debugInfo)}
                                            <button class="action-btn copy-btn" on:click={() => copyDebugInfo(sync)} title="Copy debug info for AI">
                                                🤖 Copy for AI
                                            </button>
                                        {:else}
                                            <span class="muted">-</span>
                                        {/if}
                                    </td>
                                </tr>
                                {#if expandedSyncLogs.has(sync.id) && sync.status === "error"}
                                    <tr class="debug-row">
                                        <td colspan="9">
                                            <div class="debug-details">
                                                <div class="debug-section">
                                                    <h4>⚠️ Error</h4>
                                                    <pre class="error-message">{sync.errorDetail || "No error detail"}</pre>
                                                </div>
                                                {#if sync.debugInfo}
                                                    <div class="debug-section">
                                                        <h4>🔍 Debug Information</h4>
                                                        <div class="debug-grid">
                                                            {#if sync.debugInfo.error_category}
                                                                <div class="debug-item">
                                                                    <label>Category:</label>
                                                                    <span class="category-badge" class:playwright={sync.debugInfo.error_category === "playwright_scraper"}>{sync.debugInfo.error_category}</span>
                                                                </div>
                                                            {/if}
                                                            {#if sync.debugInfo.likely_cause}
                                                                <div class="debug-item">
                                                                    <label>Likely Cause:</label>
                                                                    <span class="highlight">{sync.debugInfo.likely_cause}</span>
                                                                </div>
                                                            {/if}
                                                            {#if sync.debugInfo.ai_analysis_hint}
                                                                <div class="debug-item">
                                                                    <label>💡 AI Hint:</label>
                                                                    <span class="ai-hint">{sync.debugInfo.ai_analysis_hint}</span>
                                                                </div>
                                                            {/if}
                                                        </div>
                                                        {#if sync.debugInfo.playwright_stderr}
                                                            <div class="stderr-section">
                                                                <h5>📋 Playwright Output (stderr):</h5>
                                                                <pre class="stderr-output">{sync.debugInfo.playwright_stderr}</pre>
                                                            </div>
                                                        {/if}
                                                        <details class="raw-json">
                                                            <summary>🔧 Raw Debug JSON</summary>
                                                            <pre>{JSON.stringify(sync.debugInfo, null, 2)}</pre>
                                                        </details>
                                                    </div>
                                                {/if}
                                            </div>
                                        </td>
                                    </tr>
                                {/if}
                            {/each}
                        </tbody>
                    </table>
                </div>
            {/if}
        </div>
    {/if}
</div>

{#if conflictModalOpen && currentConflict}
    <div class="modal-overlay" on:click={() => conflictModalOpen = false}>
        <div class="modal-card conflict-modal" on:click|stopPropagation>
            <div class="modal-header">
                <h3>Conflict: {currentConflict.repairNumber}</h3>
                <button class="close-btn" on:click={() => conflictModalOpen = false}>&times;</button>
            </div>
            <p class="modal-desc">The database already contains information that differs from the Excel file. Please choose which data to keep.</p>

            <table class="conflict-table">
                <thead>
                    <tr>
                        <th>Field</th>
                        <th>Current DB Value</th>
                        <th>Excel Value (Incoming)</th>
                    </tr>
                </thead>
                <tbody>
                    {#each currentConflict._conflicts as c}
                        <tr>
                            <td><strong>{c.field}</strong></td>
                            <td class="val-db">{c.db || '-'}</td>
                            <td class="val-ex">{c.ex || '-'}</td>
                        </tr>
                    {/each}
                </tbody>
            </table>

            <div class="modal-actions-bar">
                <button class="btn secondary" on:click={() => conflictModalOpen = false}>
                    Keep DB Data (Skip)
                </button>
                <button class="btn primary" on:click={resolveConflict}>
                    Accept Excel Data (Overwrite)
                </button>
            </div>
        </div>
    </div>
{/if}

<style>
    .scrapers-page { padding: 0; }
    header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 1.5rem; }
    h1 { font-size: 1.8rem; color: #fff; margin: 0; }
    .header-actions { display: flex; gap: 1rem; }

    .refresh-btn { padding: 0.6rem 1.2rem; border-radius: 4px; border: 1px solid #4a69bd; background: transparent; color: #4a69bd; font-weight: 600; cursor: pointer; transition: all 0.2s; }
    .refresh-btn:hover:not(:disabled) { background: #4a69bd; color: white; }
    .refresh-btn:disabled { opacity: 0.5; cursor: not-allowed; }

    .tabs { display: flex; gap: 1rem; margin-bottom: 2rem; border-bottom: 2px solid #333; }
    .tab { padding: 0.8rem 1.5rem; border: none; background: transparent; color: #aaa; font-size: 1rem; font-weight: 600; cursor: pointer; border-bottom: 3px solid transparent; transition: all 0.2s; }
    .tab:hover { color: #fff; }
    .tab.active { color: #4a69bd; border-bottom-color: #4a69bd; }

    .section-desc { color: #aaa; margin-bottom: 1.5rem; font-size: 0.95rem; }
    .error { text-align: center; padding: 3rem; color: #ff6b6b; background: #1e1e1e; border-radius: 8px; border: 1px solid #ff6b6b; }
    .empty-state { text-align: center; padding: 3rem; color: #666; background: #1e1e1e; border-radius: 8px; border: 1px dashed #333; }
    .empty-state p { font-size: 1.2rem; margin: 0 0 0.5rem 0; }
    .empty-state small { color: #555; }

    .db-actions { display: flex; gap: 1rem; margin-bottom: 1.5rem; }
    .restore-btn { padding: 0.4rem 0.8rem; font-size: 0.85rem; border-radius: 4px; border: 1px solid #e67e22; background: transparent; color: #e67e22; cursor: pointer; transition: all 0.2s; }
    .restore-btn:hover:not(:disabled) { background: #e67e22; color: #fff; }
    .restore-btn:disabled { opacity: 0.5; cursor: not-allowed; }
    .mono { font-family: monospace; font-size: 0.9rem; }

    .table-container { background: #1e1e1e; border-radius: 8px; border: 1px solid #333; overflow-x: auto; }
    table { width: 100%; border-collapse: collapse; }
    thead { background: #252525; }
    th { padding: 1rem; text-align: left; font-weight: 600; color: #aaa; text-transform: uppercase; font-size: 0.75rem; border-bottom: 2px solid #333; }
    td { padding: 1rem; border-bottom: 1px solid #2a2a2a; color: #e0e0e0; }
    tbody tr:hover { background: #252525; }

    .action-btn { padding: 0.5rem 1rem; border-radius: 4px; border: none; font-weight: 600; font-size: 0.85rem; cursor: pointer; transition: all 0.2s; white-space: nowrap; }
    .muted { color: #666; font-style: italic; }

    /* Sync history */
    .sync-time { font-family: monospace; color: #aaa; font-size: 0.9rem; }
    .sync-badge { display: inline-block; padding: 0.3rem 0.8rem; border-radius: 12px; font-size: 0.75rem; font-weight: 600; color: white; }
    .sync-badge.success { background: #28a745; }
    .sync-badge.error { background: #dc3545; }
    .sync-badge.running { background: #17a2b8; }
    .provider-badge { display: inline-block; padding: 0.3rem 0.8rem; border-radius: 4px; background: #2a2a2a; font-family: monospace; font-size: 0.85rem; text-transform: uppercase; color: #4a69bd; }
    .provider-badge.opal { background: #166534; color: #4ade80; }
    .provider-badge.dhl { background: #713f12; color: #fbbf24; }
    .stat-cell { font-family: monospace; text-align: center; color: #4a69bd; font-weight: 600; }
    .duration-cell { font-family: monospace; color: #888; }

    .sync-row { transition: background 0.2s; }
    .sync-row.has-error { cursor: pointer; }
    .sync-row.has-error:hover { background: #2a2a2a; }
    .sync-row.expanded { background: #252525; border-bottom: none; }
    .expand-cell { width: 30px; text-align: center; }
    .expand-icon { color: #666; font-size: 0.8rem; }

    .copy-btn { background: #1a472a; color: #4ade80; border: 1px solid #22c55e; padding: 0.4rem 0.8rem; font-size: 0.8rem; }
    .copy-btn:hover { background: #166534; }

    .debug-row { background: #1a1a1a; }
    .debug-row td { padding: 0; border-bottom: 2px solid #333; }
    .debug-details { padding: 1.5rem; }
    .debug-section { background: #252525; border-radius: 8px; padding: 1rem; margin-bottom: 1rem; }
    .debug-section h4 { margin: 0 0 1rem 0; color: #fff; font-size: 0.95rem; border-bottom: 1px solid #333; padding-bottom: 0.5rem; }
    .error-message { background: #2a1a1a; color: #ff6b6b; padding: 1rem; border-radius: 4px; border-left: 3px solid #dc3545; overflow-x: auto; font-size: 0.85rem; line-height: 1.4; white-space: pre-wrap; word-wrap: break-word; }
    .debug-grid { display: grid; gap: 0.75rem; }
    .debug-item { display: flex; gap: 0.5rem; font-size: 0.9rem; }
    .debug-item label { color: #888; min-width: 150px; flex-shrink: 0; }
    .debug-item .highlight { color: #ffc107; font-weight: 600; }
    .debug-item .ai-hint { color: #4ade80; font-style: italic; }
    .category-badge { display: inline-block; padding: 0.2rem 0.6rem; border-radius: 4px; background: #2a2a2a; font-size: 0.8rem; text-transform: uppercase; font-weight: 600; }
    .category-badge.playwright { background: #422006; color: #fbbf24; }
    .stderr-section { margin-top: 1rem; }
    .stderr-section h5 { margin: 1rem 0 0.5rem 0; color: #aaa; font-size: 0.85rem; }
    .stderr-output { background: #1a1a1a; color: #aaa; padding: 1rem; border-radius: 4px; border: 1px solid #333; overflow-x: auto; font-size: 0.8rem; line-height: 1.4; max-height: 300px; overflow-y: auto; }
    .raw-json { margin-top: 1rem; }
    .raw-json summary { cursor: pointer; color: #888; font-size: 0.85rem; padding: 0.5rem; background: #2a2a2a; border-radius: 4px; user-select: none; }
    .raw-json summary:hover { color: #aaa; background: #333; }
    .raw-json pre { background: #1a1a1a; color: #4a69bd; padding: 1rem; border-radius: 4px; border: 1px solid #333; overflow-x: auto; font-size: 0.75rem; line-height: 1.4; margin-top: 0.5rem; }

    /* Scraper Admin */
    .scraper-section { display: flex; flex-direction: column; gap: 1.25rem; }
    .scraper-status-bar { display: flex; align-items: center; justify-content: space-between; background: #1e1e1e; border: 1px solid #333; border-radius: 8px; padding: 0.8rem 1.2rem; }
    .status-left { display: flex; align-items: center; gap: 0.75rem; font-size: 0.9rem; color: #ccc; }
    .status-dot { width: 10px; height: 10px; border-radius: 50%; flex-shrink: 0; }
    .status-dot.online  { background: #22c55e; box-shadow: 0 0 6px #22c55e; }
    .status-dot.offline { background: #ef4444; box-shadow: 0 0 6px #ef4444; }
    .status-dot.unknown { background: #6b7280; }
    .status-label code { background: #2a2a2a; border-radius: 3px; padding: 0.1rem 0.4rem; font-size: 0.8rem; color: #4a69bd; }
    .status-actions { display: flex; gap: 0.5rem; align-items: center; }
    .start-scraper-btn { padding: 0.4rem 1rem; font-size: 0.8rem; background: #166534; color: #4ade80; border: 1px solid #22c55e; border-radius: 4px; font-weight: 600; cursor: pointer; }
    .start-scraper-btn:hover { background: #14532d; }
    .status-dot.starting { background: #f59e0b; box-shadow: 0 0 6px #f59e0b; animation: pulse 1.2s ease-in-out infinite; }
    @keyframes pulse { 0%,100% { opacity: 1; } 50% { opacity: 0.4; } }
    .scraper-start-error { background: rgba(239,68,68,0.05); border: 1px solid #ef4444; border-radius: 8px; padding: 0.75rem 1rem; display: flex; flex-direction: column; gap: 0.5rem; }
    .refresh-btn.small { padding: 0.4rem 0.8rem; font-size: 0.8rem; }

    .endpoints-hint { display: flex; flex-wrap: wrap; gap: 0.5rem; }
    .ep-badge { display: inline-flex; align-items: center; gap: 0.3rem; background: #1e1e1e; border: 1px solid #333; border-radius: 4px; padding: 0.25rem 0.6rem; font-size: 0.75rem; }
    .ep-method { color: #4a69bd; font-weight: 700; font-family: monospace; }
    .ep-path { color: #888; font-family: monospace; }

    .provider-cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(340px, 1fr)); gap: 1.25rem; }
    .provider-card { background: #1e1e1e; border: 1px solid #333; border-radius: 10px; padding: 1.25rem; display: flex; flex-direction: column; gap: 1rem; }
    .opal-card  { border-color: #166534; }
    .dhl-card   { border-color: #713f12; }
    .exact-card { border-color: #1e3a5f; }
    .zoho-card  { border-color: #4a1d6e; }

    .card-header { display: flex; align-items: baseline; justify-content: space-between; }
    .card-title { font-size: 1.1rem; font-weight: 700; color: #fff; }
    .card-hint { font-size: 0.75rem; color: #666; font-family: monospace; }
    .card-controls { display: flex; align-items: center; gap: 1.5rem; flex-wrap: wrap; }
    .control-row { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; color: #aaa; }
    .control-row select { background: #2a2a2a; color: #e0e0e0; border: 1px solid #444; border-radius: 4px; padding: 0.3rem 0.5rem; font-size: 0.85rem; cursor: pointer; }
    .toggle-row { display: flex; align-items: center; gap: 0.5rem; cursor: pointer; font-size: 0.85rem; }
    .toggle-label { color: #888; }
    .toggle-label.debug-on { color: #fbbf24; font-weight: 600; }
    .debug-hint { font-size: 0.8rem; color: #fbbf24; background: rgba(251,191,36,0.08); border: 1px solid rgba(251,191,36,0.2); border-radius: 4px; padding: 0.5rem 0.75rem; }

    .run-btn { padding: 0.75rem 1.5rem; border-radius: 6px; border: none; font-weight: 700; font-size: 0.95rem; cursor: pointer; transition: all 0.2s; }
    .opal-run { background: #166534; color: #4ade80; border: 1px solid #22c55e; }
    .opal-run:hover:not(:disabled) { background: #14532d; }
    .dhl-run { background: #713f12; color: #fbbf24; border: 1px solid #f59e0b; }
    .dhl-run:hover:not(:disabled) { background: #92400e; }
    .exact-run { background: #1e3a5f; color: #93c5fd; border: 1px solid #3b82f6; }
    .exact-run:hover:not(:disabled) { background: #1e40af; }
    .zoho-run { background: #4a1d6e; color: #d8b4fe; border: 1px solid #a855f7; }
    .zoho-run:hover:not(:disabled) { background: #6b21a8; }
    .stub-warning { font-size: 0.78rem; color: #f59e0b; }
    .run-btn:disabled { opacity: 0.45; cursor: not-allowed; }
    .spinner { display: inline-block; animation: spin 1s linear infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }

    .result-box { border-radius: 6px; border: 1px solid #333; padding: 0.75rem 1rem; display: flex; flex-direction: column; gap: 0.5rem; }
    .result-box.result-ok  { border-color: #22c55e; background: rgba(34,197,94,0.05); }
    .result-box.result-err { border-color: #ef4444; background: rgba(239,68,68,0.05); }
    .result-summary { font-size: 0.9rem; color: #e0e0e0; }
    .result-summary.error { color: #ff6b6b; }
    .toggle-json { align-self: flex-start; background: none; border: none; color: #4a69bd; font-size: 0.82rem; cursor: pointer; padding: 0; font-family: monospace; }
    .toggle-json:hover { text-decoration: underline; }
    .result-json { background: #141414; color: #4a69bd; border: 1px solid #2a2a2a; border-radius: 4px; padding: 0.75rem; font-size: 0.72rem; line-height: 1.5; overflow: auto; max-height: 400px; white-space: pre; }

    .threads-section { display: flex; flex-direction: column; gap: 0.75rem; border-top: 1px solid #2a2a2a; padding-top: 1rem; }
    .threads-row { display: flex; gap: 0.5rem; }
    .ticket-id-input { flex: 1; background: #141414; border: 1px solid #4a1d6e; color: #e2d9f3; padding: 0.5rem 0.75rem; border-radius: 6px; font-size: 0.85rem; font-family: monospace; }
    .ticket-id-input::placeholder { color: #555; }
    .ticket-id-input:focus { outline: none; border-color: #a855f7; }
    .ticket-id-input:disabled { opacity: 0.5; cursor: not-allowed; }

    .creds-note { font-size: 0.8rem; color: #555; text-align: center; }
    .creds-note code { background: #2a2a2a; border-radius: 3px; padding: 0.1rem 0.35rem; color: #888; }

    .import-run { background: #1a3a5c; color: #93c5fd; border: 1px solid #3b82f6; align-self: flex-start; padding: 0.5rem 1rem; font-size: 0.85rem; }
    .import-run:hover:not(:disabled) { background: #1e40af; }
    .import-errors { display: flex; flex-direction: column; gap: 0.25rem; }
    .import-error-line { font-size: 0.8rem; color: #fbbf24; background: rgba(251,191,36,0.07); border-radius: 4px; padding: 0.3rem 0.6rem; }
    .import-ids { font-size: 0.75rem; color: #555; font-family: monospace; word-break: break-all; }

    .error-row { display: flex; align-items: center; justify-content: space-between; gap: 0.75rem; }
    .error-badge { font-size: 0.9rem; font-weight: 700; color: #ff6b6b; white-space: nowrap; }
    .error-detail { font-size: 0.78rem; color: #aa6b6b; font-family: monospace; background: #1a1010; border-left: 3px solid #ef4444; padding: 0.5rem 0.75rem; border-radius: 4px; white-space: pre-wrap; word-break: break-word; max-height: 120px; overflow-y: auto; }

    /* ── Excel Sync ─────────────────────────────────────────────────── */
    .excel-section { margin-top: 1.5rem; border: 2px solid #c2610a; border-radius: 10px; padding: 1.25rem; background: rgba(194,97,10,0.04); display: flex; flex-direction: column; gap: 0.75rem; }
    .excel-header { display: flex; align-items: center; justify-content: space-between; }
    .excel-title { font-size: 1.1rem; font-weight: 700; color: #fb923c; }
    .excel-info-btn { padding: 0.3rem 0.75rem !important; font-size: 0.8rem !important; background: #4a2a0a; color: #fb923c; border: 1px solid #c2610a; }
    .excel-info-btn:hover:not(:disabled) { background: #6b3a10; }
    .excel-info-bar { font-size: 0.82rem; padding: 0.4rem 0.75rem; border-radius: 5px; }
    .excel-info-bar.info-ok { color: #a3e635; background: rgba(163,230,53,0.06); border: 1px solid rgba(163,230,53,0.2); }
    .excel-info-bar.info-err { color: #ff6b6b; background: rgba(255,107,107,0.06); border: 1px solid rgba(255,107,107,0.2); }
    .excel-mode-tabs { display: flex; gap: 0.5rem; }
    .excel-tab { flex: 1; padding: 0.5rem; border: 1px solid #333; background: #1a1a1a; color: #888; border-radius: 6px; cursor: pointer; font-size: 0.85rem; font-weight: 600; transition: all 0.15s; }
    .excel-tab.active { background: #4a2a0a; color: #fb923c; border-color: #c2610a; }
    .excel-tab:hover:not(.active) { background: #252525; color: #aaa; }
    .excel-panel { display: flex; flex-direction: column; gap: 0.75rem; }
    .excel-controls-row { display: flex; gap: 0.75rem; align-items: center; }
    .excel-run { background: #4a2a0a; color: #fb923c; border: 1px solid #c2610a; }
    .excel-run:hover:not(:disabled) { background: #6b3a10; }
    .excel-table-info { font-size: 0.8rem; color: #888; }
    .excel-table-wrap { overflow-x: auto; max-height: 400px; overflow-y: auto; border: 1px solid #333; border-radius: 6px; }
    .excel-table { width: 100%; border-collapse: collapse; font-size: 0.78rem; }
    .excel-table th { position: sticky; top: 0; background: #1a1a1a; color: #aaa; padding: 0.4rem 0.5rem; text-align: left; border-bottom: 1px solid #333; white-space: nowrap; }
    .excel-table td { padding: 0.35rem 0.5rem; border-bottom: 1px solid #1e1e1e; color: #ccc; }
    .excel-table tr:hover { background: rgba(251,146,60,0.05); }
    .excel-table tr.selected { background: rgba(251,146,60,0.1); }
    .excel-table .mono { font-family: monospace; font-size: 0.75rem; }
    .excel-table .muted { color: #666; }
    .excel-table .truncate { max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .excel-actions-row { display: flex; gap: 0.75rem; align-items: center; }
    .excel-empty { font-size: 0.85rem; color: #666; padding: 1rem; text-align: center; }
    .status-dot.done { color: #4ade80; }
    .status-dot.wip { color: #fb923c; }

    .change-badge { padding: 0.2rem 0.5rem; border-radius: 4px; font-size: 0.7rem; font-weight: 700; text-transform: uppercase; }
    .change-badge.new { background: rgba(74, 222, 128, 0.2); color: #4ade80; border: 1px solid rgba(74, 222, 128, 0.4); }
    .change-badge.update { background: rgba(96, 165, 250, 0.2); color: #60a5fa; border: 1px solid rgba(96, 165, 250, 0.4); }
    .change-badge.conflict { background: rgba(239, 68, 68, 0.2); color: #ef4444; border: 1px solid rgba(239, 68, 68, 0.4); }
    .change-badge.unchanged { background: rgba(107, 114, 128, 0.2); color: #9ca3af; border: 1px solid rgba(107, 114, 128, 0.4); }
    .change-badge.resolved { background: rgba(251, 191, 36, 0.2); color: #fbbf24; border: 1px solid rgba(251, 191, 36, 0.4); }

    .review-btn { background: #4a2a0a; color: #fbbf24; border: 1px solid #f59e0b; font-size: 0.7rem; margin-left: 4px; padding: 2px 6px; border-radius: 4px; cursor: pointer; }
    .review-btn:hover { background: #78350f; }

    .modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.8); z-index: 1000; display: flex; justify-content: center; align-items: center; }
    .conflict-modal { background: #1e1e1e; border: 1px solid #ef4444; border-radius: 8px; width: 90%; max-width: 800px; padding: 1.5rem; display: flex; flex-direction: column; gap: 1rem; }
    .modal-header { display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid #333; padding-bottom: 0.75rem; }
    .modal-header h3 { margin: 0; color: #ff6b6b; font-size: 1.3rem; }
    .modal-desc { color: #ccc; font-size: 0.95rem; margin: 0; }

    .conflict-table { width: 100%; border-collapse: collapse; margin-top: 0.5rem; font-size: 0.9rem; }
    .conflict-table th { text-align: left; padding: 0.75rem; background: #252525; color: #aaa; border-bottom: 2px solid #444; }
    .conflict-table td { padding: 0.75rem; border-bottom: 1px solid #333; vertical-align: top; }
    .val-db { color: #e07070; background: rgba(239, 68, 68, 0.05); }
    .val-ex { color: #4ade80; background: rgba(34, 197, 94, 0.05); font-weight: bold; }

    .modal-actions-bar { display: flex; justify-content: flex-end; gap: 1rem; margin-top: 1rem; padding-top: 1rem; border-top: 1px solid #333; }

    .diff-list { display: flex; flex-direction: column; gap: 0.2rem; }
    .diff-item { font-size: 0.75rem; color: #a1a1aa; }

    .excel-paths { display: flex; flex-direction: column; gap: 0.4rem; padding: 0.6rem; background: #1a1a1a; border: 1px solid #333; border-radius: 6px; margin-bottom: 0.5rem; }
    .excel-path-row { display: flex; align-items: center; gap: 0.5rem; }
    .path-label { font-size: 0.75rem; color: #888; min-width: 70px; flex-shrink: 0; }
    .path-input { flex: 1; background: #111; border: 1px solid #333; border-radius: 4px; padding: 0.3rem 0.5rem; color: #ccc; font-size: 0.75rem; font-family: monospace; }
    .path-input:focus { border-color: #fb923c; outline: none; }
    .path-input::placeholder { color: #555; }
    .excel-save-paths { align-self: flex-end; font-size: 0.75rem; padding: 0.25rem 0.75rem; }
</style>
