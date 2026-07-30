<script>
  // Self-hosted CJK webfonts (woff2 subsets with unicode-range) so Korean/
  // Japanese/Chinese render on nodes whose OS ships no CJK fonts — kiosks
  // have no internet, so fonts must come from the WMS itself, not a CDN.
  // unicode-range means a subset is only downloaded when its glyphs are
  // actually on screen, so unused scripts cost nothing at runtime.
  import '@fontsource-variable/noto-sans-kr';
  import '@fontsource-variable/noto-sans-jp';
  import '@fontsource-variable/noto-sans-sc';
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
