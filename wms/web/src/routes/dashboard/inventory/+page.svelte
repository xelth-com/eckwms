<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { toastStore } from '$lib/stores/toastStore';
    import { wsStore } from '$lib/stores/wsStore';
    import { t, tr } from '$lib/i18n';

    // Flat inventory table: one row per counted (shelf, part) — time · place ·
    // part · IST (counted) vs SOLL (Exact expected). Newest first.
    let lines = [];
    let loading = true;
    let error = null;
    let discrepanciesOnly = false;

    onMount(load);

    async function load() {
        loading = true;
        error = null;
        try {
            const res = await api.get('/api/warehouse/inventory');
            lines = res.lines || [];
        } catch (e) {
            error = e.message;
            toastStore.add(tr('inventory.load_failed', { error: e.message }), 'error');
        } finally {
            loading = false;
        }
    }

    function fmtTime(t) {
        if (!t) return '—';
        const d = new Date(t);
        if (isNaN(d)) return t;
        return d.toLocaleString();
    }
    function num(n) {
        const v = Number(n) || 0;
        return v % 1 === 0 ? String(v) : v.toFixed(1);
    }

    // Colour by discrepancy: green = match; toward BLUE when IST > SOLL (over),
    // toward RED when IST < SOLL (short). The bigger the mismatch, the more
    // saturated (bg alpha + colour blend scale with |diff| relative to soll).
    const GREEN = [74, 222, 128], BLUE = [96, 165, 250], RED = [248, 113, 113];
    function colors(l) {
        const ist = Number(l.ist) || 0, soll = Number(l.soll) || 0, diff = ist - soll;
        const denom = Math.max(Math.abs(diff), soll, 1);
        const t = Math.min(1, Math.abs(diff) / denom); // 0 = match … 1 = max mismatch
        let c = GREEN;
        if (diff > 0) c = GREEN.map((g, i) => Math.round(g + (BLUE[i] - g) * t));
        else if (diff < 0) c = GREEN.map((g, i) => Math.round(g + (RED[i] - g) * t));
        const bgA = diff === 0 ? 0.07 : 0.12 + 0.4 * t;
        return { bg: `rgba(${c[0]},${c[1]},${c[2]},${bgA})`, fg: `rgb(${c[0]},${c[1]},${c[2]})` };
    }

    $: shown = discrepanciesOnly ? lines.filter((l) => Number(l.diff) !== 0) : lines;
    $: totalIst = lines.reduce((s, l) => s + (Number(l.ist) || 0), 0);

    // Live refresh: a put-away/take on this node broadcasts INVENTORY_UPDATED
    // over the shared /E/ws socket — reload so every open tab sees it without
    // a manual refresh. Debounced so a burst of scans doesn't hammer the API.
    // Note: only reaches browsers connected to the SAME node the scan hit —
    // a cross-node update still needs mesh sync to land first.
    let reloadTimer;
    function scheduleReload() {
        clearTimeout(reloadTimer);
        reloadTimer = setTimeout(load, 400);
    }
    $: if ($wsStore.lastMessage?.type === 'INVENTORY_UPDATED') scheduleReload();
</script>

<div class="inv">
    <header>
        <h1>{$t('inventory.title')}</h1>
        <div class="actions">
            <label class="toggle">
                <input type="checkbox" bind:checked={discrepanciesOnly} />
                {$t('inventory.discrepancies_only')}
            </label>
            <button class="btn" on:click={load} disabled={loading}>↻ {$t('inventory.refresh')}</button>
        </div>
    </header>

    {#if loading}
        <div class="msg">{$t('inventory.loading')}</div>
    {:else if error}
        <div class="msg err">{error}</div>
    {:else if lines.length === 0}
        <div class="msg">{$t('inventory.empty')}</div>
    {:else}
        <div class="summary">{$t('inventory.summary', { rows: shown.length, pcs: num(totalIst) })}</div>
        <table>
            <thead>
                <tr>
                    <th>{$t('inventory.th_time')}</th>
                    <th>{$t('inventory.th_location')}</th>
                    <th>{$t('inventory.th_warehouse')}</th>
                    <th>{$t('inventory.th_code')}</th>
                    <th>{$t('inventory.th_name')}</th>
                    <th class="num">{$t('inventory.th_ist')}</th>
                    <th class="num">{$t('inventory.th_soll')}</th>
                    <th class="num">Δ</th>
                </tr>
            </thead>
            <tbody>
                {#each shown as l}
                    {@const col = colors(l)}
                    <tr style:background={col.bg}>
                        <td class="dim">{fmtTime(l.time)}</td>
                        <td class="mono">{l.location ?? '—'}</td>
                        <td class="dim">{l.warehouse ?? ''}</td>
                        <td class="mono">{l.default_code ?? '—'}</td>
                        <td class="name">{l.name ?? ''}</td>
                        <td class="num ist">{num(l.ist)}</td>
                        <td class="num soll">{num(l.soll)}</td>
                        <td class="num diff" style:color={col.fg}>
                            {Number(l.diff) > 0 ? '+' : ''}{num(l.diff)}
                        </td>
                    </tr>
                {/each}
            </tbody>
        </table>
    {/if}
</div>

<style>
    .inv { max-width: 1100px; margin: 0 auto; color: #ddd; }
    header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 1.25rem; flex-wrap: wrap; gap: 1rem; }
    h1 { margin: 0; color: #fff; font-size: 1.6rem; }
    .actions { display: flex; align-items: center; gap: 1rem; }
    .toggle { display: flex; align-items: center; gap: 0.4rem; font-size: 0.85rem; color: #aaa; cursor: pointer; }
    .btn { background: #4a69bd; color: #fff; border: none; border-radius: 6px; padding: 0.5rem 1rem; cursor: pointer; font-weight: 600; }
    .btn:hover { background: #3d5aa8; }
    .btn:disabled { opacity: 0.6; cursor: not-allowed; }
    .msg { color: #888; padding: 2rem 0; text-align: center; }
    .msg.err { color: #ff6b6b; }
    .summary { color: #888; font-size: 0.85rem; margin-bottom: 0.5rem; }

    table { width: 100%; border-collapse: collapse; }
    th, td { text-align: left; padding: 0.5rem 0.6rem; border-bottom: 1px solid #2a2a2a; font-size: 0.9rem; }
    th { color: #777; font-size: 0.7rem; text-transform: uppercase; letter-spacing: 0.5px; }
    td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; }
    .mono { font-family: monospace; color: #cbd5e1; }
    .dim { color: #888; font-size: 0.8rem; }
    .name { color: #bbb; max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .ist { color: #fff; font-weight: 600; }
    .soll { color: #9aa; }
    .diff.pos { color: #4ade80; }
    .diff.neg { color: #ff8080; }
</style>
