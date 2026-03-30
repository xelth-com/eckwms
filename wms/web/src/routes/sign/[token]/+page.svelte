<script>
    import { page } from '$app/stores';
    import { onMount } from 'svelte';
    import { base } from '$app/paths';

    let token = $page.params.token;
    let order = null;
    let loading = true;
    let error = null;
    let signed = false;

    let agreedAgb = false;
    let agreedAvv = false;
    let isSigning = false;

    onMount(async () => {
        try {
            const res = await fetch(`${base}/api/public/agreement/${token}`);
            if (!res.ok) {
                throw new Error("Dieser Link ist ungültig oder abgelaufen.");
            }
            order = await res.json();
        } catch (e) {
            error = e.message;
        } finally {
            loading = false;
        }
    });

    async function signDocument() {
        if (!agreedAgb || !agreedAvv) return;
        isSigning = true;
        try {
            const res = await fetch(`${base}/api/public/agreement/${token}/sign`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    agreed_to_agb: agreedAgb,
                    agreed_to_avv: agreedAvv
                })
            });

            if (!res.ok) {
                const err = await res.json();
                throw new Error(err.error || "Fehler beim Signieren");
            }
            signed = true;
        } catch (e) {
            error = e.message;
        } finally {
            isSigning = false;
        }
    }
</script>

