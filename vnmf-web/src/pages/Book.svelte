<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { onMount } from 'svelte';
  import { api, urls } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { navigate } from '../lib/router.svelte.js';
  import { session } from '../lib/session.svelte.js';
  import { toast, toastError } from '../lib/toast.svelte.js';
  import { size, date, number } from '../lib/format.js';
  import Dialog from '../components/Dialog.svelte';
  import MetadataEditor from '../components/MetadataEditor.svelte';
  import Icon from '../components/Icon.svelte';
  import Menu from '../components/Menu.svelte';
  import Carousel from '../components/Carousel.svelte';
  import ListMenu from '../components/ListMenu.svelte';

  let { id } = $props();

  let book = $state(null);
  let contents = $state(null);
  let metadata = $state(null);
  let bookmarks = $state([]);
  let covers = $state([]);
  let viewing = $state(null); // the index of the cover shown in the carousel
  let error = $state('');
  let busy = $state('');
  let editing = $state(false);
  let health = $state(null);
  let repairLog = $state(null);
  let redundancy = $state(0);
  let coverInput = $state();

  const admin = $derived(session.user?.is_admin && book?.writable);
  const info = $derived(book?.info);
  const meta = $derived(metadata?.metadata);

  async function load() {
    try {
      book = await api(`/books/${id}`);
      redundancy = book.redundancy;
      if (!book.missing) {
        [contents, metadata, bookmarks, covers] = await Promise.all([
          api(`/books/${id}/contents`),
          api(`/books/${id}/metadata`),
          api(`/books/${id}/bookmarks`).then((r) => r.items),
          api(`/books/${id}/covers`).then((r) => r.covers),
        ]);
      }
    } catch (e) {
      error = e.message;
    }
  }

  onMount(load);

  /** Runs a changing action with a busy marker; `then` gets the result. */
  async function run(name, action, then) {
    busy = name;
    try {
      const res = await action();
      await then?.(res);
    } catch (e) {
      toastError(e);
    } finally {
      busy = '';
    }
  }

  const credits = $derived.by(() => {
    const out = new Map();
    for (const c of meta?.credits ?? []) {
      for (const r of c.roles.length ? c.roles : ['author']) {
        if (!out.has(r)) out.set(r, []);
        out.get(r).push(c.name);
      }
    }
    return [...out];
  });

  // The covers for the carousel; the book's own first, then the volumes'.
  const coverItems = $derived(
    covers.map((c) => ({
      src: urls.coverAt(id, c.index, book?.version),
      caption: c.volume ? t('book.volumeCover', { n: c.volume }) : t('book.bookCover'),
    })),
  );

  const readUrl = (page) => `/book/${id}/read${page != null ? `?page=${page}` : ''}`;
  const webtoon = $derived(!!(book?.webtoon || info?.webtoon));
  // Read to the end: "read again" instead of "continue". The last page reached counts too — but not
  // in a webtoon, where a page is a whole chapter (reaching it is only starting it).
  const done = $derived(!!book?.progress && (book.progress.finished || (!webtoon && book.progress.furthest >= book.page_count - 1)));
  const continuePage = $derived(book?.progress && !done ? book.progress.page : null);
  /** A chapter is read when its last page was reached (in a webtoon: when the next one was). */
  const chapterRead = (c) => !!book.progress && (book.progress.finished || book.progress.furthest >= c.first_page + c.page_count - (webtoon ? 0 : 1));
  /** The chapter a page is in (for webtoons, where a chapter is one long page). */
  function chapterOf(page) {
    for (const v of contents?.volumes ?? []) for (const c of v.chapters) if (page >= c.first_page && page < c.first_page + c.page_count) return c;
    return null;
  }
  const continueLabel = $derived.by(() => {
    if (continuePage == null) return '';
    const c = webtoon ? chapterOf(continuePage) : null;
    return c ? t('book.continueChapter', { n: c.number }) : t('book.continue', { page: continuePage + 1 });
  });
  let listKey = $state(0);
  // The translation (or, if it doesn't say, the series) is still coming out: finishing is catching up.
  const comingOut = $derived(['ongoing', 'hiatus', 'upcoming'].includes(meta?.translation?.status ?? meta?.status));

  /** Read again: every chapter unread, from the start. */
  function readAgain() {
    run('progress', () => api(`/books/${id}/progress`, { method: 'DELETE' }), () => navigate(readUrl(0)));
  }

  function saveMetadata(m) {
    run('meta', () => api(`/books/${id}/metadata`, { method: 'PUT', json: m }), async () => {
      editing = false;
      toast(t('book.saved'), 'success');
      await load();
    });
  }

  function uploadCover(e) {
    const file = e.target.files?.[0];
    e.target.value = '';
    if (!file) return;
    run('cover', () => api(`/books/${id}/covers`, { method: 'POST', body: file }), load);
  }

  function protect() {
    run('protect', () => api(`/books/${id}/protect`, { method: 'POST', json: { redundancy: Number(redundancy) } }), async () => {
      toast(t('book.protected'), 'success');
      await load();
    });
  }

  function check() {
    run('check', () => api(`/books/${id}/health?full=true`), async (h) => {
      health = h;
      await load();
    });
  }

  function repair() {
    run('repair', () => api(`/books/${id}/repair`, { method: 'POST' }), async (r) => {
      repairLog = r;
      health = null;
      toast(t(`repair.${r.outcome}`), r.outcome === 'partial' ? 'error' : 'success');
      await load();
    });
  }

  function remove() {
    if (!confirm(t('book.deleteConfirm', { title: book.title }))) return;
    run('delete', () => api(`/books/${id}`, { method: 'DELETE' }), () => navigate('/'));
  }

  function setProgress(finished) {
    const action = () =>
      finished
        ? api(`/books/${id}/progress`, { method: 'PUT', json: { page: Math.max(0, book.page_count - 1), finished: true } })
        : api(`/books/${id}/progress`, { method: 'DELETE' });
    // The lists follow (reading → completed, unread → out of them): the list button reloads too.
    run('progress', action, async () => {
      await load();
      listKey++;
    });
  }

  /** Deletes the identical file `drop` of this book and keeps `keep` (one of them is this page's book). */
  function removeCopy(keep, drop, question) {
    if (!confirm(question)) return;
    run(
      'copy',
      () =>
        api(`/books/${keep}/copies/${drop}`, { method: 'DELETE' }).catch((e) => {
          throw e.code === 'different_copy' ? new Error(t('copies.different')) : e;
        }),
      async () => {
        toast(t('copies.deleted'), 'success');
        if (drop === Number(id)) navigate(`/book/${keep}`);
        else await load();
      },
    );
  }

  function deleteBookmark(b) {
    run('bookmark', () => api(`/bookmarks/${b.id}`, { method: 'DELETE' }), () => (bookmarks = bookmarks.filter((x) => x.id !== b.id)));
  }
