<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore.js';

    let tickets = [];
    let loading = true;
    let error = null;

    onMount(async () => {
        await loadTickets();
    });

    async function loadTickets() {
        loading = true;
        error = null;
        try {
            tickets = await api.get('/api/support/tickets');
        } catch (e) {
            console.error(e);
            error = e.message;
            toastStore.add('Failed to load support tickets', 'error');
        } finally {
            loading = false;
        }
    }

    function openTicket(ticketId) {
        goto(`${base}/dashboard/support/${ticketId}`);
    }

    function formatDate(str) {
        if (!str) return '-';
        try {
            return new Date(str).toLocaleString('de-DE', {
                day: '2-digit', month: '2-digit', year: 'numeric',
                hour: '2-digit', minute: '2-digit'
            });
        } catch { return str; }
    }

    function getWarrantyStatus(dateStr) {
        if (!dateStr) return null;
        const mfgDate = new Date(dateStr);
        if (isNaN(mfgDate)) return null;

        const ageYears = (Date.now() - mfgDate.getTime()) / (1000 * 60 * 60 * 24 * 365.25);

        if (ageYears < 2.0) return { text: "Warranty", class: "w-ok" };
        if (ageYears < 2.3) return { text: "Likely Warranty (Check Purchase)", class: "w-check" };
        if (ageYears < 2.5) return { text: "Possible Goodwill (Kulanz)", class: "w-goodwill" };
        return { text: "Out of Warranty", class: "w-expired" };
    }

    function createRepairFromTicket(event, ticket) {
        event.stopPropagation();
        const params = new URLSearchParams({
            ticketId: ticket.ticket_id,
            name: ticket.customer || '',
            email: ticket.email || '',
            issue: ticket.subject || '',
            serial: ticket.serial_number || '',
            model: ticket.device_model || ''
        });
        goto(`${base}/dashboard/repairs/new?${params}`);
    }

    function statusClass(status) {
        const s = (status || '').toLowerCase();
        if (s === 'open') return 'open';
        if (s === 'closed') return 'closed';
        if (s.includes('pending agent')) return 'urgent';
        if (s.includes('research')) return 'research';
        if (s === 'onhold' || s === 'on hold') return 'onhold';
        return 'other';
    }
</script>

