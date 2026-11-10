<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // The reader: paged mode (single pages / spreads, left-to-right or right-to-left) and strip mode
  // (webtoons: one continuous column). Plays the book's music and effects, shows text layers and dark
  // versions of pages, saves the position (page; in a strip also the fragment and the offset in it).
  import { onMount, onDestroy, tick, untrack } from 'svelte';
  import { api, urls } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { route, navigate } from '../lib/router.svelte.js';
  import { session, updateSettings } from '../lib/session.svelte.js';
  import { toast, toastError } from '../lib/toast.svelte.js';
  import { SoundPlayer } from '../lib/audio.js';
  import Icon from '../components/Icon.svelte';

  let { id } = $props();

  let m = $state(null); // the manifest
  let error = $state('');
  let groups = $state([]);
  let page = $state(0); // the current page (paged: the first page of the shown group)
  let ui = $state(true);
  let panel = $state(null); // contents | bookmarks | text | settings | end
  let bookmarks = $state([]);
  let texts = $state([]);
  let wide = $state(window.innerWidth >= 900 && window.innerWidth > window.innerHeight);
  let ready = false; // the start position is applied — from now on positions are saved

  let stripEl = $state();
  let pagedEl = $state();
  let imgs = []; // strip images in order
  let chunkEl = new Map(); // chunk index → strip image
  let stripPos = { chunk: null, offset: 0 };
  let stripFinished = false;
  let stripSfx = [];
  let lastBottom = 0;
  let skipSfx = false;

  // A strip shows only a window of pages around the current one: a long webtoon is taller than a
  // browser can lay out (Firefox stops at ~17.9 million px, Chrome at ~33.5 million) and everything
  // below that would never show. The window is limited by height in strip widths and moves as the
  // reader scrolls, keeping the same place on screen.
  const WINDOW_WIDTHS = 2500; // the window's height: 2.5 million px for a 1000 px wide strip
  const SHIFT_AT = WINDOW_WIDTHS / 4; // re-centred when less than this is left beyond the screen
  let win = $state({ from: 0, to: -1 }); // pages shown, inclusive
  let heights = []; // cumulative page heights in strip widths: heights[i] = pages before i
  let shifting = false;

  const player = new SoundPlayer();
  const playbackCache = new Map();

  const prefs = $derived(session.settings.reader);
  // Separate values, so that changing one setting doesn't re-run what depends on another
  // (a new settings object would otherwise touch every image and refetch the page groups).
  const dark = $derived(prefs.dark);
  const coverAlone = $derived(prefs.coverAlone);
  const sound = $derived(prefs.sound);
  let liveVolume = $state(null);
  const volume = $derived(liveVolume ?? prefs.volume);
  // The strip width while the slider is dragged: only a CSS variable changes, saved on release.
  let liveWidth = $state(null);
  const stripWidth = $derived(liveWidth ?? prefs.stripWidth);
  const info = $derived(m?.info);
  const strip = $derived(prefs.mode === 'strip' || (prefs.mode === 'auto' && !!info?.webtoon));
  const double = $derived(prefs.mode === 'double' || (prefs.mode === 'auto' && wide));
  const rtl = $derived(prefs.direction === 'auto' ? !!info?.rtl : prefs.direction === 'rtl');
  const gi = $derived(groups.findIndex((g) => g.pages.includes(page)));
  const group = $derived(groups[gi] ?? null);
  const hasSound = $derived(!!(info?.has_music || info?.has_sfx));

  const chapter = $derived.by(() => {
    if (!m?.contents.structured) return null;
    for (const v of m.contents.volumes)
      for (const c of v.chapters) if (page >= c.first_page && page < c.first_page + c.page_count) return { v, c };
    return null;
  });
  const chapterLabel = $derived(
    chapter
      ? [m.contents.has_volumes && chapter.v.number ? t('book.volume', { n: chapter.v.number }) : '', t('book.chapter', { n: chapter.c.number }), chapter.c.title]
          .filter(Boolean)
          .join(' · ')
      : '',
  );

  function src(p, k) {
    const pg = m.pages[p];
    if (dark && pg.dark_parts.includes(k)) return urls.dark(id, p, k, info.version);
    return urls.chunk(id, pg.chunks[k].index, info.version);
  }

  function chapterStartTitle(p) {
    for (const v of m.contents.volumes)
      for (const c of v.chapters) if (c.first_page === p) return [t('book.chapter', { n: c.number }), c.title].filter(Boolean).join(' — ');
    return '';
  }

  // ------------------------------------------------------------------
  // Loading and the start position
  // ------------------------------------------------------------------

  onMount(async () => {
    try {
      const [manifest, progress, marks] = await Promise.all([
        api(`/books/${id}/manifest`),
        api(`/books/${id}/progress`),
        api(`/books/${id}/bookmarks`).then((r) => r.items),
      ]);
      bookmarks = marks;
      const requested = route.query.page != null ? Number(route.query.page) : null;
      const resume = progress && !progress.finished ? progress : null;
      page = Math.min(Math.max(0, requested ?? resume?.page ?? 0), manifest.pages.length - 1);
      if (requested == null && resume?.chunk != null) stripPos = { chunk: resume.chunk, offset: resume.offset ?? 0 };
      heights = [0];
      for (const p of manifest.pages) heights.push(heights.at(-1) + p.chunks.reduce((sum, c) => sum + c.height / Math.max(1, c.width), 0));
      win = windowAround(page, manifest.pages.length);
      m = manifest;
      if (manifest.info.has_sfx) api(`/books/${id}/strip-sfx`).then((r) => (stripSfx = r.sfx)).catch(() => {});
      setTimeout(() => (ui = false), 2500);
    } catch (e) {
      error = e.message;
    }
  });

  // Paged: the groups for the current mode; the current page stays where it was.
  $effect(() => {
    if (!m || strip) return;
    const query = `double=${double}&cover_alone=${coverAlone}&rtl=${rtl}`;
    api(`/books/${id}/groups?${query}`)
      .then((res) => {
        groups = res.groups;
        const keep = untrack(() => page);
        const g = res.groups.find((x) => x.pages.includes(keep));
        page = g ? g.pages[0] : 0;
        ready = true;
      })
      .catch(toastError);
  });

  // Strip: lay out, then scroll to the position.
  $effect(() => {
    if (!m || !strip || !stripEl) return;
    untrack(() => initStrip());
  });

  /** Pages around `center` that fit in the window's height (the centre page always). */
  function windowAround(center, count = m.pages.length) {
    let from = center;
    let to = center;
    for (let grew = true; grew; ) {
      grew = false;
      if (to + 1 < count && heights[to + 2] - heights[from] <= WINDOW_WIDTHS) {
        to++;
        grew = true;
      }
      if (from > 0 && heights[to + 1] - heights[from - 1] <= WINDOW_WIDTHS) {
        from--;
        grew = true;
      }
    }
    return { from, to };
  }

  /** Strip elements in order: an image per fragment, or a column of tiles for a fragment taller than
   * browsers decode (32767 px; `tiles` in the manifest, cut by the server). */
  function collectImages() {
    imgs = Array.from(stripEl.querySelectorAll('[data-chunk]'));
    chunkEl = new Map(imgs.map((el) => [Number(el.dataset.chunk), el]));
  }

  /** Makes page `p` part of the window (re-centred on it if it isn't). */
  async function reveal(p) {
    if (p >= win.from && p <= win.to) return;
    win = windowAround(p);
    await tick();
    collectImages();
  }

  /** Near an end of the window (and not the book's): re-centre it, keeping the place on screen. */
  async function shiftWindow() {
    if (shifting) return;
    const ahead = win.to < m.pages.length - 1 && heights[win.to + 1] - heights[page + 1] < SHIFT_AT;
    const behind = win.from > 0 && heights[page] - heights[win.from] < SHIFT_AT;
    if (!ahead && !behind) return;
    shifting = true;
    const anchor = imageAt(stripEl.scrollTop);
    const chunk = Number(anchor.dataset.chunk);
    const delta = stripEl.scrollTop - anchor.offsetTop;
    win = windowAround(page);
    await tick();
    collectImages();
    const el = chunkEl.get(chunk);
    if (el) {
      skipSfx = true;
      stripEl.scrollTop = el.offsetTop + delta;
    }
    lastBottom = stripEl.scrollTop + stripEl.clientHeight;
    shifting = false;
    loadAround();
  }

  async function initStrip() {
    await tick();
    collectImages();
    const target = (stripPos.chunk != null && chunkEl.get(stripPos.chunk)) || imgs.find((el) => Number(el.dataset.page) === page);
    if (target) stripEl.scrollTop = target.offsetTop + (stripPos.chunk != null ? stripPos.offset * target.offsetHeight : 0);
    lastBottom = stripEl.scrollTop + stripEl.clientHeight;
    loadAround();
    stripEl.focus({ preventScroll: true });
    ready = true;
  }

  // Strip images get their address only near the screen: two screens back and three ahead, and at
  // least a few images each way — like the desktop reader. Once loaded, an image keeps it (no
  // flicker when scrolling back). Done on the elements directly: thousands of images, no re-render.
  const BACK_SCREENS = 2;
  const AHEAD_SCREENS = 3;
  const MIN_IMAGES = 3;

  function loadAround() {
    if (!imgs.length) return;
    const h = stripEl.clientHeight;
    const top = stripEl.scrollTop;
    let from = imgs.indexOf(imageAt(Math.max(0, top - BACK_SCREENS * h)));
    let to = imgs.indexOf(imageAt(top + (1 + AHEAD_SCREENS) * h));
    const here = imgs.indexOf(imageAt(top));
    from = Math.max(0, Math.min(from, here - MIN_IMAGES));
    to = Math.min(imgs.length - 1, Math.max(to, here + MIN_IMAGES));
    for (let i = from; i <= to; i++) show(imgs[i]);
  }

  /** Gives the images of a strip element their address; `loadedOnly` — only those that had one. */
  function show(el, loadedOnly = false) {
    for (const img of el.tagName === 'IMG' ? [el] : el.querySelectorAll('img')) {
      if (loadedOnly && !img.hasAttribute('src')) continue;
      if (img.getAttribute('src') !== img.dataset.src) img.src = img.dataset.src;
    }
  }

  const TILE_HEIGHT = 8192; // as the server cuts them
  const tileHeight = (c, t) => Math.min(TILE_HEIGHT, c.height - t * TILE_HEIGHT);

  // Dark pages switched: loaded strip images take the other version.
  $effect(() => {
    if (!strip) return;
    dark;
    untrack(async () => {
      await tick();
      if (!stripEl) return;
      collectImages(); // a dark version may replace tiles with an image or back
      for (const el of imgs) show(el, true);
      loadAround();
    });
  });

  /** Keeps the same place of the strip on screen while its width (and so all heights) changes. */
  function keepPlace(change) {
    if (!strip || !imgs.length) return change();
    const el = imageAt(stripEl.scrollTop);
    const fraction = (stripEl.scrollTop - el.offsetTop) / Math.max(1, el.offsetHeight);
    change();
    requestAnimationFrame(() => {
      skipSfx = true;
      stripEl.scrollTop = el.offsetTop + fraction * el.offsetHeight;
      lastBottom = stripEl.scrollTop + stripEl.clientHeight;
      loadAround();
    });
  }

  // ------------------------------------------------------------------
  // Navigation
  // ------------------------------------------------------------------

  function next() {
    if (gi < groups.length - 1) page = groups[gi + 1].pages[0];
    else if (gi === groups.length - 1) {
      panel = 'end';
      save();
    }
  }

  function prev() {
    if (gi > 0) page = groups[gi - 1].pages[0];
  }

  async function goTo(p) {
    panel = null;
    if (strip) {
      await reveal(p);
      const el = imgs.find((x) => Number(x.dataset.page) === p);
      if (el) {
        skipSfx = true;
        stripEl.scrollTop = el.offsetTop;
      }
    } else {
      const g = groups.find((x) => x.pages.includes(p));
      page = g ? g.pages[0] : p;
    }
  }

  // A new group: back to the top (when fitting the width) and preload two groups back and two ahead.
  // The Image objects are kept until the next turn, so the browser doesn't drop the loading.
  let preloaded = [];
  $effect(() => {
    if (strip || !group) return;
    dark;
    if (pagedEl) pagedEl.scrollTop = 0;
    untrack(() => {
      const around = [...groups.slice(gi + 1, gi + 3), ...groups.slice(Math.max(0, gi - 2), gi)];
      preloaded = around.flatMap((g) =>
        g.pages.flatMap((p) =>
          m.pages[p].chunks.map((_, k) => {
            const img = new Image();
            img.decoding = 'async';
            img.src = src(p, k);
            return img;
          }),
        ),
      );
    });
  });

  let touchX = null;
  let swiped = false;

  function onPagedClick(e) {
    if (swiped) {
      swiped = false;
      return;
    }
    const x = e.clientX / window.innerWidth;
    if (x < 0.3) rtl ? next() : prev();
    else if (x > 0.7) rtl ? prev() : next();
    else ui = !ui;
  }

  function onTouchStart(e) {
    touchX = e.touches.length === 1 ? e.touches[0].clientX : null;
  }

  function onTouchEnd(e) {
    if (touchX == null || window.visualViewport?.scale > 1.01) return;
    const dx = e.changedTouches[0].clientX - touchX;
    touchX = null;
    if (Math.abs(dx) < 60) return;
    swiped = true;
    setTimeout(() => (swiped = false), 400);
    // Swiping left shows what's on the right.
    if (dx < 0) rtl ? prev() : next();
    else rtl ? next() : prev();
  }

  function onKey(e) {
    if (e.target.closest?.('input, textarea, select') || e.ctrlKey || e.metaKey || e.altKey) return;
    const k = e.key;
    if (k === 'Escape') {
      if (panel) panel = null;
      else navigate(`/book/${id}`);
      return;
    }
    const shortcuts = { c: 'contents', b: 'bookmarks', t: 'text', s: 'settings' };
    if (shortcuts[k.toLowerCase()]) return togglePanel(shortcuts[k.toLowerCase()]);
    if (k === 'd' && info.has_dark) return updateSettings({ reader: { dark: !prefs.dark } });
    if (k === 'm' && hasSound) return toggleSound();
    if (k === 'f') return toggleFullscreen();
    if (strip || panel) return;
    const atBottom = !pagedEl || pagedEl.scrollTop + pagedEl.clientHeight >= pagedEl.scrollHeight - 2;
    const atTop = !pagedEl || pagedEl.scrollTop <= 1;
    let handled = true;
    if (k === 'ArrowRight') rtl ? prev() : next();
    else if (k === 'ArrowLeft') rtl ? next() : prev();
    else if ((k === ' ' || k === 'PageDown' || k === 'ArrowDown') && (prefs.fit === 'contain' || atBottom)) e.shiftKey && k === ' ' ? prev() : next();
    else if ((k === 'PageUp' || k === 'ArrowUp') && (prefs.fit === 'contain' || atTop)) prev();
    else if (k === 'Home') page = groups[0]?.pages[0] ?? 0;
    else if (k === 'End') page = groups.at(-1)?.pages[0] ?? 0;
    else handled = false;
    if (handled) e.preventDefault();
  }

  function onResize() {
    wide = window.innerWidth >= 900 && window.innerWidth > window.innerHeight;
    if (strip) requestAnimationFrame(loadAround);
  }

  function togglePanel(name) {
    panel = panel === name ? null : name;
  }

  function toggleFullscreen() {
    if (document.fullscreenElement) document.exitFullscreen();
    else document.documentElement.requestFullscreen?.().catch(() => {});
  }

  // ------------------------------------------------------------------
  // Strip scrolling: position, effects, the end
  // ------------------------------------------------------------------

  let raf = 0;

  function onStripScroll() {
    if (!raf) raf = requestAnimationFrame(stripTick);
  }

  /** The last image whose top is at or above `y` (binary search). */
  function imageAt(y) {
    let lo = 0;
    let hi = imgs.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (imgs[mid].offsetTop <= y) lo = mid;
      else hi = mid - 1;
    }
    return imgs[lo];
  }

  function stripTick() {
    raf = 0;
    if (!ready || !imgs.length) return;
    const top = stripEl.scrollTop;
    const bottom = top + stripEl.clientHeight;
    const el = imageAt(top);
    stripPos = { chunk: Number(el.dataset.chunk), offset: Math.min(1, Math.max(0, (top - el.offsetTop) / Math.max(1, el.offsetHeight))) };
    page = Number(el.dataset.page);
    // Finished once the end of the strip (the "you have reached the end" line) comes on screen.
    const end = win.to === m.pages.length - 1 ? stripEl.querySelector('.strip-end') : null;
    stripFinished = !!end && bottom >= end.offsetTop + 8;
    // Effects: their trigger line came on screen from below while scrolling down.
    if (sound && !skipSfx) {
      for (const fx of stripSfx) {
        const target = chunkEl.get(fx.chunk);
        if (!target) continue;
        const y = target.offsetTop + fx.fraction * target.offsetHeight;
        if (y > lastBottom && y <= bottom) player.playEffect(fx.aux, urls.aux(id, fx.aux.index, info.version));
      }
    }
    skipSfx = false;
    lastBottom = bottom;
    loadAround();
    scheduleSave();
    shiftWindow();
  }

  // ------------------------------------------------------------------
  // Saving the position
  // ------------------------------------------------------------------

  let saveTimer = null;
  let lastSaved = '';

  function position() {
    if (strip) return { page, chunk: stripPos.chunk, offset: Math.round(stripPos.offset * 1e4) / 1e4, finished: stripFinished };
    return { page, finished: gi >= 0 && gi === groups.length - 1 && panel === 'end' };
  }

  function save(keepalive = false) {
    clearTimeout(saveTimer);
    if (!ready || !m) return;
    const pos = position();
    const key = JSON.stringify(pos);
    if (key === lastSaved) return;
    lastSaved = key;
    api(`/books/${id}/progress`, { method: 'PUT', json: { ...pos, updated_at: Math.floor(Date.now() / 1000) }, keepalive }).catch(() => {});
  }

  function scheduleSave() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(save, 1500);
  }

  // Paged reading moved: the saved strip position no longer applies; save the new page.
  $effect(() => {
    if (!m || strip) return;
    page;
    untrack(() => {
      stripPos = { chunk: null, offset: 0 };
      scheduleSave();
    });
  });

  function onVisibility() {
    if (document.visibilityState === 'hidden') save(true);
  }

  // ------------------------------------------------------------------
  // Sound
  // ------------------------------------------------------------------

  function playbackOf(p) {
    if (!playbackCache.has(p)) playbackCache.set(p, api(`/books/${id}/pages/${p}/playback`).catch(() => ({ music: null, sfx: [] })));
    return playbackCache.get(p);
  }

  let lastSfxKey = '';

  $effect(() => {
    player.setEnabled(sound);
  });

  $effect(() => {
    player.setVolume(volume);
  });

  $effect(() => {
    if (!m || !hasSound || !sound) return;
    const pages = strip ? [page] : (group?.pages ?? []);
    if (!pages.length) return;
    untrack(() => updateSound(pages));
  });

  async function updateSound(pages) {
    const key = pages.join(',');
    const infos = await Promise.all(pages.map(playbackOf));
    if (!prefs.sound) return;
    // Music started on either page of a spread continues through it: take the later page.
    const music = infos.at(-1).music;
    player.setMusic(music, music && urls.aux(id, music.index, info.version));
    if (!strip && key !== lastSfxKey) {
      lastSfxKey = key;
      for (const pb of infos) for (const fx of pb.sfx) player.playEffect(fx, urls.aux(id, fx.index, info.version));
    }
  }

  function toggleSound() {
    // The click itself lets the browser start audio.
    updateSettings({ reader: { sound: !prefs.sound } });
    if (prefs.sound) lastSfxKey = '';
  }

  // ------------------------------------------------------------------
  // Text layers and bookmarks
  // ------------------------------------------------------------------

  $effect(() => {
    if (panel !== 'text' || !m) return;
    const pages = (strip ? [page] : (group?.pages ?? [])).filter((p) => m.pages[p].has_text);
    Promise.all(pages.map((p) => api(`/books/${id}/pages/${p}/text`)))
      .then((res) => (texts = res))
      .catch(toastError);
  });

  async function addBookmark() {
    const pos = position();
    try {
      const b = await api(`/books/${id}/bookmarks`, { method: 'POST', json: strip ? { page, chunk: pos.chunk, offset: pos.offset } : { page } });
      bookmarks = [...bookmarks, b].sort((a, c) => a.page - c.page);
      toast(t('reader.bookmarkAdded'), 'success');
    } catch (e) {
      toastError(e);
    }
  }

  async function deleteBookmark(b) {
    try {
      await api(`/bookmarks/${b.id}`, { method: 'DELETE' });
      bookmarks = bookmarks.filter((x) => x.id !== b.id);
    } catch (e) {
      toastError(e);
    }
  }

  async function openBookmark(b) {
    if (strip) await reveal(b.page);
    if (strip && b.chunk != null && chunkEl.get(b.chunk)) {
      panel = null;
      const el = chunkEl.get(b.chunk);
      skipSfx = true;
      stripEl.scrollTop = el.offsetTop + (b.offset ?? 0) * el.offsetHeight;
    } else goTo(b.page);
  }

  onMount(() => {
    window.addEventListener('keydown', onKey);
    window.addEventListener('resize', onResize);
    document.addEventListener('visibilitychange', onVisibility);
  });

  onDestroy(() => {
    window.removeEventListener('keydown', onKey);
    window.removeEventListener('resize', onResize);
    document.removeEventListener('visibilitychange', onVisibility);
    cancelAnimationFrame(raf);
    save(true);
    player.stopAll();
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
  });

  const pageLabel = $derived.by(() => {
    if (!m) return '';
    if (strip || !group) return t('reader.pageOf', { page: page + 1, total: m.pages.length });
    const shown = [...group.pages].sort((a, b) => a - b).map((p) => p + 1);
    return t('reader.pageOf', { page: shown.length > 1 ? `${shown[0]}–${shown.at(-1)}` : shown[0], total: m.pages.length });
  });
