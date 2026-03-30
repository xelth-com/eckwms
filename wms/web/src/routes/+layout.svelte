<script>
  import '../app.css';
  import { authStore } from '$lib/stores/authStore';
  import { onMount } from 'svelte';

  onMount(() => {
    authStore.init();
  });
</script>

<div class="app-container">
  <slot />
</div>

<style>
  .app-container {
    min-height: 100vh;
    width: 100vw;
    display: flex;
    flex-direction: column;
    background-color: var(--bg-color);
    color: var(--text-color);
  }
</style>
