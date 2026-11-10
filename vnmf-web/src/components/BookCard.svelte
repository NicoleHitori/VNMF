<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { t } from '../lib/i18n.svelte.js';

  let { book } = $props();
  let loaded = $state(false);
  let img = $state();

  // From the cache the image may be there before `onload` is listened to.
  $effect(() => {
    if (img?.complete && img.naturalWidth) loaded = true;
  });

  const fraction = $derived(
    book.progress ? (book.progress.finished ? 1 : Math.min(1, (book.progress.page + 1) / Math.max(1, book.page_count))) : 0,
  );
</script>

<a class="book" href="/book/{book.id}" title={book.title}>
  <div class="cover" class:shimmer={!loaded}>
    <img bind:this={img} class:loaded src={book.cover_url} alt="" loading="lazy" decoding="async" onload={() => (loaded = true)} onerror={() => (loaded = true)} />
    <div class="badges">
      {#if book.webtoon}<span class="badge">{t('badge.webtoon')}</span>{/if}
      {#if book.health?.status === 'damaged'}<span class="badge danger">{t('badge.damaged')}</span>{/if}
      {#if book.missing}<span class="badge danger">{t('badge.missing')}</span>{/if}
      {#if book.progress?.finished}<span class="badge success">{t('badge.finished')}</span>{/if}
    </div>
    {#if fraction > 0 && !book.progress?.finished}
      <div class="progress"><div style:width="{fraction * 100}%"></div></div>
    {/if}
  </div>
  <div class="title">{book.title}</div>
  {#if book.authors.length}<div class="authors muted">{book.authors.join(', ')}</div>{/if}
</a>

<style>
  .book {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    color: inherit;
    text-decoration: none;
    min-width: 0;
  }
  .cover {
    position: relative;
    aspect-ratio: 2 / 3;
    border-radius: 10px;
    overflow: hidden;
    box-shadow: var(--card-shadow);
    transition:
      transform 0.15s,
      box-shadow 0.15s;
  }
  .book:hover {
    text-decoration: none;
  }
  .book:hover .cover {
    transform: translateY(-3px);
    box-shadow: var(--popover-shadow);
  }
  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
    opacity: 0;
    transition: opacity 0.3s ease-out;
  }
  img.loaded {
    opacity: 1;
  }
  .badges {
    position: absolute;
    top: 0.35rem;
    left: 0.35rem;
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
  }
  .progress {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 4px;
    background: rgb(0 0 0 / 0.4);
  }
  .progress div {
    height: 100%;
    background: var(--accent-bg);
  }
  .title {
    font-weight: 700;
    font-size: 0.92rem;
    line-height: 1.3;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .authors {
    font-size: 0.82rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