</script>

<svelte:head><title>{m?.info.title ?? 'VNMF'}</title></svelte:head>

<div class="reader" class:ui-hidden={!ui && !panel}>
  {#if error}
    <div class="center"><p class="error-text">{error}</p><a href="/book/{id}">{t('back')}</a></div>
  {:else if !m}
    <div class="center muted">{t('loading')}</div>
  {:else}
    {#if strip}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
      <div class="strip" bind:this={stripEl} onscroll={onStripScroll} onclick={() => (ui = !ui)} tabindex="0" role="document" style:--strip-width="{stripWidth}px">
        <div class="strip-inner">
          {#each m.pages.slice(win.from, win.to + 1) as p (p.index)}
            {#if p.chapter_start && p.index > 0 && chapterStartTitle(p.index)}
              <div class="chapter-sep">{chapterStartTitle(p.index)}</div>
            {/if}
            {#each p.chunks as c, k (c.index)}
              {#if c.tiles && !(dark && p.dark_parts.includes(k))}
                <div class="tiled" data-page={p.index} data-chunk={c.index} style:aspect-ratio="{c.width} / {c.height}">
                  {#each Array.from({ length: c.tiles }, (_, i) => i) as ti (ti)}
                    <img
                      data-src={urls.tile(id, c.index, ti, info.version)}
                      width={c.width}
                      height={tileHeight(c, ti)}
                      style:aspect-ratio="{c.width} / {tileHeight(c, ti)}"
                      decoding="async"
                      alt=""
                      draggable="false"
                    />
                  {/each}
                </div>
              {:else}
              <img
                data-page={p.index}
                data-chunk={c.index}
                data-src={src(p.index, k)}
                width={c.width}
                height={c.height}
                style:aspect-ratio="{c.width} / {c.height}"
                decoding="async"
                alt=""
                draggable="false"
              />
              {/if}
            {/each}
          {/each}
          {#if win.to === m.pages.length - 1}<div class="strip-end">
            <p>{t('reader.end')}</p>
            <a class="button" href="/book/{id}">{t('reader.backToBook')}</a>
          </div>{/if}
        </div>
      </div>
    {:else if group}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div
        class="paged"
        class:fit-width={prefs.fit === 'width'}
        bind:this={pagedEl}
        onclick={onPagedClick}
        ontouchstart={onTouchStart}
        ontouchend={onTouchEnd}
        style:--count={group.visual.length}
      >
        <div class="spread">
          {#each group.visual as p (p)}
            <div class="page">
              {#each m.pages[p].chunks as c, k (c.index)}
                <img src={src(p, k)} width={c.width} height={c.height} alt={t('reader.pageAlt', { n: p + 1 })} draggable="false" style:--parts={m.pages[p].chunks.length} />
              {/each}
            </div>
          {/each}
        </div>
      </div>
    {/if}

    <header class="bar top">
      <a class="button flat icon" href="/book/{id}" aria-label={t('back')} title={t('back')}><Icon name="back" /></a>
      <div class="title">
        <div class="name">{info.title ?? ''}</div>
        {#if chapterLabel}<div class="muted chapter">{chapterLabel}</div>{/if}
      </div>
      <div class="tools">
        {#if m.contents.structured}
          <button class="flat icon" class:on={panel === 'contents'} onclick={() => togglePanel('contents')} title={`${t('reader.contents')} (C)`}><Icon name="contents" /></button>
        {/if}
        <button class="flat icon" class:on={panel === 'bookmarks'} onclick={() => togglePanel('bookmarks')} title={`${t('reader.bookmarks')} (B)`}><Icon name="bookmark" /></button>
        {#if info.has_text}
          <button class="flat icon" class:on={panel === 'text'} onclick={() => togglePanel('text')} title={`${t('reader.text')} (T)`}><Icon name="text" /></button>
        {/if}
        {#if info.has_dark}
          <button class="flat icon" class:on={dark} onclick={() => updateSettings({ reader: { dark: !dark } })} title={`${t('reader.dark')} (D)`}><Icon name="dark" /></button>
        {/if}
        {#if hasSound}
          <button class="flat icon" class:on={sound} onclick={toggleSound} title={`${t('reader.sound')} (M)`}><Icon name={sound ? 'sound' : 'muted'} /></button>
        {/if}
        <button class="flat icon" class:on={panel === 'settings'} onclick={() => togglePanel('settings')} title={`${t('reader.settings')} (S)`}><Icon name="settings" /></button>
        <button class="flat icon" onclick={toggleFullscreen} title={`${t('reader.fullscreen')} (F)`}><Icon name="fullscreen" /></button>
      </div>
    </header>

    <footer class="bar bottom">
      {#if !strip && groups.length > 1}
        <input
          type="range"
          min="0"
          max={groups.length - 1}
          value={gi}
          style:direction={rtl ? 'rtl' : 'ltr'}
          oninput={(e) => (page = groups[Number(e.currentTarget.value)].pages[0])}
          aria-label={t('reader.position')}
        />
      {/if}
      <span class="label">{pageLabel}</span>
    </footer>

    {#if panel}
      <aside class="panel" class:end={panel === 'end'}>
        <div class="panel-head">
          <h2>{t(`reader.panel.${panel}`)}</h2>
          <button class="flat icon circular" onclick={() => (panel = null)} aria-label={t('close')}><Icon name="close" /></button>
        </div>
        <div class="panel-body">
          {#if panel === 'contents'}
            {#each m.contents.volumes as v (v.first_page)}
              {#if m.contents.has_volumes}<h3>{v.number ? t('book.volume', { n: v.number }) : t('book.noVolume')}{v.title ? ` — ${v.title}` : ''}</h3>{/if}
              <ul class="list">
                {#each v.chapters as c (c.first_page)}
                  <li>
                    <button class="ghost item" class:current={chapter?.c === c} onclick={() => goTo(c.first_page)}>
                      {t('book.chapter', { n: c.number })}{c.title ? ` — ${c.title}` : ''}
                    </button>
                  </li>
                {/each}
              </ul>
            {/each}
          {:else if panel === 'bookmarks'}
            <button class="primary" onclick={addBookmark}>+ {t('reader.addBookmark')}</button>
            <ul class="list">
              {#each bookmarks as b (b.id)}
                <li class="bookmark">
                  <button class="ghost item" onclick={() => openBookmark(b)}>{t('book.page', { n: b.page + 1 })}{b.note ? ` — ${b.note}` : ''}</button>
                  <button class="ghost" onclick={() => deleteBookmark(b)} aria-label={t('remove')}>✕</button>
                </li>
              {:else}
                <li class="muted">{t('reader.noBookmarks')}</li>
              {/each}
            </ul>
          {:else if panel === 'text'}
            {#each texts as tx (tx.page)}
              <h3>{t('book.page', { n: tx.page + 1 })}</h3>
              <p class="text">{tx.text ?? ''}</p>
            {:else}
              <p class="muted">{t('reader.noText')}</p>
            {/each}
          {:else if panel === 'settings'}
            <div class="boxed-list">
              <label class="list-row">
                <span class="grow">{t('reader.mode')}</span>
                <select value={prefs.mode} onchange={(e) => updateSettings({ reader: { mode: e.currentTarget.value } })}>
                  {#each ['auto', 'single', 'double', 'strip'] as v (v)}<option value={v}>{t(`reader.mode.${v}`)}</option>{/each}
                </select>
              </label>
              <label class="list-row">
                <span class="grow">{t('reader.direction')}</span>
                <select value={prefs.direction} onchange={(e) => updateSettings({ reader: { direction: e.currentTarget.value } })}>
                  {#each ['auto', 'ltr', 'rtl'] as v (v)}<option value={v}>{t(`reader.direction.${v}`)}</option>{/each}
                </select>
              </label>
              {#if !strip}
                <label class="list-row">
                  <span class="grow">{t('reader.fit')}</span>
                  <select value={prefs.fit} onchange={(e) => updateSettings({ reader: { fit: e.currentTarget.value } })}>
                    <option value="contain">{t('reader.fit.contain')}</option>
                    <option value="width">{t('reader.fit.width')}</option>
                  </select>
                </label>
                <label class="list-row">
                  <span class="grow">{t('reader.coverAlone')}</span>
                  <input type="checkbox" class="switch" checked={coverAlone} onchange={(e) => updateSettings({ reader: { coverAlone: e.currentTarget.checked } })} />
                </label>
              {:else}
                <label class="list-row column">
                  <span>{t('reader.stripWidth', { px: stripWidth })}</span>
                  <input
                    type="range"
                    min="360"
                    max="1600"
                    step="20"
                    value={stripWidth}
                    oninput={(e) => {
                      const v = Number(e.currentTarget.value);
                      keepPlace(() => (liveWidth = v));
                    }}
                    onchange={(e) => {
                      updateSettings({ reader: { stripWidth: Number(e.currentTarget.value) } });
                      liveWidth = null;
                    }}
                  />
                </label>
              {/if}
              {#if info.has_dark}
                <label class="list-row">
                  <span class="grow">{t('reader.dark')}</span>
                  <input type="checkbox" class="switch" checked={dark} onchange={(e) => updateSettings({ reader: { dark: e.currentTarget.checked } })} />
                </label>
              {/if}
              {#if hasSound}
                <label class="list-row">
                  <span class="grow">{t('reader.sound')}</span>
                  <input type="checkbox" class="switch" checked={sound} onchange={toggleSound} />
                </label>
                <label class="list-row column">
                  <span>{t('reader.volume', { p: Math.round(volume * 100) })}</span>
                  <input
                    type="range"
                    min="0"
                    max="1"
                    step="0.05"
                    value={volume}
                    oninput={(e) => (liveVolume = Number(e.currentTarget.value))}
                    onchange={(e) => {
                      updateSettings({ reader: { volume: Number(e.currentTarget.value) } });
                      liveVolume = null;
                    }}
                  />
                </label>
              {/if}
            </div>
            <p class="dim keys">{t('reader.keys')}</p>
          {:else if panel === 'end'}
            <p>{t('reader.end')}</p>
            <a class="button primary" href="/book/{id}">{t('reader.backToBook')}</a>
            <a class="button" href="/">{t('nav.library')}</a>
          {/if}
        </div>
      </aside>
    {/if}
  {/if}
</div>

<style>
  .reader {
    position: fixed;
    inset: 0;
    background: #000;
    color: #eee;
    overflow: hidden;
    /* Like GNOME's OSD controls over video and images. */
    --bar-bg: rgb(28 28 32 / 0.88);
  }
  .center {
    height: 100%;
  }

  /* Paged */
  .paged {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
    user-select: none;
    touch-action: pan-y pinch-zoom;
  }
  .spread {
    display: flex;
    align-items: center;
    max-width: 100%;
    max-height: 100%;
  }
  .page {
    display: flex;
    flex-direction: column;
  }
  .paged img {
    display: block;
    width: auto;
    height: auto;
    max-width: calc(100vw / var(--count));
    max-height: calc(100dvh / var(--parts, 1));
    object-fit: contain;
  }
  .paged.fit-width {
    align-items: flex-start;
    overflow-y: auto;
  }
  .paged.fit-width img {
    width: calc(100vw / var(--count));
    max-width: none;
    max-height: none;
  }

  /* Strip */
  .strip {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    outline: none;
    overscroll-behavior: contain;
  }
  .strip-inner {
    position: relative;
    margin: 0 auto;
    max-width: var(--strip-width, 800px);
  }
  .tiled {
    display: flow-root;
  }
  .strip img {
    display: block;
    width: 100%;
    height: auto;
    user-select: none;
    /* Until it loads: the page's place, kept by its aspect ratio. */
    background: #111114;
  }
  .chapter-sep,
  .strip-end {
    padding: 2rem 1rem;
    text-align: center;
    color: #bbb;
    font-weight: 600;
  }
  .strip-end {
    padding-bottom: 6rem;
  }

  /* Bars */
  .bar {
    position: absolute;
    left: 0;
    right: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.4rem max(0.6rem, env(safe-area-inset-left));
    background: var(--bar-bg);
    transition:
      transform 0.2s,
      opacity 0.2s;
  }
  .top {
    top: 0;
    padding-top: max(0.4rem, env(safe-area-inset-top));
  }
  .bottom {
    bottom: 0;
    padding-bottom: max(0.4rem, env(safe-area-inset-bottom));
  }
  .ui-hidden .top {
    transform: translateY(-100%);
    opacity: 0;
    pointer-events: none;
  }
  .ui-hidden .bottom {
    transform: translateY(100%);
    opacity: 0;
    pointer-events: none;
  }
  .bar {
    backdrop-filter: blur(12px);
  }
  .bar :global(.button),
  .bar button {
    color: #fff;
  }
  .bar button:hover,
  .bar :global(.button:hover) {
    background: rgb(255 255 255 / 0.12);
  }
  .bar button.on {
    background: var(--accent-bg);
    color: var(--accent-fg);
  }
  .bottom input[type='range'] {
    accent-color: var(--accent-bg);
  }
  .title {
    flex: 1;
    min-width: 0;
  }
  .name,
  .chapter {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .name {
    font-weight: 600;
  }
  .chapter {
    font-size: 0.85rem;
    color: #aaa;
  }
  .tools {
    display: flex;
    gap: 0.1rem;
  }
  .tools button {
    min-width: 2.3rem;
    padding: 0.35rem 0.5rem;
  }
  .bottom input {
    flex: 1;
  }
  .label {
    margin-left: auto;
    font-size: 0.9rem;
    color: #ccc;
    white-space: nowrap;
  }

  /* Side panel */
  .panel {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    z-index: 6;
    width: min(24rem, 100vw);
    display: flex;
    flex-direction: column;
    background: var(--window-bg);
    color: var(--fg);
    box-shadow: var(--popover-shadow);
  }
  .panel.end {
    top: auto;
    bottom: 50%;
    right: 50%;
    transform: translate(50%, 50%);
    border-radius: calc(var(--radius) * 1.5);
    height: auto;
  }
  .panel-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.7rem 1rem;
    padding-top: max(0.7rem, env(safe-area-inset-top));
    border-bottom: 1px solid var(--border);
  }
  .panel-head h2 {
    margin: 0;
  }
  .panel-body {
    padding: 1rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .item {
    width: 100%;
    justify-content: flex-start;
    white-space: normal;
    text-align: left;
  }
  .item.current {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .item {
    font-weight: normal;
  }
  .list-row.column {
    flex-direction: column;
    align-items: stretch;
    gap: 6px;
  }
  .list-row.column input[type='range'] {
    width: 100%;
  }
  .panel :global(.list-row select) {
    max-width: 60%;
  }
  .bookmark {
    display: flex;
  }
  .text {
    white-space: pre-line;
    margin: 0;
  }
  .keys {
    font-size: 0.8rem;
  }
  h3 {
    margin: 0.4rem 0 0;
  }
</style>
