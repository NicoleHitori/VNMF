<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // Editing vnmf-metadata/1: the common fields as a form, everything else as JSON. Fields the form
  // doesn't show (identifiers, links, the rest of translation, contents, unknown ones) are kept as they are.
  import { untrack } from 'svelte';
  import { t } from '../lib/i18n.svelte.js';
  import { TYPES, STATUSES, PUBLICATIONS, DEMOGRAPHICS, AGE_RATINGS, DIRECTIONS, LAYOUTS, COLORS, ROLES } from '../lib/vocab.js';

  let { metadata, onsave, oncancel, busy = false } = $props();

  // The editor works on a copy taken when it opens.
  const base = untrack(() => $state.snapshot(metadata)) ?? { schema: 'vnmf-metadata/1' };
  let tab = $state('form');
  let error = $state('');

  const TEXT = ['title', 'original_title', 'romanized_title', 'summary', 'language', 'original_language', 'origin_country', 'publisher',
    'imprint', 'serialization', 'original_publisher', 'edition', 'release_date', 'start_date', 'end_date', 'isbn'];
  const SELECT = { type: TYPES, status: STATUSES, publication: PUBLICATIONS, demographic: DEMOGRAPHICS, age_rating: AGE_RATINGS,
    reading_direction: DIRECTIONS, layout: LAYOUTS, color: COLORS };
  const LISTS = ['genres', 'tags', 'characters', 'content_warnings'];

  // The form state: strings for everything (lists comma-separated).
  let form = $state(fromMeta(base));
  let json = $state('');

  function fromMeta(m) {
    const f = {};
    for (const k of TEXT) f[k] = m[k] ?? '';
    for (const k of Object.keys(SELECT)) f[k] = m[k] ?? '';
    for (const k of LISTS) f[k] = (m[k] ?? []).join(', ');
    f.alternative_titles = (m.alternative_titles ?? []).map((a) => a.title).join('\n');
    f.translation_status = m.translation?.status ?? '';
    f.volume_count = m.volume_count ?? '';
    f.chapter_count = m.chapter_count ?? '';
    f.credits = (m.credits ?? []).map((c) => ({ name: c.name, roles: c.roles.join(', '), original_name: c.original_name ?? '' }));
    return f;
  }

  const split = (s) => s.split(/[,;]/).map((x) => x.trim()).filter(Boolean);

  function toMeta(f, into) {
    const m = { ...into, schema: 'vnmf-metadata/1' };
    for (const k of [...TEXT, ...Object.keys(SELECT)]) {
      const v = String(f[k] ?? '').trim();
      if (v) m[k] = v;
      else delete m[k];
    }
    for (const k of LISTS) {
      const v = split(f[k]);
      if (v.length) m[k] = v;
      else delete m[k];
    }
    // Keep the languages of alternative titles that are still there.
    const oldAlt = new Map((into.alternative_titles ?? []).map((a) => [a.title, a]));
    const alt = f.alternative_titles.split('\n').map((s) => s.trim()).filter(Boolean).map((title) => oldAlt.get(title) ?? { title });
    if (alt.length) m.alternative_titles = alt;
    else delete m.alternative_titles;
    // translation.status — the other translation fields as they were.
    const translation = { ...into.translation };
    if (f.translation_status) translation.status = f.translation_status;
    else delete translation.status;
    if (Object.keys(translation).length) m.translation = translation;
    else delete m.translation;
    for (const k of ['volume_count', 'chapter_count']) {
      const n = parseInt(f[k], 10);
      if (Number.isFinite(n) && n >= 0) m[k] = n;
      else delete m[k];
    }
    const credits = f.credits
      .filter((c) => c.name.trim())
      .map((c) => {
        const credit = { name: c.name.trim(), roles: split(c.roles) };
        if (c.original_name.trim()) credit.original_name = c.original_name.trim();
        return credit;
      });
    if (credits.length) m.credits = credits;
    else delete m.credits;
    return m;
  }

  function switchTab(next) {
    error = '';
    if (next === 'json' && tab === 'form') json = JSON.stringify(toMeta($state.snapshot(form), base), null, 2);
    if (next === 'form' && tab === 'json') {
      try {
        Object.assign(base, JSON.parse(json));
        form = fromMeta(base);
      } catch (e) {
        error = e.message;
        return;
      }
    }
    tab = next;
  }

  function save(e) {
    e.preventDefault();
    error = '';
    let m;
    try {
      m = tab === 'json' ? JSON.parse(json) : toMeta($state.snapshot(form), base);
    } catch (err) {
      error = err.message;
      return;
    }
    onsave(m);
  }
</script>

