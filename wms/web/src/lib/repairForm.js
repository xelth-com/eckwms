// Repair-order form model bridge.
//
// Orders are stored snake_case in the DB (the create handler writes
// `customer_name`, `serial_number`, …; claimlist / receipt intake do the same),
// but the detail form and the create payload speak camelCase. Before this
// module the detail page bound camelCase keys straight onto a snake_case row, so
// orders created from receipts / claimlist loaded into an EMPTY form.
//
// `normalizeOrder()` accepts EITHER key style and yields the camelCase model the
// form binds to. `toSavePayload()` emits the canonical snake_case keys the
// PUT /api/rma/:id merge expects — so a save never spawns duplicate camelCase
// columns and (because PUT merges, see AppState::put_synced_entity) never blanks
// a field the form did not touch.

// Canonical snake_case ↔ form camelCase for the plain string fields.
const FIELDS = [
    ['order_type', 'orderType'],
    ['order_number', 'orderNumber'],
    ['customer_name', 'customerName'],
    ['customer_email', 'customerEmail'],
    ['product_sku', 'productSku'],
    ['product_name', 'productName'],
    ['serial_number', 'serialNumber'],
    ['issue_description', 'issueDescription'],
    ['repair_notes', 'repairNotes'],
];

// Non-editable fields the form still displays (agreement audit, identity,
// stage timestamps). Carried through on load so the template keeps working;
// never re-sent on save (PUT merges, so leaving them out preserves them).
const PASSTHROUGH = [
    'id', 'uuid', 'order_number',
    'agreement_status', 'agreement_token', 'agreement_url', 'agreement_log',
    'status_changed_at', 'updated_at', 'started_at', 'created_at',
];

// First defined, non-null value of either key style.
function pick(raw, snake, camel) {
    const a = raw?.[snake];
    if (a !== undefined && a !== null) return a;
    const b = raw?.[camel];
    if (b !== undefined && b !== null) return b;
    return undefined;
}

/** Loaded order row (snake_case OR camelCase) → camelCase form model. */
export function normalizeOrder(raw = {}) {
    const f = {
        orderType: 'repair',
        orderNumber: '',
        customerName: '',
        customerEmail: '',
        productSku: '',
        productName: '',
        serialNumber: '',
        issueDescription: '',
        status: 'pending',
        priority: 'normal',
        repairNotes: '',
        laborHours: 0,
        partsUsed: [],
        metadata: {},
    };

    for (const [snake, camel] of FIELDS) {
        const v = pick(raw, snake, camel);
        if (v !== undefined) f[camel] = v;
    }

    const status = pick(raw, 'status', 'status');
    if (status !== undefined) f.status = status;
    const priority = pick(raw, 'priority', 'priority');
    if (priority !== undefined) f.priority = priority;

    const lh = pick(raw, 'labor_hours', 'laborHours');
    f.laborHours = lh === undefined ? 0 : (parseFloat(lh) || 0);

    const parts = pick(raw, 'parts_used', 'partsUsed');
    f.partsUsed = Array.isArray(parts) ? parts : [];

    const meta = pick(raw, 'metadata', 'metadata');
    f.metadata = meta && typeof meta === 'object' && !Array.isArray(meta) ? meta : {};

    for (const k of PASSTHROUGH) {
        if (raw?.[k] !== undefined) f[k] = raw[k];
    }
    return f;
}

/**
 * camelCase form model → canonical snake_case payload for PUT /api/rma/:id.
 * Only the fields the form controls are sent; everything else on the stored row
 * (timestamps, agreement audit, customer_phone, diagnosis_notes, total_cost, …)
 * is preserved by the server-side merge. The requester person is never sent.
 */
export function toSavePayload(form = {}) {
    return {
        customer_name: form.customerName ?? '',
        customer_email: form.customerEmail ?? '',
        product_sku: form.productSku ?? '',
        product_name: form.productName ?? '',
        serial_number: form.serialNumber ?? '',
        issue_description: form.issueDescription ?? '',
        repair_notes: form.repairNotes ?? '',
        labor_hours: parseFloat(form.laborHours) || 0,
        parts_used: Array.isArray(form.partsUsed) ? form.partsUsed : [],
        status: form.status || 'pending',
        priority: form.priority || 'normal',
        metadata: form.metadata && typeof form.metadata === 'object' ? form.metadata : {},
    };
}
