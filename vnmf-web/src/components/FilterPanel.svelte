<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // Catalog filters: long lists (genres, tags, countries, languages) unfold with a search field,
  // ranges are "from – to", the dictionaries of the format are checkboxes with the number of books.
  import { t, i18n } from '../lib/i18n.svelte.js';
  import { TYPES, STATUSES, PUBLICATIONS, DEMOGRAPHICS, AGE_RATINGS, COLORS } from '../lib/vocab.js';
  import Icon from './Icon.svelte';

  let { filters = $bindable(), facets, onreset } = $props();

  let open = $state(null); // the unfolded list
  let search = $state('');

  const counts = (list) => new Map((list ?? []).map((f) => [f.value, f.count]));

  function regionName(code) {
    try {
      return new Intl.DisplayNames([i18n.lang], { type: 'region' }).of(code) ?? code;
    } catch {
      return code;
    }
  }

  function languageName(code) {
    try {
      return new Intl.DisplayNames([i18n.lang], { type: 'language' }).of(code) ?? code;
    } catch {
      return code;
    }
  }

  /** Lists that unfold: values come from the library itself. */
  const lists = $derived([
    { key: 'genre', label: t('meta.genres'), values: facets?.genres ?? [], name: (v) => v },
    { key: 'tag', label: t('meta.tags'), values: facets?.tags ?? [], name: (v) => `#${v}` },
    { key: 'country', label: t('filter.country'), values: facets?.countries ?? [], name: regionName },
    { key: 'language', label: t('meta.language'), values: facets?.languages ?? [], name: languageName },
  ]);

  /** Dictionaries of the format: every value is shown, with how many books have it. */
  const groups = $derived([
    { key: 'age', label: t('meta.age_rating'), values: AGE_RATINGS, prefix: 'age_rating', counts: counts(facets?.age_ratings) },
    { key: 'kind', label: t('meta.type'), values: TYPES, prefix: 'type', counts: counts(facets?.kinds) },
    { key: 'publication', label: t('filter.publication'), values: PUBLICATIONS, prefix: 'publication', counts: counts(facets?.publications) },
    { key: 'color', label: t('meta.color'), values: COLORS, prefix: 'color', counts: counts(facets?.colors) },
    { key: 'demographic', label: t('meta.demographic'), values: DEMOGRAPHICS, prefix: 'demographic', counts: counts(facets?.demographics) },
    { key: 'status', label: t('filter.status'), values: STATUSES, prefix: 'status', counts: counts(facets?.statuses) },
    { key: 'translation', label: t('filter.translation'), values: ['official', 'fan', 'machine'], prefix: 'translation', counts: counts(facets?.translations) },
  ]);

  const ranges = $derived([
    { key: 'chapters', label: t('filter.chapters'), range: facets?.ranges?.chapters },
    { key: 'pages', label: t('filter.pages'), range: facets?.ranges?.pages },
    { key: 'year', label: t('filter.year'), range: facets?.ranges?.year },
  ]);

  function toggle(key, value) {
    const list = filters[key];
    const i = list.indexOf(value);
    if (i >= 0) list.splice(i, 1);
    else list.push(value);
  }

  function summary(list) {
    const chosen = filters[list.key];
    if (!chosen.length) return t('filter.any');
    return chosen.map(list.name).join(', ');
  }

  function unfold(key) {
    open = open === key ? null : key;
    search = '';
  }

  const shown = (list) => {
    const s = search.trim().toLowerCase();
    return s ? list.values.filter((v) => list.name(v.value).toLowerCase().includes(s)) : list.values;
  };
</script>

