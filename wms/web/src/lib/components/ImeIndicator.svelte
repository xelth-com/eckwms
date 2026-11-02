<script>
  // Small fixed pill showing the active input language for the in-app IME.
  // Shown only when the current UI locale has an alternate layout (ru/ko).
  // Clicking it toggles the layout; it uses pointerdown + preventDefault so the
  // focused text field keeps focus (and stays out of the tab order — keyboard
  // users toggle with the hotkeys named in the tooltip).
  import { imeLang, imeOn, imeAvailable, toggleIme } from '$lib/ime/ime';
  import { t, localeName } from '$lib/i18n';

  function onToggle(e) {
    e.preventDefault();
    toggleIme();
  }

  // Off → English; on → the native short label of the active layout.
  $: label = !$imeOn ? 'EN' : $imeLang === 'ru' ? 'РУ' : $imeLang === 'ko' ? '한' : 'EN';
  $: langName = $imeLang ? localeName($imeLang) : '';
  $: hint = $t('shell.ime_hint', { lang: langName });
</script>

{#if $imeAvailable}
  <button
    type="button"
    class="ime-pill"
    class:on={$imeOn}
    tabindex="-1"
    on:pointerdown={onToggle}
    title={hint}
    aria-label={hint}
  >
    <span class="ime-dot" aria-hidden="true"></span>
    <span class="ime-label">{label}</span>
  </button>
{/if}

<style>
  .ime-pill {
    position: fixed;
    top: 10px;
    left: 10px;
    z-index: 10000;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 9px;
    border-radius: 999px;
    border: 1px solid rgba(120, 130, 150, 0.4);
    background: rgba(30, 30, 30, 0.55);
    color: #cfd4dd;
    font-size: 0.72rem;
    font-weight: 700;
    line-height: 1.4;
    letter-spacing: 0.5px;
    cursor: pointer;
    /* Unobtrusive on office PCs: faded until hovered/focused. */
    opacity: 0.45;
    transition: opacity 0.15s, background 0.15s, border-color 0.15s;
    -webkit-user-select: none;
    user-select: none;
  }
  .ime-pill:hover,
  .ime-pill:focus-visible {
    opacity: 1;
    background: rgba(40, 40, 46, 0.95);
    border-color: rgba(74, 105, 189, 0.7);
    outline: none;
  }
  .ime-pill.on {
    opacity: 0.7;
    color: #e9edf4;
    border-color: rgba(74, 105, 189, 0.55);
  }
  .ime-pill.on:hover,
  .ime-pill.on:focus-visible {
    opacity: 1;
  }
  .ime-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #6b7280; /* grey = English pass-through */
  }
  .ime-pill.on .ime-dot {
    background: #4ade80; /* green = alternate layout active */
  }
  .ime-label {
    font-family: system-ui, sans-serif;
  }
</style>
