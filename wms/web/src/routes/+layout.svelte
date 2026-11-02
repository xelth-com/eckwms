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
  import { initIme } from '$lib/ime/ime';
  import ImeIndicator from '$lib/components/ImeIndicator.svelte';
  import { updated } from '$app/stores';
  import { browser } from '$app/environment';
  import { RESUME_STORAGE_KEY, shouldReloadNow } from '$lib/idleReload';

  // Reload-on-new-deploy: the kiosk Chromium otherwise keeps a stale
  // SvelteKit page for days after an OTA. `$updated` flips true once
  // `kit.version.pollInterval` (svelte.config.js) notices a new build; we
  // then wait until the user is genuinely idle (no pointer/keyboard/touch
  // for IDLE_RELOAD_MS, and not focused on an editable field) before
  // reloading, so nobody mid-click or mid-typing gets yanked out. Right
  // before reloading we stamp a resume marker the login page's observer
  // auto-login reads to skip its 5-minute countdown.
  let lastActivityAt = Date.now();
  function markActivity() {
    lastActivityAt = Date.now();
  }

  onMount(() => {
    authStore.init();
    // In-app IME for physical keyboards (Russian/Korean by key position). Must
    // run on every page — including the login page — so it lives here, above the
    // dashboard layout. Returns a cleanup fn.
    const cleanupIme = initIme();

    let idleCheckTimer = null;
    let unsubUpdated = () => {};

    if (browser) {
      window.addEventListener('pointerdown', markActivity, { passive: true });
      window.addEventListener('keydown', markActivity, { passive: true });
      window.addEventListener('touchstart', markActivity, { passive: true });

      unsubUpdated = updated.subscribe((isUpdated) => {
        if (!isUpdated || idleCheckTimer) return;
        idleCheckTimer = setInterval(() => {
          if (!shouldReloadNow({ lastActivityAt, now: Date.now(), activeElement: document.activeElement })) {
            return;
          }
          clearInterval(idleCheckTimer);
          idleCheckTimer = null;
          try {
            sessionStorage.setItem(RESUME_STORAGE_KEY, String(Date.now()));
          } catch (e) {
            // sessionStorage unavailable — reload still happens, just
            // without the countdown skip on the login page.
          }
          window.location.reload();
        }, 2000);
      });
    }

    return () => {
      cleanupIme?.();
      unsubUpdated();
      if (idleCheckTimer) clearInterval(idleCheckTimer);
      if (browser) {
        window.removeEventListener('pointerdown', markActivity);
        window.removeEventListener('keydown', markActivity);
        window.removeEventListener('touchstart', markActivity);
      }
    };
  });
</script>

<div class="app-container">
  <slot />
</div>

<!-- Input-language pill (renders itself only when the locale has a layout). -->
<ImeIndicator />

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