</script>

{#if error}
  <div class="center"><p class="error-text">{error}</p><a href="/">{t('nav.library')}</a></div>
{:else if !book}
  <div class="center muted">{t('loading')}</div>
{:else}
  <div class="layout">
    <aside>
      {#if covers.length}
        <button class="cover-button" onclick={() => (viewing = 0)} title={t('book.showCovers')} aria-label={t('book.showCovers')}>
          <img class="cover" src={book.cover_url} alt="" />
          <span class="zoom" aria-hidden="true">
            <Icon name={covers.length > 1 ? 'images' : 'search'} />
            {#if covers.length > 1}{t('book.coversCount', { n: covers.length })}{/if}
          </span>
        </button>
      {:else}
        <img class="cover" src={book.cover_url} alt="" />
      {/if}
      {#if !book.missing}
        <div class="actions">
          {#if done}
            <button class="primary pill" onclick={readAgain} disabled={!!busy}><Icon name="refresh" />{t('book.readAgain')}</button>
          {:else if continuePage != null}
            <a class="button primary pill" href={readUrl(continuePage)}><Icon name="play" />{continueLabel}</a>
            <a class="button pill" href={readUrl(0)}>{t('book.fromStart')}</a>
          {:else}
            <a class="button primary pill" href={readUrl(0)}><Icon name="play" />{t('book.read')}</a>
          {/if}
          {#if done && comingOut}<p class="caught-up dim">{t('book.caughtUp')}</p>{/if}
          {#key listKey}<ListMenu book={id} onchange={load} />{/key}
          <div class="row split">
            <Menu label={t('book.download')} align="start" flat={false}>
              {#snippet button()}<Icon name="download" />{t('book.download')}{/snippet}
              <a href={urls.file(id)} download>.vnmf</a>
              <a href={urls.export(id, 'cbz')} download>.cbz — {t('book.cbzHint')}</a>
              <a href={urls.export(id, 'zip')} download>.zip — {t('book.zipHint')}</a>
              {#if info?.has_metadata}<a href={urls.comicinfo(id)} download="ComicInfo.xml">ComicInfo.xml</a>{/if}
            </Menu>
            {#if done}
              <button onclick={() => setProgress(false)} disabled={!!busy}>{t('book.markUnread')}</button>
            {:else}
              <button onclick={() => setProgress(true)} disabled={!!busy}><Icon name="check" />{t('book.markRead')}</button>
            {/if}
          </div>
        </div>
      {/if}
    </aside>

    <div class="main">
      <h1>{book.title}</h1>
      {#if meta?.original_title || meta?.romanized_title}
        <p class="dim alt">{[meta.original_title, meta.romanized_title].filter(Boolean).join(' · ')}</p>
      {/if}
      {#if book.missing}<p class="error-text">{t('book.missing')}</p>{/if}

      {#if credits.length}
        <p class="credits">
          {#each credits as [role, names] (role)}
            <span><span class="dim">{t(`role.${role}`)}:</span> {names.join(', ')}</span>
          {/each}
        </p>
      {/if}

      <div class="row chips">
        {#if meta?.type}<span class="chip accent">{t(`type.${meta.type}`)}</span>{/if}
        {#if meta?.status}<span class="chip accent">{t(`status.${meta.status}`)}</span>{/if}
        {#if meta?.translation?.status}<span class="chip accent">{t('book.translation', { status: t(`status.${meta.translation.status}`).toLowerCase() })}</span>{/if}
        {#each meta?.genres ?? [] as g (g)}<a class="chip" href="/?genre={encodeURIComponent(g)}">{g}</a>{/each}
        {#each meta?.tags ?? [] as tag (tag)}<a class="chip" href="/?tag={encodeURIComponent(tag)}">#{tag}</a>{/each}
      </div>

      {#if meta?.summary}<p class="summary">{meta.summary}</p>{/if}

      {#if contents?.structured}
        <h2 class="heading">{t('book.contents')}</h2>
        {#each contents.volumes as v (v.first_page)}
          {#if contents.has_volumes}
            <h3 class="volume">{v.number ? t('book.volume', { n: v.number }) : t('book.noVolume')}{v.title ? ` — ${v.title}` : ''}</h3>
          {/if}
          <div class="boxed-list">
            {#each v.chapters as c (c.first_page)}
              {@const current = book.progress && !book.progress.finished && book.progress.page >= c.first_page && book.progress.page < c.first_page + c.page_count}
              {@const read = chapterRead(c)}
              <a class="list-row chapter" class:read href={readUrl(c.first_page)}>
                <span class="grow">
                  <span class="title">{t('book.chapter', { n: c.number })}{c.title ? ` — ${c.title}` : ''}</span>
                  {#if c.date}<span class="subtitle">{c.date}</span>{/if}
                </span>
                {#if current}<span class="chip accent">{t('book.here')}</span>{:else if read}<span class="chip read-mark"><Icon name="check" />{t('book.chapterRead')}</span>{/if}
                {#if !webtoon}<span class="dim">{t('book.pages', { n: c.page_count })}</span>{/if}
                <Icon name="forward" />
              </a>
            {/each}
          </div>
        {/each}
      {/if}

      {#if book.copies?.length}
        <h2 class="heading">{t('copies.title')}</h2>
        <p class="dim hint">{t('copies.hint')}</p>
        <div class="boxed-list">
          {#each book.copies as c (c.id)}
            <div class="list-row">
              <Icon name="images" size={20} />
              <a class="grow copy" href="/book/{c.id}">
                <span class="title mono">{c.path ?? `#${c.id}`}</span>
                <span class="subtitle">
                  {[size(c.size), c.managed && t('copies.uploaded'), c.missing && t('copies.missing'), !c.missing && !c.same_size && t('copies.otherVersion')]
                    .filter(Boolean)
                    .join(' · ')}
                </span>
              </a>
              {#if session.user?.is_admin && (c.missing || c.same_size) && !book.missing}
                {#if c.missing || c.writable}
                  <button class="danger" disabled={!!busy} onclick={() => removeCopy(Number(id), c.id, t('copies.deleteConfirm', { path: c.path ?? `#${c.id}` }))}>
                    {c.missing ? t('copies.forget') : t('copies.delete')}
                  </button>
                {:else if book.writable && !c.missing}
                  <button disabled={!!busy} title={t('copies.keepThatHint')} onclick={() => removeCopy(c.id, Number(id), t('copies.keepConfirm', { path: book.title }))}>
                    {t('copies.keepThat')}
                  </button>
                {/if}
              {/if}
            </div>
          {/each}
        </div>
      {/if}

      {#if bookmarks.length}
        <h2 class="heading">{t('book.bookmarks')}</h2>
        <div class="boxed-list">
          {#each bookmarks as b (b.id)}
            <div class="list-row">
              <Icon name="bookmark" />
              <a class="grow" href={readUrl(b.page)}>
                <span class="title">{t('book.page', { n: b.page + 1 })}</span>
                {#if b.note}<span class="subtitle">{b.note}</span>{/if}
              </a>
              <button class="flat icon circular" onclick={() => deleteBookmark(b)} aria-label={t('remove')} title={t('remove')}><Icon name="trash" /></button>
            </div>
          {/each}
        </div>
      {/if}

      {#if info}
        <h2 class="heading">{t('book.file')}</h2>
        <div class="boxed-list">
          <div class="list-row">
            <span class="grow">{t('book.pagesCount')}</span>
            <span class="dim">{number(info.page_count)}{info.webtoon ? ` · ${t('badge.webtoon')}` : ''}{info.rtl ? ` · ${t('book.rtl')}` : ''}</span>
          </div>
          {#if info.volume_count || info.chapter_count > 1}
            <div class="list-row">
              <span class="grow">{t('book.structure')}</span>
              <span class="dim">{t('book.structureValue', { v: info.volume_count, c: info.chapter_count })}</span>
            </div>
          {/if}
          <div class="list-row">
            <span class="grow">{t('book.extras')}</span>
            <span class="dim">
              {[info.has_music && t('book.music'), info.has_sfx && t('book.sfx'), info.has_text && t('book.text'), info.has_dark && t('book.dark')]
                .filter(Boolean)
                .join(', ') || '—'}
            </span>
          </div>
          <div class="list-row">
            <span class="grow">{t('book.size')}</span>
            <span class="dim">{size(info.file_size)} · VNMF {info.format_version}{info.legacy ? ` (${t('book.legacy')})` : ''}</span>
          </div>
          <div class="list-row">
            <span class="grow">{t('book.added')}</span>
            <span class="dim">{date(book.added_at)}</span>
          </div>
          <div class="list-row">
            <span class="grow">UUID</span>
            <span class="dim mono">{info.uuid}</span>
          </div>
        </div>

        <h2 class="heading">{t('book.safety')}</h2>
        <div class="boxed-list">
          <div class="list-row">
            <Icon name="shield" size={20} />
            <span class="grow">
              <span class="title">{t('book.protection')}</span>
              <span class="subtitle">
                {#if info.parity}
                  {t('book.redundancy', { p: info.redundancy, burst: size(info.parity.max_burst_bytes) })}
                {:else}
                  {t('book.noRedundancy')}
                {/if}
                {#if info.tail_copy}· {t('book.tailCopy')}{/if}
              </span>
            </span>
          </div>
          <div class="list-row">
            <Icon name={book.health?.status === 'damaged' ? 'warning' : 'check'} size={20} />
            <span class="grow">
              <span class="title">{t('book.health')}</span>
              <span class="subtitle">
                {#if !book.health}
                  {t('book.notChecked')}
                {:else if book.health.status === 'ok'}
                  <span class="success-text">{book.health.full ? t('book.intactFull') : t('book.intactQuick')}</span>
                {:else}
                  <span class="error-text">{t('book.damaged')}</span>
                {/if}
                {#if book.health?.checked_at}· {date(book.health.checked_at)}{/if}
              </span>
            </span>
            <button onclick={check} disabled={!!busy}><Icon name="refresh" />{busy === 'check' ? t('book.checking') : t('book.checkFull')}</button>
          </div>
          {#if book.health?.status === 'damaged'}
            <div class="list-row details">
              <ul class="issues">{#each book.health.issues as issue, i (i)}<li>{issue}</li>{/each}</ul>
              {#if health && !health.ok && health.prognosis}<p class="dim">{health.prognosis}</p>{/if}
              {#if admin}
                <button class="primary" onclick={repair} disabled={!!busy}>{busy === 'repair' ? t('book.repairing') : t('book.repair')}</button>
              {/if}
            </div>
          {/if}
          {#if repairLog}
            <div class="list-row details">
              <details open><summary>{t(`repair.${repairLog.outcome}`)}</summary><pre>{repairLog.log.join('\n')}</pre></details>
            </div>
          {/if}
        </div>
      {/if}

      {#if session.user?.is_admin}
        <h2 class="heading">{t('book.manage')}</h2>
        {#if !book.writable && !book.missing}
          <p class="dim">{t('book.readOnly')}</p>
        {:else}
          <div class="boxed-list">
            {#if !book.missing}
              <button class="list-row" onclick={() => (editing = true)} disabled={!!busy}>
                <Icon name="edit" size={20} />
                <span class="grow"><span class="title">{t('book.editMetadata')}</span><span class="subtitle">{t('book.editMetadataHint')}</span></span>
                <Icon name="forward" />
              </button>
              <button class="list-row" onclick={() => coverInput.click()} disabled={!!busy}>
                <Icon name="image" size={20} />
                <span class="grow"><span class="title">{busy === 'cover' ? t('saving') : t('book.changeCover')}</span><span class="subtitle">{t('book.changeCoverHint')}</span></span>
                <Icon name="forward" />
              </button>
              <input bind:this={coverInput} type="file" accept="image/*" class="visually-hidden" onchange={uploadCover} />
              <div class="list-row">
                <Icon name="shield" size={20} />
                <label class="grow" for="redundancy">
                  <span class="title">{t('book.redundancyLabel')}</span>
                  <span class="subtitle">{t('book.redundancyHint')}</span>
                </label>
                <select id="redundancy" bind:value={redundancy}>
                  {#each [0, 5, 10, 15, 20, 25, 30] as p (p)}<option value={p}>{p ? `${p}%` : t('book.off')}</option>{/each}
                </select>
                <button onclick={protect} disabled={!!busy || Number(redundancy) === book.redundancy}>{busy === 'protect' ? t('saving') : t('apply')}</button>
              </div>
            {/if}
            <div class="list-row">
              <Icon name="trash" size={20} />
              <span class="grow"><span class="title">{t('book.delete')}</span><span class="subtitle">{t('book.deleteHint')}</span></span>
              <button class="danger filled" onclick={remove} disabled={!!busy}>{t('remove')}</button>
            </div>
          </div>
        {/if}
      {/if}
    </div>
  </div>

  {#if viewing != null}
    <Carousel items={coverItems} start={viewing} onclose={() => (viewing = null)} />
  {/if}

  <Dialog open={editing} title={t('book.editMetadata')} wide onclose={() => (editing = false)}>
    <MetadataEditor metadata={meta} busy={busy === 'meta'} onsave={saveMetadata} oncancel={() => (editing = false)} />
  </Dialog>
{/if}

<style>
  .layout {
    display: grid;
    grid-template-columns: 15rem minmax(0, 1fr);
    gap: 32px;
    align-items: start;
    max-width: 68rem;
    margin: 0 auto;
  }
  aside {
    position: sticky;
    top: 64px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .cover {
    width: 100%;
    aspect-ratio: 2 / 3;
    object-fit: cover;
    border-radius: 10px;
    box-shadow: var(--card-shadow);
    background: var(--button-bg);
  }
  /* The poster opens the covers: a pill with their number, and a lift on hover. */
  .cover-button {
    position: relative;
    display: block;
    padding: 0;
    min-height: 0;
    border-radius: 10px;
    background: none;
    cursor: zoom-in;
  }
  .cover-button:hover {
    background: none;
  }
  .cover-button .cover {
    display: block;
    transition:
      transform 0.15s,
      box-shadow 0.15s;
  }
  .cover-button:hover .cover {
    transform: translateY(-2px);
    box-shadow: var(--popover-shadow);
  }
  .zoom {
    position: absolute;
    right: 8px;
    bottom: 8px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 10px;
    border-radius: 9999px;
    background: rgb(0 0 0 / 0.6);
    color: #fff;
    font-size: 0.8rem;
    font-weight: 700;
    backdrop-filter: blur(6px);
    pointer-events: none;
  }
  .actions {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .split > :global(*) {
    flex: 1;
  }
  /* All buttons of the column are pills, like the main one. */
  .actions :global(button),
  .actions :global(.button) {
    border-radius: 9999px;
    min-height: 40px;
    /* Long labels (Ukrainian ones are) wrap instead of overflowing the column. */
    white-space: normal;
    text-align: center;
    line-height: 1.2;
    max-width: 100%;
  }
  .split {
    flex-wrap: wrap;
  }
  .caught-up {
    margin: 0 4px;
    font-size: 0.82rem;
    line-height: 1.35;
  }
  .split :global(.menu > button) {
    width: 100%;
  }
  .chapter.read .title {
    color: var(--dim);
  }
  /* Read chapters are darker (rows are transparent over the list's background). */
  .chapter.read {
    background: rgb(0 0 0 / 0.14);
  }
  .chapter.read:hover {
    background: var(--hover);
  }
  .read-mark {
    font-size: 0.75rem;
    padding: 0 8px;
    gap: 4px;
  }
  .read-mark :global(.icon) {
    width: 14px;
    height: 14px;
  }
  .copy {
    min-width: 0;
    color: inherit;
  }
  .copy .title {
    overflow-wrap: anywhere;
    font-size: 0.85rem;
  }
  .hint {
    margin: -4px 0 10px;
    font-size: 0.88rem;
  }
  .alt {
    margin-top: -6px;
  }
  .credits {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 18px;
  }
  .chips {
    margin: 8px 0 14px;
  }
  .summary {
    white-space: pre-line;
    max-width: 70ch;
  }
  .volume {
    margin: 14px 0 8px;
    color: var(--dim);
  }
  .list-row .grow {
    color: inherit;
  }
  a.list-row,
  a.grow {
    text-decoration: none;
  }
  .details {
    flex-direction: column;
    align-items: flex-start;
  }
  .issues {
    margin: 0;
    padding-left: 1.2rem;
  }
  .mono {
    font-family: ui-monospace, monospace;
    font-size: 0.82rem;
    word-break: break-all;
  }
  pre {
    white-space: pre-wrap;
    font-size: 0.8rem;
    background: var(--entry-bg);
    padding: 10px;
    border-radius: var(--radius);
  }
  @media (max-width: 760px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
      gap: 20px;
    }
    aside {
      position: static;
      flex-direction: row;
      align-items: flex-start;
    }
    .cover {
      width: 8.5rem;
    }
    .actions {
      flex: 1;
      min-width: 0;
    }
  }
</style>
