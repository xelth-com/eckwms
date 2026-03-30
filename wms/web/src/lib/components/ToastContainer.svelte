<script>
    import { toastStore } from '$lib/stores/toastStore';
    import { fly } from 'svelte/transition';
</script>

<div class="toast-container">
    {#each $toastStore as toast (toast.id)}
        <!-- svelte-ignore a11y-click-events-have-key-events -->
        <!-- svelte-ignore a11y-no-noninteractive-element-interactions -->
        <div
            class="toast {toast.type}"
            in:fly="{{ y: 20, duration: 300 }}"
            out:fly="{{ x: 20, duration: 300 }}"
            on:click={() => toastStore.remove(toast.id)}
            role="alert"
        >
            {toast.message}
        </div>
    {/each}
</div>

<style>
    .toast-container {
        position: fixed;
        bottom: 20px;
        right: 20px;
        display: flex;
        flex-direction: column;
        gap: 10px;
        z-index: 9999;
        pointer-events: none; /* Allow clicking through container */
    }

    .toast {
        padding: 12px 20px;
        border-radius: 4px;
        color: white;
        font-weight: 500;
        box-shadow: 0 4px 6px rgba(0,0,0,0.3);
        min-width: 250px;
        cursor: pointer;
        pointer-events: auto; /* Enable clicks on toasts */
        opacity: 0.95;
    }

    .info { background-color: #4a69bd; }
    .success { background-color: #28a745; }
    .error { background-color: #d32f2f; }
    .warning { background-color: #f39c12; color: #333; }
</style>
