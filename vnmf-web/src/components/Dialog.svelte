<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  /** A modal dialog; open while `open` is true. Esc / the close button call `onclose`. */
  import Icon from './Icon.svelte';

  let { open, title, onclose, children, wide = false } = $props();
  let dialog;

  $effect(() => {
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  });
</script>

<dialog bind:this={dialog} class:wide onclose={() => open && onclose?.()}>
  <header>
    <h2>{title}</h2>
    <button class="flat icon circular" onclick={() => onclose?.()} aria-label="✕"><Icon name="close" /></button>
  </header>
  <div class="body">
    {#if open}{@render children?.()}{/if}
  </div>
</dialog>

<style>
  dialog {
    max-width: min(34rem, calc(100vw - 1.5rem));
  }
  dialog.wide {
    max-width: min(56rem, calc(100vw - 1.5rem));
  }
  header {
    display: grid;
    grid-template-columns: 34px 1fr 34px;
    align-items: center;
    padding: 6px 8px;
    position: sticky;
    top: 0;
    background: var(--dialog-bg);
    z-index: 1;
  }
  header::before {
    content: '';
  }
  h2 {
    margin: 0;
    text-align: center;
    font-size: 1rem;
  }
  .body {
    padding: 8px 20px 20px;
  }
</style>
