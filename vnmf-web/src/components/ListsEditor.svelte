<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // Editing the lists: rename (a basic list goes back to its default name when emptied), reorder,
  // delete own lists, add new ones. Every change is saved right away.
  import { api } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { lists, setLists, listName } from '../lib/lists.svelte.js';
  import { toastError } from '../lib/toast.svelte.js';
  import Icon from './Icon.svelte';

  let newName = $state('');
  let busy = $state(false);

  async function run(action) {
    busy = true;
    try {
      await action();
    } catch (e) {
      toastError(e);
    } finally {
      busy = false;
    }
  }

  function rename(list, value) {
    const name = value.trim();
    if ((name || null) === (list.name ?? null) || (!name && !list.kind)) return;
    run(async () => setLists(await api(`/lists/${list.id}`, { method: 'PUT', json: { name: name || null } })));
  }

  function move(index, step) {
    const ids = lists.items.map((l) => l.id);
    const [id] = ids.splice(index, 1);
    ids.splice(index + step, 0, id);
    run(async () => setLists(await api('/lists/order', { method: 'PUT', json: { ids } })));
  }

  function remove(list) {
    if (!confirm(t('lists.deleteConfirm', { name: listName(list) }))) return;
    run(async () => {
      await api(`/lists/${list.id}`, { method: 'DELETE' });
      setLists(await api('/lists'));
    });
  }

  function add(e) {
    e.preventDefault();
    const name = newName.trim();
    if (!name) return;
    run(async () => {
      await api('/lists', { method: 'POST', json: { name } });
      newName = '';
      setLists(await api('/lists'));
    });
  }
</script>

<p class="dim hint">{t('lists.hint')}</p>
<div class="boxed-list">
  {#each lists.items as list, i (list.id)}
    <div class="list-row">
      <input
        value={list.name ?? ''}
        placeholder={list.kind ? t(`list.${list.kind}`) : ''}
        aria-label={listName(list)}
        maxlength="64"
        onchange={(e) => rename(list, e.currentTarget.value)}
        onkeydown={(e) => e.key === 'Enter' && e.currentTarget.blur()}
      />
      {#if list.kind}<span class="chip basic">{t('lists.basic')}</span>{/if}
      <span class="tools">
        <button class="flat icon" onclick={() => move(i, -1)} disabled={busy || i === 0} title={t('lists.up')} aria-label={t('lists.up')}><span class="up"><Icon name="forward" /></span></button>
        <button class="flat icon" onclick={() => move(i, 1)} disabled={busy || i === lists.items.length - 1} title={t('lists.down')} aria-label={t('lists.down')}><span class="down"><Icon name="forward" /></span></button>
        {#if !list.kind}
          <button class="flat icon" onclick={() => remove(list)} disabled={busy} title={t('remove')} aria-label={t('remove')}><Icon name="trash" /></button>
        {:else}
          <span class="spacer"></span>
        {/if}
      </span>
    </div>
  {/each}
</div>

<form class="add" onsubmit={add}>
  <input bind:value={newName} placeholder={t('lists.newName')} aria-label={t('lists.newName')} maxlength="64" />
  <button class="primary" disabled={busy || !newName.trim()}><Icon name="plus" />{t('lists.add')}</button>
</form>

<style>
  .hint {
    margin: 0 0 12px;
    font-size: 0.88rem;
  }
  .list-row {
    gap: 8px;
    padding-right: 8px;
  }
  .list-row input {
    flex: 1;
    max-width: none !important;
    background: transparent;
    padding-left: 0;
  }
  .list-row input:focus {
    background: var(--entry-bg);
    padding-left: 8px;
  }
  .list-row input::placeholder {
    color: var(--fg);
    opacity: 1;
  }
  .basic {
    font-size: 0.72rem;
    padding: 0 8px;
  }
  .tools {
    display: flex;
  }
  .up,
  .down {
    display: inline-flex;
  }
  .up {
    transform: rotate(-90deg);
  }
  .down {
    transform: rotate(90deg);
  }
  .spacer {
    width: 34px;
  }
  .add {
    display: flex;
    gap: 8px;
    margin-top: 14px;
  }
  .add input {
    flex: 1;
  }
</style>