<div class="support-page">
    <header>
        <h1>Support Tickets</h1>
        <button class="refresh-btn" on:click={loadTickets} disabled={loading}>
            {loading ? 'Loading...' : 'Refresh'}
        </button>
    </header>

    {#if loading}
        <div class="loading">Loading tickets...</div>
    {:else if error}
        <div class="error-box">Failed to load: {error}</div>
    {:else if tickets.length === 0}
        <div class="empty-state">
            <p>No support tickets imported yet</p>
            <small>Use the Scrapers page - Zoho Desk - Fetch Threads - Save to System</small>
        </div>
    {:else}
        <div class="table-container">
            <table>
                <thead>
                    <tr>
                        <th>Ticket #</th>
                        <th>Request Details</th>
                        <th>Customer</th>
                        <th>Status</th>
                        <th class="center">Threads</th>
                        <th>Updated</th>
                        <th>Action</th>
                    </tr>
                </thead>
                <tbody>
                    {#each tickets as ticket}
                        <tr class="ticket-row" on:click={() => openTicket(ticket.ticket_id)}>
                            <td class="mono highlight">#{ticket.ticket_number || ticket.ticket_id.substring(0,8)}</td>

                            <td class="req-cell">
                                <div class="subject">{ticket.subject}</div>
                                <div class="device-row">
                                    {#if ticket.device_model || ticket.serial_number}
                                        <div class="device-badge">
                                            {#if ticket.device_model}{ticket.device_model}{/if}
                                            {#if ticket.device_model && ticket.serial_number} | {/if}
                                            {#if ticket.serial_number}SN: <span class="mono">{ticket.serial_number}</span>{/if}
                                        </div>
                                    {/if}
                                    {#if ticket.manufacturing_date}
                                        {@const wStatus = getWarrantyStatus(ticket.manufacturing_date)}
                                        {#if wStatus}
                                            <div class="warranty-badge {wStatus.class}">{wStatus.text}</div>
                                        {/if}
                                    {/if}
                                </div>
                            </td>

                            <td class="customer-cell">
                                <div class="c-row">
                                    <span class="c-name">{ticket.customer || 'Unknown'}</span>
                                    {#if ticket.company}<span class="c-company">{ticket.company}</span>{/if}
                                </div>
                                <div class="c-row c-contact">
                                    {#if ticket.email}<span class="c-email">{ticket.email}</span>{/if}
                                    {#if ticket.phone}<span class="c-phone">{ticket.phone}</span>{/if}
                                </div>
                            </td>

                            <td>
                                <span class="status-badge {statusClass(ticket.status)}">
                                    {ticket.status}
                                </span>
                            </td>

                            <td class="center mono">{ticket.thread_count}</td>
                            <td class="mono date">{formatDate(ticket.latest_update)}</td>

                            <td on:click|stopPropagation>
                                <button class="action-btn repair-btn" on:click={(e) => createRepairFromTicket(e, ticket)}>
                                    Repair
                                </button>
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        </div>
    {/if}
</div>

<style>
    .support-page { padding: 0; }

    header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 1.5rem;
    }
    h1 { font-size: 1.8rem; color: #fff; margin: 0; }

    .refresh-btn {
        padding: 0.6rem 1.2rem;
        border-radius: 4px;
        border: 1px solid #4a69bd;
        background: transparent;
        color: #4a69bd;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s;
    }
    .refresh-btn:hover:not(:disabled) { background: #4a69bd; color: white; }
    .refresh-btn:disabled { opacity: 0.5; cursor: not-allowed; }

    .loading { color: #aaa; text-align: center; padding: 3rem; }

    .error-box {
        text-align: center;
        padding: 2rem;
        color: #ff6b6b;
        background: #1e1e1e;
        border: 1px solid #ff6b6b;
        border-radius: 8px;
    }

    .empty-state {
        text-align: center;
        padding: 4rem 2rem;
        color: #666;
        background: #1e1e1e;
        border-radius: 8px;
        border: 1px dashed #333;
    }
    .empty-state p { font-size: 1.2rem; margin: 0 0 0.5rem 0; color: #aaa; }
    .empty-state small { color: #555; }

    .table-container {
        background: #1e1e1e;
        border-radius: 8px;
        border: 1px solid #333;
        overflow-x: auto;
    }
    table { width: 100%; border-collapse: collapse; }
    thead { background: #252525; }
    th {
        padding: 0.9rem 1rem;
        text-align: left;
        font-weight: 600;
        color: #aaa;
        text-transform: uppercase;
        font-size: 0.73rem;
        letter-spacing: 0.5px;
        border-bottom: 2px solid #333;
    }
    td {
        padding: 0.9rem 1rem;
        border-bottom: 1px solid #2a2a2a;
        color: #e0e0e0;
        font-size: 0.9rem;
    }

    .ticket-row { cursor: pointer; transition: background 0.15s; }
    .ticket-row:hover { background: #252525; }
    .ticket-row:last-child td { border-bottom: none; }

    .mono { font-family: monospace; }
    .highlight { color: #6bc5f0; font-weight: bold; }
    .date { color: #888; font-size: 0.85rem; }
    .center { text-align: center; }

    .req-cell { max-width: 400px; }
    .subject { font-weight: 500; color: #fff; line-height: 1.4; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
    .device-row { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: center; margin-top: 0.4rem; }
    .device-badge { font-size: 0.75rem; color: #a3bffa; background: #1a2a4a; padding: 0.15rem 0.5rem; border-radius: 4px; border: 1px solid #4a69bd; white-space: nowrap; }

    .warranty-badge { font-size: 0.7rem; padding: 0.15rem 0.4rem; border-radius: 4px; font-weight: 600; white-space: nowrap; }
    .warranty-badge.w-ok { background: rgba(34, 197, 94, 0.15); color: #4ade80; border: 1px solid rgba(34, 197, 94, 0.3); }
    .warranty-badge.w-check { background: rgba(251, 191, 36, 0.15); color: #fbbf24; border: 1px solid rgba(251, 191, 36, 0.3); }
    .warranty-badge.w-goodwill { background: rgba(249, 115, 22, 0.15); color: #fb923c; border: 1px solid rgba(249, 115, 22, 0.3); }
    .warranty-badge.w-expired { background: rgba(156, 163, 175, 0.1); color: #9ca3af; border: 1px solid rgba(156, 163, 175, 0.3); }

    .customer-cell { max-width: 350px; }
    .c-row { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: center; margin-bottom: 0.2rem; }
    .c-name { font-weight: 600; color: #e0e0e0; font-size: 0.95rem; }
    .c-company { font-size: 0.75rem; color: #fbbf24; background: rgba(251, 191, 36, 0.1); padding: 0.1rem 0.4rem; border-radius: 4px; border: 1px solid rgba(251, 191, 36, 0.3); }
    .c-contact { font-size: 0.8rem; color: #888; margin-top: 0.3rem; }
    .c-email, .c-phone { white-space: nowrap; }

    .action-btn { padding: 0.4rem 0.8rem; border-radius: 4px; font-size: 0.8rem; cursor: pointer; font-weight: 600; transition: all 0.2s; white-space: nowrap; }
    .repair-btn { background: transparent; border: 1px solid #3b82f6; color: #93c5fd; }
    .repair-btn:hover { background: #1e3a5f; color: #fff; }

    .status-badge {
        display: inline-block;
        padding: 0.2rem 0.7rem;
        border-radius: 12px;
        font-size: 0.75rem;
        font-weight: 600;
        text-transform: capitalize;
    }
    .status-badge.open    { background: #1a3a1a; color: #4ade80; border: 1px solid #22c55e; }
    .status-badge.closed  { background: #2a2a2a; color: #888;    border: 1px solid #444; }
    .status-badge.onhold  { background: #3a2a0a; color: #fbbf24; border: 1px solid #f59e0b; }
    .status-badge.urgent  { background: #4a1010; color: #ff6b6b; border: 1px solid #dc3545; }
    .status-badge.research { background: #1a2a4a; color: #a3bffa; border: 1px solid #4a69bd; }
    .status-badge.other   { background: #1a2a3a; color: #93c5fd; border: 1px solid #3b82f6; }
</style>
