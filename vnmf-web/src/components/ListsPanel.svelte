<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // The user's lists with the number of books (on the account page): each opens its books in the library.
  import { t } from '../lib/i18n.svelte.js';
  import { lists, listName } from '../lib/lists.svelte.js';
  import Icon from './Icon.svelte';

  let { onedit } = $props();
</script>

<div class="boxed-list">
  <a class="list-row" href="/?list=any">
    <Icon name="library" />
    <span class="grow">{t('lists.inLists')}</span><span class="count">{lists.in_lists}</span><Icon name="forward" />
  </a>
  {#each lists.items as list (list.id)}
    <a class="list-row" href="/?list={list.id}">
      <Icon name={list.kind ? 'bookmark' : 'contents'} />
      <span class="grow name">{listName(list)}</span><span class="count">{list.count}</span><Icon name="forward" />
    </a>
  {/each}
  <button class="list-row edit" onclick={onedit}>
    <Icon name="edit" />
    <span class="grow">{t('lists.edit')}</span>
  </button>
</div>

<style>
  .list-row {
    min-height: 46px;
    color: inherit;
  }
  .list-row :global(.icon) {
    color: var(--dim);
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    color: var(--dim);
    font-size: 0.85rem;
    font-variant-numeric: tabular-nums;
  }
  .edit {
    color: var(--dim);
  }
</style>
