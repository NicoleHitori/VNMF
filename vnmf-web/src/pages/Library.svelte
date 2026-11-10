<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { onMount } from 'svelte';
  import { fly, fade } from 'svelte/transition';
  import SkeletonCard from '../components/SkeletonCard.svelte';
  import { api } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { route, setQuery } from '../lib/router.svelte.js';
  import { toastError } from '../lib/toast.svelte.js';
  import BookCard from '../components/BookCard.svelte';
  import Icon from '../components/Icon.svelte';
  import FilterPanel from '../components/FilterPanel.svelte';
  import { lists, loadLists, listName } from '../lib/lists.svelte.js';

  const PAGE = 60;
  /** Filters with several values (comma-separated in the URL and the API). */
  const LISTS = ['genre', 'tag', 'country', 'language', 'kind', 'status', 'age', 'publication', 'demographic', 'color', 'translation'];
  const RANGES = ['chapters_min', 'chapters_max', 'pages_min', 'pages_max', 'year_min', 'year_max'];

  function fromQuery() {
    const f = {};
    for (const k of LISTS) f[k] = (route.query[k] ?? '').split(',').filter(Boolean);
    for (const k of RANGES) f[k] = route.query[k] ?? '';
    return f;
  }

  let q = $state(route.query.q ?? '');
  let filters = $state(fromQuery());
  let reading = $state(route.query.reading ?? '');
  let list = $state(route.query.list ?? '');
  let health = $state(route.query.health ?? '');
  let sort = $state(route.query.sort ?? 'title');
  let panelOpen = $state(false);

  let items = $state([]);
  let total = $state(0);
  let loading = $state(false);
  // Filters changed and their books aren't here yet: card outlines are shown instead of stale books.
  let pending = $state(true);
  let recent = $state([]);
  let recentLoaded = $state(false);
  let facets = $state(null);
  let request = 0;

  const isSet = (v) => v !== '' && v != null;
  const active = $derived(LISTS.reduce((n, k) => n + filters[k].length, 0) + RANGES.filter((k) => isSet(filters[k])).length);
  /** The name of the chosen list (lists live on the account page; the library only shows one). */
  const listLabel = $derived.by(() => {
    if (list === 'any') return t('lists.inLists');
    const found = lists.items.find((l) => String(l.id) === list);
    return found ? listName(found) : '';
  });
  const filtered = $derived(Boolean(q || reading || health || list || active));

  /** Everything that goes into the URL and the request. */
  function filterQuery() {
    const out = { q, reading, health, list };
    for (const k of LISTS) out[k] = filters[k].join(',');
    for (const k of RANGES) out[k] = isSet(filters[k]) ? String(filters[k]) : '';
    return out;
  }

  function params(offset) {
    const p = new URLSearchParams({ limit: PAGE, offset, sort });
    if (sort === 'added' || sort === 'updated') p.set('order', 'desc');
    for (const [k, v] of Object.entries(filterQuery())) if (v) p.set(k, v);
    return p;
  }

  async function load(reset) {
    const mine = ++request;
    loading = true;
    try {
      const res = await api(`/books?${params(reset ? 0 : items.length)}`);
      if (mine !== request) return;
      items = reset ? res.items : [...items, ...res.items];
      total = res.total;
      if (reset) pending = false;
    } catch (e) {
      toastError(e);
      if (mine === request) pending = false;
    } finally {
      if (mine === request) loading = false;
    }
  }

  // Filters → the URL and a (debounced) reload.
  $effect(() => {
    const current = { ...filterQuery(), sort: sort === 'title' ? '' : sort };
    pending = true;
    const timer = setTimeout(() => {
      setQuery(current);
      load(true);
    }, 300);
    return () => clearTimeout(timer);
  });

  onMount(async () => {
    try {
      [recent, facets] = await Promise.all([api('/progress?limit=12').then((r) => r.items), api('/books/facets'), loadLists()]);
    } catch (e) {
      toastError(e);
    } finally {
      recentLoaded = true;
    }
  });

  function resetFilters() {
    for (const k of LISTS) filters[k] = [];
    for (const k of RANGES) filters[k] = '';
  }

  function clear() {
    q = reading = health = list = '';
    resetFilters();
  }
</script>

