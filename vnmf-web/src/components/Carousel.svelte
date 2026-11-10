<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // Full-screen viewer of a set of images (the book's covers): arrows, keys, swipes, thumbnails.
  import { untrack } from 'svelte';
  import { t } from '../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** `items`: [{ src, caption }]. */
  let { items, start = 0, onclose } = $props();
  // Opens at `start`, then moves on its own.
  let index = $state(untrack(() => start));
  let touchX = null;

  const item = $derived(items[index]);
  const many = $derived(items.length > 1);

  const go = (step) => (index = (index + step + items.length) % items.length);

  function key(e) {
    if (e.key === 'Escape') onclose();
    else if (e.key === 'ArrowRight') go(1);
    else if (e.key === 'ArrowLeft') go(-1);
    else if (e.key === 'Home') index = 0;
    else if (e.key === 'End') index = items.length - 1;
    else return;
    e.preventDefault();
  }

  function touchEnd(e) {
    if (touchX == null) return;
    const dx = e.changedTouches[0].clientX - touchX;
    touchX = null;
    if (Math.abs(dx) > 50) go(dx < 0 ? 1 : -1);
  }

  // The neighbours load in advance.
  $effect(() => {
    if (!many) return;
    for (const step of [1, -1]) new Image().src = items[(index + step + items.length) % items.length].src;
  });

  // While open, the page underneath doesn't scroll.
  $effect(() => {
    const before = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => (document.body.style.overflow = before);
  });
</script>

<svelte:window onkeydown={key} />

<div class="viewer" role="dialog" aria-modal="true" aria-label={item.caption}>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="stage" onclick={(e) => e.target === e.currentTarget && onclose()} ontouchstart={(e) => (touchX = e.touches[0].clientX)} ontouchend={touchEnd}>
    {#key index}
      <img src={item.src} alt={item.caption} />
    {/key}
  </div>

  <header>
    <span class="caption">{item.caption}{#if many}<span class="count">{index + 1} / {items.length}</span>{/if}</span>
    <button class="osd circular" onclick={onclose} aria-label={t('close')} title={t('close')}><Icon name="close" /></button>
  </header>

  {#if many}
    <button class="osd circular nav prev" onclick={() => go(-1)} aria-label={t('carousel.previous')}><Icon name="back" /></button>
    <button class="osd circular nav next" onclick={() => go(1)} aria-label={t('carousel.next')}><Icon name="forward" /></button>
    <div class="thumbs">
      {#each items as it, i (i)}
        <button class="thumb" class:active={i === index} onclick={() => (index = i)} aria-label={it.caption} title={it.caption}>
          <img src={it.src} alt="" loading="lazy" />
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .viewer {
    position: fixed;
    inset: 0;
    z-index: 200;
    background: rgb(0 0 0 / 0.7);
    color: #fff;
    animation: fade 0.15s ease-out;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  .stage {
    position: absolute;
    inset: 56px 72px 104px;
    display: flex;
    align-items: center;
    justify-content: center;
    touch-action: pan-y;
  }
  /* A flex item in a box of definite size: percentages limit it on both sides. */
  .stage img {
    max-width: 100%;
    max-height: 100%;
    min-width: 0;
    min-height: 0;
    object-fit: contain;
    border-radius: 8px;
    box-shadow: 0 8px 40px rgb(0 0 0 / 0.6);
    animation: fade 0.2s ease-out;
  }
  header {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 12px 10px 20px;
  }
  .caption {
    font-weight: 700;
  }
  .count {
    margin-left: 12px;
    font-weight: 600;
    color: rgb(255 255 255 / 0.6);
  }
  /* GNOME's OSD buttons over images. */
  .osd {
    background: rgb(0 0 0 / 0.55);
    color: #fff;
    width: 44px;
    min-width: 44px;
    height: 44px;
    backdrop-filter: blur(8px);
  }
  .osd:hover {
    background: rgb(0 0 0 / 0.75);
  }
  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
  }
  .prev {
    left: 16px;
  }
  .next {
    right: 16px;
  }
  .thumbs {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 16px;
    display: flex;
    justify-content: center;
    gap: 8px;
    padding: 0 16px;
    overflow-x: auto;
  }
  .thumb {
    flex: none;
    width: 52px;
    height: 72px;
    padding: 0;
    border-radius: 6px;
    overflow: hidden;
    background: rgb(255 255 255 / 0.1);
    opacity: 0.55;
    outline: 2px solid transparent;
    outline-offset: 2px;
    transition: opacity 0.15s;
  }
  .thumb:hover {
    opacity: 0.85;
    background: rgb(255 255 255 / 0.1);
  }
  .thumb.active {
    opacity: 1;
    outline-color: var(--accent-bg);
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  @media (max-width: 600px) {
    .nav {
      display: none;
    }
    .stage {
      inset: 56px 8px 100px;
    }
    .viewer :global(.stage) {
      inset: 56px 8px 100px;
    }
  }
</style>
