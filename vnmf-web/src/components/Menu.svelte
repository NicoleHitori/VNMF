<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // A menu button with a popover (like GtkMenuButton): closes on a click outside, on Esc, on choosing.
  let { label, button, children, align = 'end', flat = true } = $props();
  let open = $state(false);
  let root;

  function outside(e) {
    if (open && !root.contains(e.target)) open = false;
  }

  function key(e) {
    if (open && e.key === 'Escape') open = false;
  }
</script>

<svelte:window onpointerdown={outside} onkeydown={key} />

<div class="menu" bind:this={root}>
  <button class:flat class:open aria-haspopup="menu" aria-expanded={open} aria-label={label} title={label} onclick={() => (open = !open)}>
    {@render button()}
  </button>
  {#if open}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div class="popover {align}" role="menu" tabindex="-1" onclick={() => (open = false)}>
      {@render children()}
    </div>
  {/if}
</div>

<style>
  .menu {
    position: relative;
  }
  .popover {
    position: absolute;
    top: calc(100% + 6px);
    z-index: 50;
    min-width: 12rem;
    padding: 6px;
    border-radius: 12px;
    background: var(--popover-bg);
    color: var(--fg);
    box-shadow: var(--popover-shadow);
    display: flex;
    flex-direction: column;
  }
  .popover.end {
    right: 0;
  }
  .popover.start {
    left: 0;
  }
  .popover :global(a),
  .popover :global(button) {
    justify-content: flex-start;
    width: 100%;
    padding: 6px 10px;
    border-radius: 6px;
    background: transparent;
    color: inherit;
    text-decoration: none;
    font-weight: normal;
  }
  .popover :global(a:hover),
  .popover :global(button:hover) {
    background: var(--hover);
  }
  .popover :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 6px 4px;
  }
  .popover :global(.current) {
    color: var(--accent);
    font-weight: 600;
  }
</style>