<section class="catalog">
  <div class="results">
  <div class="toolbar">
    <label class="search">
      <Icon name="search" />
      <input type="search" placeholder={t('library.search')} bind:value={q} aria-label={t('library.search')} />
    </label>
    <select bind:value={reading} aria-label={t('library.reading')}>
      <option value="">{t('library.allBooks')}</option>
      <option value="unread">{t('library.unread')}</option>
      <option value="reading">{t('library.inProgress')}</option>
      <option value="finished">{t('library.finished')}</option>
    </select>
    <select bind:value={sort} aria-label={t('library.sort')}>
      <option value="title">{t('sort.title')}</option>
      <option value="added">{t('sort.added')}</option>
      <option value="read">{t('sort.read')}</option>
      <option value="updated">{t('sort.updated')}</option>
      <option value="pages">{t('sort.pages')}</option>
      <option value="size">{t('sort.size')}</option>
    </select>
    {#if list}<button class="chip accent" onclick={() => (list = '')}><Icon name="bookmark" />{listLabel} ✕</button>{/if}
    {#if health}<button class="chip" onclick={() => (health = '')}>{t(`health.${health}`)} ✕</button>{/if}
    {#if filtered}<button class="ghost" onclick={clear}>{t('library.clear')}</button>{/if}
    <button class="filters-button" class:on={active} onclick={() => (panelOpen = true)}>
      <Icon name="filter" />{t('filter.title')}{#if active}<span class="badge-count">{active}</span>{/if}
    </button>
    <span class="muted count">{t('library.count', { n: total })}</span>
  </div>

  {#if pending}
    <div class="grid" in:fade={{ duration: 120 }}>
      {#each Array.from({ length: Math.min(18, Math.max(6, items.length || 12)) }, (_, i) => i) as i (i)}
        <SkeletonCard />
      {/each}
    </div>
  {:else if items.length}
    <div class="grid">
      {#each items as book, i (book.id)}
        <div class="card-in" in:fly|global={{ y: 14, duration: 260, delay: Math.min(i % PAGE, 14) * 22 }}>
          <BookCard {book} />
        </div>
      {/each}
    </div>
    {#if items.length < total}
      <div class="more"><button onclick={() => load(false)} disabled={loading}>{t('library.more')}</button></div>
    {/if}
  {:else}
    <div class="center muted" in:fade={{ duration: 200 }}>
      {#if filtered}
        <p>{t('library.nothingFound')}</p>
      {:else}
        <p>{t('library.empty')}</p>
        <a href="/upload">{t('nav.upload')}</a>
      {/if}
    </div>
  {/if}

  <!-- Always the same, whatever the filters. -->
  {#if !recentLoaded || recent.length}
    <section class="recent">
      <h2 class="heading">{t('library.continue')}</h2>
      <div class="shelf">
        {#if !recentLoaded}
          {#each [0, 1, 2, 3, 4] as i (i)}<SkeletonCard />{/each}
        {:else}
          {#each recent as book, i (book.id)}
            <div in:fly|global={{ x: 16, duration: 260, delay: Math.min(i, 8) * 30 }}><BookCard {book} /></div>
          {/each}
        {/if}
      </div>
    </section>
  {/if}
  </div>

  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="scrim" class:open={panelOpen} onclick={() => (panelOpen = false)}></div>
  <aside class="filters" class:open={panelOpen} aria-label={t('filter.title')}>
    <FilterPanel bind:filters {facets} onreset={resetFilters} />
    <button class="primary pill show" onclick={() => (panelOpen = false)}>{t('filter.show', { n: total })}</button>
  </aside>
</section>

<style>
  /* "Continue reading" under the catalog. */
  .recent {
    margin-top: 36px;
  }
  /* The catalog with the filters on the right (a drawer on narrow screens). */
  .catalog {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 19rem;
    gap: 28px;
    align-items: start;
  }
  .filters {
    position: sticky;
    top: 64px;
    max-height: calc(100dvh - 80px);
    overflow-y: auto;
    padding: 2px 4px 16px;
    scrollbar-width: thin;
  }
  .filters .show,
  .filters-button,
  .scrim {
    display: none;
  }
  .filters-button.on {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .badge-count {
    min-width: 20px;
    height: 20px;
    padding: 0 6px;
    border-radius: 9999px;
    background: var(--accent-bg);
    color: var(--accent-fg);
    font-size: 0.75rem;
    display: inline-grid;
    place-items: center;
  }
  @media (max-width: 1000px) {
    .catalog {
      grid-template-columns: 1fr;
    }
    .filters-button {
      display: inline-flex;
    }
    .filters {
      position: fixed;
      top: 0;
      right: 0;
      bottom: 0;
      z-index: 60;
      width: min(24rem, 92vw);
      max-height: none;
      padding: 16px;
      background: var(--window-bg);
      box-shadow: var(--popover-shadow);
      transform: translateX(105%);
      transition: transform 0.2s ease-out;
    }
    .filters.open {
      transform: none;
    }
    /* Stays at the bottom of the drawer while its content scrolls. */
    .filters .show {
      display: inline-flex;
      position: sticky;
      bottom: 0;
      width: 100%;
      margin-top: 20px;
      box-shadow: 0 0 0 8px var(--window-bg);
    }
    .scrim {
      display: block;
      position: fixed;
      inset: 0;
      z-index: 59;
      background: rgb(0 0 6 / 0.35);
      opacity: 0;
      pointer-events: none;
      transition: opacity 0.2s;
    }
    .scrim.open {
      opacity: 1;
      pointer-events: auto;
    }
  }
  .shelf {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: 9rem;
    gap: 1rem;
    overflow-x: auto;
    padding-bottom: 0.5rem;
    scroll-snap-type: x proximity;
  }
  .shelf > :global(*) {
    scroll-snap-align: start;
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    align-items: center;
    margin-bottom: 1rem;
  }
  .search {
    flex: 1 1 16rem;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 12px;
    border-radius: 9999px;
    background: var(--entry-bg);
    color: var(--dim);
  }
  .search:focus-within {
    outline: 2px solid color-mix(in srgb, var(--accent-bg), transparent 30%);
    outline-offset: -2px;
  }
  .search input {
    flex: 1;
    background: transparent;
    padding-left: 0;
    color: var(--fg);
  }
  .search input:focus {
    outline: none;
  }
  .toolbar select {
    flex: 0 1 auto;
    max-width: 14rem;
  }
  .count {
    margin-left: auto;
    font-size: 0.9rem;
  }
  button.chip {
    border: none;
    cursor: pointer;
  }
  .card-in {
    min-width: 0;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(9.5rem, 1fr));
    gap: 1.2rem 1rem;
  }
  .more {
    display: flex;
    justify-content: center;
    margin-top: 1.5rem;
  }
  @media (max-width: 600px) {
    .grid {
      grid-template-columns: repeat(auto-fill, minmax(7.5rem, 1fr));
    }
    .shelf {
      grid-auto-columns: 7.5rem;
    }
  }
</style>
