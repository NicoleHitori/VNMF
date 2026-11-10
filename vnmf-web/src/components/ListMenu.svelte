<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // "Add to list" on the book page: a basic list (one at a time) and any of the user's own lists.
  import { onMount } from 'svelte';
  import { api } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { lists, loadLists, listName } from '../lib/lists.svelte.js';
  import { toastError } from '../lib/toast.svelte.js';
  import Icon from './Icon.svelte';
  import Menu from './Menu.svelte';

  let { book, onchange } = $props();
  let member = $state([]); // ids of the lists that have the book

  onMount(async () => {
    try {
      const [, mine] = await Promise.all([lists.loaded ? null : loadLists(), api(`/books/${book}/lists`)]);
      member = mine.lists;
    } catch (e) {
      toastError(e);
    }
  });

  const basic = $derived(lists.items.filter((l) => l.kind));
  const own = $derived(lists.items.filter((l) => !l.kind));
  const current = $derived(basic.find((l) => member.includes(l.id)));
  const ownCount = $derived(own.filter((l) => member.includes(l.id)).length);

  async function toggle(list) {
    const inside = member.includes(list.id);
    try {
      const r = await api(`/books/${book}/lists/${list.id}`, { method: inside ? 'DELETE' : 'PUT' });
      member = r.lists;
      await loadLists();
      onchange?.(); // "completed" marks the book read
    } catch (e) {
      toastError(e);
    }
  }
</script>

<div class="list-menu" class:set={current || ownCount}>
  <Menu label={t('lists.addTo')} align="start" flat={false}>
    {#snippet button()}
      <Icon name={current || ownCount ? 'check' : 'bookmark'} />
      <span>{current ? listName(current) : ownCount ? t('lists.inOwn', { n: ownCount }) : t('lists.addTo')}</span>
    {/snippet}
    {#each basic as list (list.id)}
      <button role="menuitemradio" aria-checked={member.includes(list.id)} class:current={member.includes(list.id)} onclick={() => toggle(list)}>
        <span class="mark">{#if member.includes(list.id)}<Icon name="check" />{/if}</span>{listName(list)}
      </button>
    {/each}
    {#if own.length}
      <hr />
      {#each own as list (list.id)}
        <button role="menuitemcheckbox" aria-checked={member.includes(list.id)} class:current={member.includes(list.id)} onclick={() => toggle(list)}>
          <span class="mark">{#if member.includes(list.id)}<Icon name="check" />{/if}</span>{listName(list)}
        </button>
      {/each}
    {/if}
  </Menu>
</div>

<style>
  .list-menu :global(.menu > button) {
    width: 100%;
    border-radius: 9999px;
    min-height: 40px;
  }
  .list-menu.set :global(.menu > button) {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .list-menu :global(.menu > button span) {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .mark {
    display: inline-flex;
    width: 16px;
    margin-right: 6px;
  }
</style>