<div class="panel">
  <div class="head">
    <h2>{t('filter.title')}</h2>
    <button class="flat" onclick={onreset}>{t('filter.reset')}</button>
  </div>

  <div class="boxed-list">
    {#each lists as list (list.key)}
      <button class="list-row unfold" aria-expanded={open === list.key} onclick={() => unfold(list.key)} disabled={!list.values.length}>
        <span class="grow">{list.label}</span>
        <span class="value" class:set={filters[list.key].length}>{summary(list)}</span>
        <span class="chevron" class:down={open === list.key}><Icon name="forward" /></span>
      </button>
      {#if open === list.key}
        <div class="options">
          {#if list.values.length > 8}
            <label class="find">
              <Icon name="search" />
              <input type="search" bind:value={search} placeholder={t('filter.find')} aria-label={t('filter.find')} />
            </label>
          {/if}
          <div class="checks scroll">
            {#each shown(list) as v (v.value)}
              <label class="check">
                <input type="checkbox" checked={filters[list.key].includes(v.value)} onchange={() => toggle(list.key, v.value)} />
                <span class="label">{list.name(v.value)}</span>
                <span class="count">{v.count}</span>
              </label>
            {/each}
          </div>
          {#if list.key === 'genre' || list.key === 'tag'}<p class="hint">{t('filter.allOf')}</p>{/if}
        </div>
      {/if}
    {/each}
  </div>

  {#each ranges as r (r.key)}
    <h3>{r.label}</h3>
    <div class="range">
      <input
        type="number"
        min="0"
        inputmode="numeric"
        bind:value={filters[`${r.key}_min`]}
        placeholder={r.range?.min != null ? `${t('filter.from')} ${r.range.min}` : t('filter.from')}
        aria-label="{r.label}: {t('filter.from')}"
      />
      <span class="dash">–</span>
      <input
        type="number"
        min="0"
        inputmode="numeric"
        bind:value={filters[`${r.key}_max`]}
        placeholder={r.range?.max != null ? `${t('filter.to')} ${r.range.max}` : t('filter.to')}
        aria-label="{r.label}: {t('filter.to')}"
      />
    </div>
  {/each}

  {#each groups as g (g.key)}
    <h3>{g.label}</h3>
    <div class="checks two">
      {#each g.values as v (v)}
        <label class="check" class:empty={!g.counts.get(v)}>
          <input type="checkbox" checked={filters[g.key].includes(v)} onchange={() => toggle(g.key, v)} />
          <span class="label">{t(`${g.prefix}.${v}`)}</span>
          {#if g.counts.get(v)}<span class="count">{g.counts.get(v)}</span>{/if}
        </label>
      {/each}
    </div>
  {/each}
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }
  .head h2 {
    margin: 0;
  }
  h3 {
    margin: 18px 0 8px;
  }
  .unfold {
    min-height: 48px;
  }
  .unfold:disabled {
    opacity: 0.5;
  }
  .value {
    color: var(--dim);
    font-size: 0.9rem;
    max-width: 55%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .value.set {
    color: var(--accent);
    font-weight: 700;
  }
  .chevron {
    display: inline-flex;
    color: var(--dim);
    transition: transform 0.15s;
  }
  .chevron.down {
    transform: rotate(90deg);
  }
  .options {
    padding: 4px 14px 12px;
    background: var(--hover);
  }
  .find {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 6px 0;
    padding: 0 10px;
    border-radius: var(--radius);
    background: var(--entry-bg);
    color: var(--dim);
  }
  .find input {
    flex: 1;
    background: transparent;
    padding-left: 0;
  }
  .find input:focus {
    outline: none;
  }
  .checks {
    display: grid;
    gap: 2px 12px;
  }
  /* minmax(0, …): columns may shrink below their content's width (a narrow drawer on phones). */
  .checks.two {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  }
  .scroll {
    max-height: 16rem;
    overflow-y: auto;
  }
  .check {
    min-height: 34px;
    min-width: 0;
  }
  .check .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .check.empty .label {
    color: var(--dim);
  }
  .count {
    color: var(--dim);
    font-size: 0.8rem;
    font-variant-numeric: tabular-nums;
  }
  .hint {
    margin: 6px 0 0;
    font-size: 0.8rem;
    color: var(--dim);
  }
  .range {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    align-items: center;
    gap: 8px;
  }
  .range input {
    width: 100%;
    min-width: 0;
  }
  .dash {
    color: var(--dim);
  }
</style>
