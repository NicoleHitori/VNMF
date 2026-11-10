<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // The user's own library: who they are, their reading in numbers and their lists (each opens its
  // books in the catalog). Settings of the account are on the account page.
  import { onMount } from 'svelte';
  import { api } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { session } from '../lib/session.svelte.js';
  import { toastError } from '../lib/toast.svelte.js';
  import { number, displayName } from '../lib/format.js';
  import Avatar from '../components/Avatar.svelte';
  import Dialog from '../components/Dialog.svelte';
  import ListsPanel from '../components/ListsPanel.svelte';
  import ListsEditor from '../components/ListsEditor.svelte';
  import { loadLists } from '../lib/lists.svelte.js';

  let stats = $state(null);
  let editingLists = $state(false);

  onMount(async () => {
    try {
      [stats] = await Promise.all([api('/me/stats'), loadLists()]);
    } catch (e) {
      toastError(e);
    }
  });
</script>

<div class="clamp">
  <div class="profile">
    <Avatar user={session.user} size={88} />
    <h1>{displayName(session.user)}</h1>
  </div>

  {#if stats}
    <div class="stats">
      <div class="card stat"><span class="value">{number(stats.reading)}</span><span class="dim">{t('account.reading')}</span></div>
      <div class="card stat"><span class="value">{number(stats.finished)}</span><span class="dim">{t('account.finished')}</span></div>
      <div class="card stat"><span class="value">{number(stats.pages_read)}</span><span class="dim">{t('account.pagesRead')}</span></div>
      <div class="card stat"><span class="value">{number(stats.bookmarks)}</span><span class="dim">{t('account.bookmarks')}</span></div>
    </div>
  {/if}

  <h2 class="heading">{t('lists.title')}</h2>
  <ListsPanel onedit={() => (editingLists = true)} />
</div>

<Dialog open={editingLists} title={t('lists.title')} onclose={() => (editingLists = false)}>
  <ListsEditor />
</Dialog>

<style>
  .profile {
    display: flex;
    align-items: center;
    gap: 16px;
    margin: 8px 0 20px;
  }
  .profile h1 {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(8.5rem, 1fr));
    gap: 10px;
  }
  .stat {
    display: flex;
    flex-direction: column;
    padding: 12px 14px;
  }
  .stat .value {
    font-size: 1.5rem;
    font-weight: 800;
    color: var(--accent);
  }
</style>
