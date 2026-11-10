<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { upload } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { session } from '../lib/session.svelte.js';
  import { size } from '../lib/format.js';
  import Icon from '../components/Icon.svelte';

  let files = $state([]);
  let dragging = $state(false);
  let progress = $state(null);
  let result = $state(null);
  let error = $state('');
  let input = $state();

  let combine = $state(false);
  let webtoon = $state(false);
  let mixedFormats = $state(false);
  let rtl = $state(false);
  let redundancy = $state(10);
  let volume = $state('');
  let cover = $state('first');
  let replace = $state(false);

  const total = $derived(files.reduce((s, f) => s + f.size, 0));
  const tooBig = $derived(session.server && total > session.server.max_upload);
  const hasArchives = $derived(combine || files.some((f) => !/\.vnmf$/i.test(f.name)));

  function add(list) {
    const known = new Set(files.map((f) => `${f.name}/${f.size}`));
    files = [...files, ...[...list].filter((f) => !known.has(`${f.name}/${f.size}`))].sort((a, b) =>
      a.name.localeCompare(b.name, undefined, { numeric: true }),
    );
    result = null;
  }

  function drop(e) {
    e.preventDefault();
    dragging = false;
    add(e.dataTransfer.files);
  }

  async function send() {
    error = '';
    result = null;
    const form = new FormData();
    for (const f of files) form.append('file', f, f.name);
    const q = new URLSearchParams({ redundancy, cover });
    if (combine) q.set('combine', 'true');
    if (webtoon) q.set('webtoon', 'true');
    if (mixedFormats) q.set('mixed_formats', 'true');
    if (rtl) q.set('rtl', 'true');
    if (replace) q.set('replace', 'true');
    if (volume !== '') q.set('volume', volume);
    progress = 0;
    try {
      result = await upload(`/books?${q}`, form, { onProgress: (p) => (progress = p) });
      files = [];
    } catch (e) {
      error = e.message;
    } finally {
      progress = null;
    }
  }
</script>

<h1>{t('upload.title')}</h1>

<div
  class="drop"
  class:dragging
  role="button"
  tabindex="0"
  ondragover={(e) => {
    e.preventDefault();
    dragging = true;
  }}
  ondragleave={() => (dragging = false)}
  ondrop={drop}
  onclick={() => input.click()}
  onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && input.click()}
>
  <Icon name="upload" size={40} />
  <p><strong>{t('upload.drop')}</strong></p>
  <p class="muted">{t('upload.formats')}</p>
  <input bind:this={input} type="file" multiple class="visually-hidden" onchange={(e) => add(e.currentTarget.files)} />
</div>

{#if files.length}
  <ul class="files">
    {#each files as f, i (f.name + f.size)}
      <li>
        <span class="name">{f.name}</span>
        <span class="muted">{size(f.size)}</span>
        <button class="flat icon circular" onclick={() => files.splice(i, 1)} aria-label={t('remove')} disabled={progress != null}><Icon name="close" /></button>
      </li>
    {/each}
  </ul>
  <p class="muted">{t('upload.total', { n: files.length, size: size(total) })}</p>
  {#if tooBig}<p class="error-text">{t('upload.tooBig', { size: size(session.server.max_upload) })}</p>{/if}

  <h2 class="heading">{t('upload.options')}</h2>
  <div class="boxed-list">
    <label class="list-row">
      <span class="grow">{t('upload.combine')}</span>
      <input type="checkbox" class="switch" bind:checked={combine} />
    </label>
    <label class="list-row">
      <span class="grow">{t('upload.replace')}</span>
      <input type="checkbox" class="switch" bind:checked={replace} />
    </label>
  </div>
  {#if hasArchives}
    <h2 class="heading">{t('upload.conversion')}</h2>
    <div class="boxed-list">
      <label class="list-row">
        <span class="grow">{t('upload.webtoon')}</span>
        <input type="checkbox" class="switch" bind:checked={webtoon} />
      </label>
      {#if webtoon}
        <label class="list-row">
          <span class="grow">{t('upload.mixedFormats')}</span>
          <input type="checkbox" class="switch" bind:checked={mixedFormats} />
        </label>
      {/if}
      <label class="list-row">
        <span class="grow">{t('upload.rtl')}</span>
        <input type="checkbox" class="switch" bind:checked={rtl} />
      </label>
      <label class="list-row">
        <span class="grow">{t('book.redundancyLabel')}<span class="subtitle">{t('book.redundancyHint')}</span></span>
        <select bind:value={redundancy}>
          {#each [0, 5, 10, 15, 20, 25, 30] as p (p)}<option value={p}>{p ? `${p}%` : t('book.off')}</option>{/each}
        </select>
      </label>
      <label class="list-row">
        <span class="grow">{t('upload.volume')}</span>
        <input type="number" min="0" max="65535" bind:value={volume} placeholder="—" class="narrow" />
      </label>
      <label class="list-row">
        <span class="grow">{t('upload.cover')}</span>
        <select bind:value={cover}>
          <option value="first">{t('upload.coverFirst')}</option>
          <option value="none">{t('upload.coverNone')}</option>
        </select>
      </label>
    </div>
  {/if}

  <div class="row send">
    <button class="primary pill" onclick={send} disabled={progress != null || tooBig}><Icon name="upload" />{t('upload.send')}</button>
    {#if progress != null}
      <progress max="1" value={progress}></progress>
      <span class="muted">{progress < 1 ? `${Math.round(progress * 100)}%` : t('upload.processing')}</span>
    {/if}
  </div>
{/if}

{#if error}<p class="error-text">{error}</p>{/if}

{#if result}
  <section class="card result">
    <h2>{t('upload.done', { n: result.books.length })}</h2>
    <ul>
      {#each result.books as b (b.id)}<li><a href="/book/{b.id}">{b.title}</a> <span class="muted">{t('book.pages', { n: b.page_count })}</span></li>{/each}
    </ul>
    {#if result.errors.length}
      <h3 class="error-text">{t('upload.errors')}</h3>
      <ul>{#each result.errors as e, i (i)}<li><strong>{e.file}</strong>: {e.message}</li>{/each}</ul>
    {/if}
    {#if result.warnings.length}
      <h3>{t('upload.warnings')}</h3>
      <ul class="muted">{#each result.warnings as w, i (i)}<li>{w}</li>{/each}</ul>
    {/if}
  </section>
{/if}

<style>
  .drop {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    border: 2px dashed var(--border);
    border-radius: var(--radius-card);
    padding: 40px 16px;
    text-align: center;
    cursor: pointer;
    color: var(--dim);
    transition:
      background 0.15s,
      border-color 0.15s;
  }
  .drop strong {
    color: var(--fg);
  }
  .drop.dragging,
  .drop:hover {
    border-color: var(--accent-bg);
    background: var(--accent-soft);
    color: var(--accent);
  }
  .drop p {
    margin: 0;
  }
  .files {
    list-style: none;
    padding: 0;
    margin: 16px 0 6px;
    border-radius: var(--radius-card);
    background: var(--card-bg);
    box-shadow: var(--card-shadow);
    max-height: 20rem;
    overflow-y: auto;
  }
  .files li {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 44px;
    padding: 4px 6px 4px 14px;
  }
  .files li + li {
    border-top: 1px solid var(--border);
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .narrow {
    width: 7rem;
  }
  .send {
    margin-top: 20px;
    justify-content: center;
  }
  .send progress {
    flex: 1;
    max-width: 20rem;
    accent-color: var(--accent-bg);
  }
  .result {
    margin-top: 24px;
  }
</style>