<div class="sign-page">
    <div class="card">
        <div class="logo">
            <span class="e-label">/E/</span> eckWMS <span class="badge">InBody</span>
        </div>

        {#if loading}
            <div class="spinner-container">
                <div class="spinner"></div>
                <p>Lade Dokument...</p>
            </div>
        {:else if error}
            <div class="error-box">
                <h3>Fehler</h3>
                <p>{error}</p>
            </div>
        {:else if signed || order.agreement_status === 'signed'}
            <div class="success-box">
                <div class="check-icon">&#10003;</div>
                <h2>Vielen Dank!</h2>
                <p>Der Vertrag wurde erfolgreich elektronisch signiert und ist rechtsbindend.</p>
                <p class="small-text">Sie können dieses Fenster nun schließen.</p>
            </div>
        {:else}
            <h2>Reparaturauftrag bestätigen</h2>

            <div class="order-info">
                <div class="info-row"><span>Auftrags-Nr:</span> <strong>{order.order_number}</strong></div>
                <div class="info-row"><span>Kunde:</span> <strong>{order.customer_name}</strong></div>
                <div class="info-row"><span>Gerät:</span> <strong>{order.product_name}</strong></div>
                <div class="info-row"><span>Seriennummer:</span> <strong class="mono">{order.serial_number || 'N/A'}</strong></div>
            </div>

            <div class="terms-box">
                <h3>1. Kostenübernahme (AGB)</h3>
                <p>Sollten Sie nach der Diagnose den Reparaturkostenvoranschlag ablehnen oder das Gerät unrepariert zurückfordern, berechnen wir eine Servicepauschale für Diagnose und Transportkosten.</p>

                <h3>2. Datenschutz (AVV)</h3>
                <p>Auf dem Speichermedium des Geräts können sich sensible Gesundheitsdaten nach Art. 9 DSGVO befinden. Mit der Übergabe des Geräts schließen Sie mit uns einen Vertrag zur Auftragsverarbeitung (AVV) ab. Wir verarbeiten die Daten ausschließlich zum Zweck der Reparatur und sichern diese technisch ab.</p>
            </div>

            <div class="checkboxes">
                <label class="cb-container">
                    <input type="checkbox" bind:checked={agreedAgb}>
                    <span class="cb-text">Ich habe die AGB gelesen und verpflichte mich zur Kostenübernahme bei Reparaturabbruch.</span>
                </label>
                <label class="cb-container">
                    <input type="checkbox" bind:checked={agreedAvv}>
                    <span class="cb-text">Ich stimme dem Auftragsverarbeitungsvertrag (AVV) zur Verarbeitung von Gesundheitsdaten zu.</span>
                </label>
            </div>

            <button
                class="sign-btn"
                disabled={!agreedAgb || !agreedAvv || isSigning}
                on:click={signDocument}
            >
                {isSigning ? 'Wird signiert...' : 'Zahlungspflichtig beauftragen'}
            </button>

            <p class="legal-hint">
                Zur rechtlichen Absicherung werden Ihre IP-Adresse und ein genauer Zeitstempel erfasst und als manipulationssicherer Hash auf der Hedera-Blockchain (DLT) gespeichert.
            </p>
        {/if}
    </div>
</div>

<style>
    .sign-page {
        display: flex; justify-content: center; align-items: flex-start;
        padding: 3rem 1rem; background: #121212; min-height: 100vh; color: #fff;
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    }
    .card {
        background: #1e1e1e; padding: 2.5rem; border-radius: 12px;
        max-width: 600px; width: 100%; border: 1px solid #333;
        box-shadow: 0 10px 30px rgba(0,0,0,0.5);
    }
    .logo { font-size: 1.2rem; font-weight: 800; color: #fff; margin-bottom: 2rem; border-bottom: 1px solid #333; padding-bottom: 1rem; display: flex; align-items: center; }
    .e-label { color: #e03c31; font-family: monospace; margin-right: 8px; }
    .badge { background: #3b82f6; color: #fff; font-size: 0.7rem; padding: 2px 6px; border-radius: 4px; margin-left: 8px; }

    h2 { font-size: 1.4rem; color: #fff; margin: 0 0 1.5rem 0; }

    .order-info { background: #252525; padding: 1rem; border-radius: 6px; margin-bottom: 1.5rem; border-left: 3px solid #4a69bd; }
    .info-row { display: flex; justify-content: space-between; padding: 0.3rem 0; font-size: 0.95rem; }
    .info-row span { color: #888; }
    .mono { font-family: monospace; color: #a3bffa; }

    .terms-box { background: #1a1a1a; padding: 1.5rem; border-radius: 6px; margin-bottom: 1.5rem; border: 1px solid #333; }
    .terms-box h3 { font-size: 1rem; color: #a3bffa; margin: 0 0 0.5rem 0; }
    .terms-box p { font-size: 0.85rem; color: #aaa; line-height: 1.5; margin: 0 0 1rem 0; }
    .terms-box p:last-child { margin-bottom: 0; }

    .checkboxes { display: flex; flex-direction: column; gap: 1rem; margin-bottom: 2rem; }
    .cb-container { display: flex; align-items: flex-start; gap: 0.75rem; cursor: pointer; background: #252525; padding: 1rem; border-radius: 6px; border: 1px solid #333; transition: border-color 0.2s; }
    .cb-container:hover { border-color: #4a69bd; }
    .cb-container input[type="checkbox"] { margin-top: 3px; transform: scale(1.2); accent-color: #4a69bd; }
    .cb-text { font-size: 0.9rem; color: #e0e0e0; line-height: 1.4; }

    .sign-btn {
        width: 100%; background: #28a745; color: white; padding: 1.2rem;
        border: none; border-radius: 6px; font-weight: 600; font-size: 1.1rem;
        cursor: pointer; transition: background 0.2s;
    }
    .sign-btn:hover:not(:disabled) { background: #218838; }
    .sign-btn:disabled { background: #333; color: #666; cursor: not-allowed; }

    .legal-hint { font-size: 0.8rem; color: #666; text-align: center; margin-top: 1.5rem; line-height: 1.4; }

    .success-box { text-align: center; padding: 2rem 0; }
    .check-icon { font-size: 4rem; margin-bottom: 1rem; color: #4ade80; }
    .success-box h2 { color: #4ade80; }
    .success-box p { color: #ccc; line-height: 1.5; }
    .small-text { font-size: 0.85rem; color: #666; margin-top: 1rem; }

    .error-box { background: rgba(239, 68, 68, 0.1); border: 1px solid #ef4444; padding: 1.5rem; border-radius: 6px; text-align: center; }
    .error-box h3 { color: #ff6b6b; margin: 0 0 0.5rem 0; }
    .error-box p { color: #ccc; margin: 0; }

    .spinner-container { text-align: center; padding: 2rem; color: #888; }
    .spinner { display: inline-block; width: 40px; height: 40px; border: 3px solid #333; border-radius: 50%; border-top-color: #4a69bd; animation: spin 1s ease-in-out infinite; margin-bottom: 1rem; }
    @keyframes spin { to { transform: rotate(360deg); } }
</style>