<form onsubmit={save}>
  <div class="tabs" role="tablist">
    <button type="button" role="tab" class:active={tab === 'form'} onclick={() => switchTab('form')}>{t('meta.form')}</button>
    <button type="button" role="tab" class:active={tab === 'json'} onclick={() => switchTab('json')}>JSON</button>
  </div>

  {#if tab === 'form'}
    <div class="grid">
      <label class="field wide">{t('meta.title')}<input bind:value={form.title} /></label>
      <label class="field">{t('meta.original_title')}<input bind:value={form.original_title} /></label>
      <label class="field">{t('meta.romanized_title')}<input bind:value={form.romanized_title} /></label>
      <label class="field wide">{t('meta.alternative_titles')}<textarea rows="2" bind:value={form.alternative_titles}></textarea></label>
      <label class="field wide">{t('meta.summary')}<textarea rows="5" bind:value={form.summary}></textarea></label>

      {#each Object.entries(SELECT) as [key, values] (key)}
        <label class="field">
          {t(`meta.${key}`)}
          <select bind:value={form[key]}>
            <option value="">—</option>
            {#each values as v (v)}<option value={v}>{t(`${key}.${v}`)}</option>{/each}
            {#if form[key] && !values.includes(form[key])}<option value={form[key]}>{form[key]}</option>{/if}
          </select>
        </label>
      {/each}
      <label class="field">
        {t('meta.translation_status')}
        <select bind:value={form.translation_status}>
          <option value="">—</option>
          {#each STATUSES as v (v)}<option value={v}>{t(`status.${v}`)}</option>{/each}
        </select>
      </label>

      <label class="field">{t('meta.genres')}<input bind:value={form.genres} placeholder="Fantasy, Comedy" /></label>
      <label class="field">{t('meta.tags')}<input bind:value={form.tags} /></label>
      <label class="field">{t('meta.characters')}<input bind:value={form.characters} /></label>
      <label class="field">{t('meta.content_warnings')}<input bind:value={form.content_warnings} /></label>

      <label class="field">{t('meta.language')}<input bind:value={form.language} placeholder="uk" /></label>
      <label class="field">{t('meta.original_language')}<input bind:value={form.original_language} placeholder="ja" /></label>
      <label class="field">{t('meta.origin_country')}<input bind:value={form.origin_country} placeholder="JP" /></label>
      <label class="field">{t('meta.publisher')}<input bind:value={form.publisher} /></label>
      <label class="field">{t('meta.imprint')}<input bind:value={form.imprint} /></label>
      <label class="field">{t('meta.serialization')}<input bind:value={form.serialization} /></label>
      <label class="field">{t('meta.original_publisher')}<input bind:value={form.original_publisher} /></label>
      <label class="field">{t('meta.edition')}<input bind:value={form.edition} /></label>
      <label class="field">{t('meta.isbn')}<input bind:value={form.isbn} /></label>
      <label class="field">{t('meta.start_date')}<input bind:value={form.start_date} placeholder="2020-01-31" /></label>
      <label class="field">{t('meta.end_date')}<input bind:value={form.end_date} /></label>
      <label class="field">{t('meta.release_date')}<input bind:value={form.release_date} /></label>
      <label class="field">{t('meta.volume_count')}<input type="number" min="0" bind:value={form.volume_count} /></label>
      <label class="field">{t('meta.chapter_count')}<input type="number" min="0" bind:value={form.chapter_count} /></label>
    </div>

    <h3>{t('meta.credits')}</h3>
    <datalist id="roles">
      {#each ROLES as r (r)}<option value={r}>{t(`role.${r}`)}</option>{/each}
    </datalist>
    <div class="credits">
      {#each form.credits as credit, i (i)}
        <div class="credit">
          <input bind:value={credit.name} placeholder={t('meta.name')} aria-label={t('meta.name')} />
          <input bind:value={credit.original_name} placeholder={t('meta.original_name')} aria-label={t('meta.original_name')} />
          <input bind:value={credit.roles} placeholder="story, art" list="roles" aria-label={t('meta.roles')} />
          <button type="button" class="ghost" onclick={() => form.credits.splice(i, 1)} aria-label={t('remove')}>✕</button>
        </div>
      {/each}
      <button type="button" onclick={() => form.credits.push({ name: '', roles: '', original_name: '' })}>+ {t('meta.addPerson')}</button>
      <p class="muted hint">{t('meta.rolesHint')}: {ROLES.join(', ')}</p>
    </div>
  {:else}
    <textarea class="json" rows="22" bind:value={json} spellcheck="false"></textarea>
  {/if}

  {#if error}<p class="error-text">{error}</p>{/if}
  <div class="actions">
    <button type="button" onclick={oncancel}>{t('cancel')}</button>
    <button class="primary" disabled={busy}>{busy ? t('saving') : t('save')}</button>
  </div>
</form>

<style>
  /* Linked toggle buttons, like a GtkStackSwitcher. */
  .tabs {
    display: flex;
    justify-content: center;
    margin-bottom: 16px;
  }
  .tabs button {
    border-radius: 0;
    min-width: 7rem;
  }
  .tabs button:first-child {
    border-radius: var(--radius) 0 0 var(--radius);
  }
  .tabs button:last-child {
    border-radius: 0 var(--radius) var(--radius) 0;
  }
  .tabs .active {
    background: var(--accent-bg);
    color: var(--accent-fg);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(14rem, 1fr));
    gap: 0.7rem;
  }
  .wide {
    grid-column: 1 / -1;
  }
  h3 {
    margin-top: 1.2rem;
  }
  .credits {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    align-items: flex-start;
  }
  .credit {
    display: grid;
    grid-template-columns: 2fr 2fr 2fr auto;
    gap: 0.4rem;
    width: 100%;
  }
  .hint {
    font-size: 0.8rem;
    margin: 0;
  }
  .json {
    width: 100%;
    font-family: ui-monospace, monospace;
    font-size: 0.85rem;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 1rem;
  }
  @media (max-width: 600px) {
    .credit {
      grid-template-columns: 1fr auto;
    }
  }
</style>
