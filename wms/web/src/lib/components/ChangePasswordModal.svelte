<script>
    import { authStore } from "$lib/stores/authStore";
    import { toastStore } from "$lib/stores/toastStore";
    import { t, tr } from "$lib/i18n";
    import { createEventDispatcher } from "svelte";

    // `forced` = the account is flagged mustChangePassword; the modal cannot be
    // dismissed until the change succeeds (no close button, no backdrop-close).
    export let forced = false;

    const dispatch = createEventDispatcher();

    let oldPassword = "";
    let newPassword = "";
    let confirmPassword = "";
    let busy = false;
    let error = "";

    const MIN_LEN = 8;

    async function submit() {
        error = "";
        if (newPassword.length < MIN_LEN) {
            error = tr("shell.pw_too_short", { n: MIN_LEN });
            return;
        }
        if (newPassword !== confirmPassword) {
            error = tr("shell.pw_mismatch");
            return;
        }
        if (newPassword === oldPassword) {
            error = tr("shell.pw_same");
            return;
        }
        busy = true;
        try {
            const res = await fetch("/api/auth/change-password", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    Authorization: `Bearer ${$authStore.token}`,
                },
                body: JSON.stringify({ oldPassword, newPassword }),
            });
            const data = await res.json();
            if (!res.ok || !data.success) {
                error = data.error || tr("shell.pw_change_failed");
                return;
            }
            // Clear the forced flag locally so the app unlocks immediately.
            authStore.clearMustChangePassword();
            toastStore.add(tr("shell.pw_changed"), "success");
            oldPassword = newPassword = confirmPassword = "";
            dispatch("done");
        } catch (e) {
            error = e.message || tr("shell.pw_change_failed");
        } finally {
            busy = false;
        }
    }

    function close() {
        if (forced) return;
        dispatch("close");
    }
</script>

<div class="cp-backdrop" on:click|self={close} role="presentation">
    <div class="cp-modal" role="dialog" aria-modal="true" aria-label={$t("shell.change_password")}>
        <h2>{$t("shell.change_password")}</h2>
        {#if forced}
            <p class="cp-forced">{$t("shell.pw_must_change")}</p>
        {/if}

        <form on:submit|preventDefault={submit}>
            <label>
                <span>{$t("shell.current_password")}</span>
                <input type="password" bind:value={oldPassword} autocomplete="current-password" required />
            </label>
            <label>
                <span>{$t("shell.new_password")}</span>
                <input type="password" bind:value={newPassword} autocomplete="new-password" required />
            </label>
            <label>
                <span>{$t("shell.confirm_password")}</span>
                <input type="password" bind:value={confirmPassword} autocomplete="new-password" required />
            </label>

            {#if error}<p class="cp-error">{error}</p>{/if}

            <div class="cp-actions">
                {#if !forced}
                    <button type="button" class="cp-cancel" on:click={close} disabled={busy}>
                        {$t("shell.cancel")}
                    </button>
                {/if}
                <button type="submit" class="cp-submit" disabled={busy}>
                    {busy ? $t("shell.saving") : $t("shell.save")}
                </button>
            </div>
        </form>
    </div>
</div>

<style>
    .cp-backdrop {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.55);
        display: flex;
        align-items: center;
        justify-content: center;
        z-index: 1000;
    }
    .cp-modal {
        background: #1e1e2a;
        color: #e8e8f0;
        border: 1px solid #33334a;
        border-radius: 10px;
        padding: 1.5rem;
        width: min(92vw, 380px);
        box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    }
    .cp-modal h2 {
        margin: 0 0 0.75rem;
        font-size: 1.15rem;
    }
    .cp-forced {
        margin: 0 0 1rem;
        font-size: 0.85rem;
        color: #ffcf8b;
    }
    .cp-modal label {
        display: block;
        margin-bottom: 0.75rem;
    }
    .cp-modal label span {
        display: block;
        font-size: 0.8rem;
        margin-bottom: 0.25rem;
        color: #a8a8c0;
    }
    .cp-modal input {
        width: 100%;
        padding: 0.55rem 0.65rem;
        border-radius: 6px;
        border: 1px solid #40405a;
        background: #14141d;
        color: #e8e8f0;
        box-sizing: border-box;
    }
    .cp-error {
        color: #ff8b8b;
        font-size: 0.82rem;
        margin: 0.25rem 0 0.75rem;
    }
    .cp-actions {
        display: flex;
        justify-content: flex-end;
        gap: 0.5rem;
        margin-top: 0.5rem;
    }
    .cp-actions button {
        padding: 0.5rem 1rem;
        border-radius: 6px;
        border: none;
        cursor: pointer;
        font-weight: 600;
    }
    .cp-submit {
        background: #4a6cf7;
        color: #fff;
    }
    .cp-submit:disabled {
        opacity: 0.6;
        cursor: default;
    }
    .cp-cancel {
        background: transparent;
        color: #a8a8c0;
        border: 1px solid #40405a !important;
    }
</style>
