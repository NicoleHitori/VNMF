<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { onMount, onDestroy } from 'svelte';
  import { api } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { session } from '../lib/session.svelte.js';
  import { toast, toastError } from '../lib/toast.svelte.js';
  import { size, date, number, displayName } from '../lib/format.js';
  import { navigate } from '../lib/router.svelte.js';
  import Avatar from '../components/Avatar.svelte';

  let status = $state(null);
  let users = $state([]);
  let dups = $state(null);
  let dupBusy = $state(false);
  let newName = $state('');
  let newPassword = $state('');
  let newAdmin = $state(false);
  let timer = null;

  async function loadStatus() {
    try {
      status = await api('/library');
    } catch (e) {
      toastError(e);
    }
    clearTimeout(timer);
    // While a job runs, follow it.
    if (status?.scan.running || status?.verify.running) timer = setTimeout(loadStatus, 1500);
  }

  async function loadUsers() {
    try {
      users = (await api('/users')).users;
    } catch (e) {
      toastError(e);
    }
  }

  async function loadDuplicates() {
    try {
      dups = (await api('/library/duplicates')).groups;
    } catch (e) {
      toastError(e);
    }
  }

  onMount(() => {
    loadStatus();
    loadUsers();
    loadDuplicates();
  });

  // ------------------------------------------------------------------
  // Duplicates
  // ------------------------------------------------------------------

  const deletable = (c) => c.missing || c.writable;

  /** The copy that stays: a file that can't be deleted, else one people read, else the oldest. */
  function keeper(group) {
    const present = group.copies.filter((c) => !c.missing);
    const rank = (c) => [!deletable(c), c.readers > 0, -c.id];
    return present.sort((a, b) => {
      const [x, y] = [rank(a), rank(b)];
      for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return x[i] > y[i] ? -1 : 1;
      return 0;
    })[0];
  }

  /** Copies that can go, keeping `keep`: deletable and of the same size (or missing). */
  function removable(group, keep = keeper(group)) {
    if (!keep) return [];
    return group.copies.filter((c) => c.id !== keep.id && deletable(c) && (c.missing || c.size === keep.size));
  }

  async function dropCopies(keep, copies) {
    dupBusy = true;
    let done = 0;
    for (const c of copies) {
      try {
        await api(`/books/${keep.id}/copies/${c.id}`, { method: 'DELETE' });
        done++;
      } catch (e) {
        if (e.code === 'different_copy') toast(t('dups.different', { path: c.path }), 'error');
        else toastError(e);
      }
    }
    if (done) toast(t('dups.deleted', { n: done }), 'success');
    dupBusy = false;
    await Promise.all([loadDuplicates(), loadStatus()]);
  }

  function deleteCopy(group, c) {
    // Kept: another present copy of the same size, preferably one that can't be deleted.
    const keep = keeper({ ...group, copies: group.copies.filter((x) => x.id !== c.id && (c.missing || x.size === c.size)) });
    if (keep && confirm(t('dups.deleteConfirm', { path: c.path }))) dropCopies(keep, [c]);
  }

  function keepOne(group) {
    const keep = keeper(group);
    const copies = removable(group, keep);
    if (confirm(t('dups.keepOneConfirm', { path: keep.path, n: copies.length, title: group.title }))) dropCopies(keep, copies);
  }

  /** Whether `c` can be deleted in favour of another present copy of the same size. */
  const canDelete = (group, c) => deletable(c) && group.copies.some((x) => x.id !== c.id && !x.missing && (c.missing || x.size === c.size));
  onDestroy(() => clearTimeout(timer));

  async function start(job) {
    try {
      await api(`/library/${job}`, { method: 'POST' });
      setTimeout(loadStatus, 200);
    } catch (e) {
      if (e.status === 409) toast(t('admin.running'));
      else toastError(e);
    }
  }

  async function addUser(e) {
    e.preventDefault();
    try {
      await api('/users', { method: 'POST', json: { name: newName.trim(), password: newPassword, is_admin: newAdmin } });
      newName = newPassword = '';
      newAdmin = false;
      toast(t('admin.userAdded'), 'success');
      loadUsers();
    } catch (err) {
      toastError(err);
    }
  }

  const adminCount = $derived(users.filter((u) => u.is_admin).length);
  /** The last administrator can't lose the rights (the server refuses it too). */
  const lastAdmin = (u) => u.is_admin && adminCount <= 1;

  async function setAdmin(u, value, input) {
    const self = u.id === session.user.id;
    if (self && !value && !confirm(t('admin.dropOwnConfirm'))) {
      input.checked = true;
      return;
    }
    try {
      await api(`/users/${u.id}`, { method: 'PUT', json: { is_admin: value } });
      if (self && !value) {
        session.user = await api('/me');
        navigate('/');
        return;
      }
      loadUsers();
    } catch (e) {
      input.checked = u.is_admin;
      toastError(e);
    }
  }

  async function resetPassword(u) {
    const password = prompt(t('admin.newPasswordFor', { name: displayName(u) }));
    if (!password) return;
    try {
      await api(`/users/${u.id}`, { method: 'PUT', json: { password } });
      toast(t('admin.passwordChanged'), 'success');
    } catch (e) {
      toastError(e);
    }
  }

  async function removeUser(u) {
    if (!confirm(t('admin.deleteConfirm', { name: displayName(u) }))) return;
    try {
      await api(`/users/${u.id}`, { method: 'DELETE' });
      loadUsers();
    } catch (e) {
      toastError(e);
    }
  }

  function jobLine(job) {
    if (job.running) return t('admin.progress', { done: job.done, total: job.total });
    if (!job.finished_at) return t('admin.never');
    return t('admin.finishedAt', { when: date(job.finished_at) });
  }
</script>

<h1>{t('nav.admin')}</h1>

{#if status}
  <section class="stats">
    <div class="card stat"><div class="value">{number(status.counts.books)}</div><div class="muted">{t('admin.books')}</div></div>
    <div class="card stat"><div class="value">{number(status.counts.pages)}</div><div class="muted">{t('admin.pages')}</div></div>
    <div class="card stat"><div class="value">{size(status.counts.bytes)}</div><div class="muted">{t('admin.size')}</div></div>
    <div class="card stat"><div class="value">{number(status.counts.protected)}</div><div class="muted">{t('admin.protected')}</div></div>
    <a class="card stat" href="/?health=damaged" class:bad={status.counts.damaged > 0}>
      <div class="value">{number(status.counts.damaged)}</div>
      <div class="muted">{t('admin.damaged')}</div>
    </a>
    <div class="card stat"><div class="value">{number(status.counts.missing)}</div><div class="muted">{t('admin.missing')}</div></div>
    <a class="card stat" href="#duplicates" class:warn={status.counts.duplicates > 0}>
      <div class="value">{number(status.counts.duplicates)}</div>
      <div class="muted">{t('admin.duplicates')}</div>
    </a>
  </section>

  <section class="jobs">
    <div class="card job">
      <h2>{t('admin.scan')}</h2>
      <p class="muted">{t('admin.scanHint')}</p>
      <p>{jobLine(status.scan)}</p>
      {#if status.scan.finished_at && !status.scan.running}
        <p class="muted">{t('admin.scanResult', { added: status.scan.added, updated: status.scan.updated, missing: status.scan.missing })}</p>
      {/if}
      {#each status.scan.errors as e, i (i)}<p class="error-text small">{e}</p>{/each}
      <button onclick={() => start('scan')} disabled={status.scan.running}>{t('admin.scanNow')}</button>
    </div>
    <div class="card job">
      <h2>{t('admin.verify')}</h2>
      <p class="muted">{t('admin.verifyHint', { n: status.counts.not_fully_checked })}</p>
      {#if status.verify.running || status.verify.finished_at || !status.counts.oldest_full_check}
        <p>{jobLine(status.verify)}</p>
      {/if}
      {#if status.counts.oldest_full_check}<p class="muted">{t('admin.oldestCheck', { when: date(status.counts.oldest_full_check) })}</p>{/if}
      <p class="muted">{status.verify_interval_days ? t('admin.autoVerify', { n: status.verify_interval_days }) : t('admin.autoVerifyOff')}</p>
      {#if status.verify.finished_at && !status.verify.running}<p class="muted">{t('admin.verifyResult', { n: status.verify.damaged })}</p>{/if}
      {#each status.verify.errors as e, i (i)}<p class="error-text small">{e}</p>{/each}
      <button onclick={() => start('verify')} disabled={status.verify.running}>{t('admin.verifyNow')}</button>
    </div>
  </section>

  {#if status.folders}
    <section>
      <h2>{t('admin.folders')}</h2>
      <ul class="folders">
        <li><code>{status.folders.library}</code> — {t('admin.ownLibrary')}</li>
        {#each status.folders.extra as f (f)}
          <li><code>{f}</code> — {status.folders.writable_extra ? t('admin.writable') : t('admin.readOnly')}</li>
        {/each}
      </ul>
    </section>
  {/if}
{/if}

<section id="duplicates">
  <h2>{t('dups.title')}</h2>
  <p class="muted hint">{t('dups.hint')}</p>
  {#if dups && !dups.length}
    <p class="muted">{t('dups.none')}</p>
  {:else if dups}
    {#if status}<p class="summary">{t('dups.summary', { n: status.counts.duplicates, size: size(status.counts.duplicate_bytes) })}</p>{/if}
    {#each dups as g (g.uuid)}
      {@const keep = keeper(g)}
      <div class="group">
        <div class="group-head">
          <a class="group-title" href="/book/{keep?.id ?? g.copies[0].id}">{g.title}</a>
          <span class="muted mono uuid">{g.uuid}</span>
          {#if removable(g).length}
            <button title={t('dups.keepOneHint')} disabled={dupBusy} onclick={() => keepOne(g)}>{t('dups.keepOne')}</button>
          {/if}
        </div>
        <div class="boxed-list">
          {#each g.copies as c (c.id)}
            <div class="list-row">
              <a class="grow copy" href="/book/{c.id}">
                <span class="title mono">{c.path}</span>
                <span class="subtitle">
                  {[
                    size(c.size),
                    c.managed ? t('dups.uploaded') : !c.writable && t('dups.readOnly'),
                    c.missing && t('dups.missing'),
                    !c.missing && keep && c.size !== keep.size && t('dups.otherVersion'),
                    c.readers && t('dups.readers', { n: c.readers }),
                    date(c.added_at),
                  ]
                    .filter(Boolean)
                    .join(' · ')}
                </span>
              </a>
              {#if canDelete(g, c)}
                <button class="danger" disabled={dupBusy} onclick={() => deleteCopy(g, c)}>{c.missing ? t('dups.forget') : t('dups.delete')}</button>
              {/if}
            </div>
          {/each}
        </div>
      </div>
    {/each}
  {/if}
</section>

<section>
  <h2>{t('admin.users')}</h2>
  <div class="boxed-list table-wrap">
    <table>
      <thead><tr><th>{t('login.username')}</th><th>{t('admin.role')}</th><th>{t('admin.lastSeen')}</th><th></th></tr></thead>
      <tbody>
        {#each users as u (u.id)}
          <tr>
            <td>
              <span class="who">
                <Avatar user={u} size={28} />
                <span>{displayName(u)}{#if u.display_name}<span class="login muted">@{u.name}</span>{/if}</span>
              </span>
            </td>
            <td>
              <label class="check">
                <input
                  type="checkbox"
                  class="switch"
                  checked={u.is_admin}
                  disabled={lastAdmin(u)}
                  title={lastAdmin(u) ? t('admin.lastAdmin') : ''}
                  onchange={(e) => setAdmin(u, e.currentTarget.checked, e.currentTarget)}
                />
                {t('admin.administrator')}
              </label>
            </td>
            <td class="muted">{u.last_seen_at ? date(u.last_seen_at) : '—'}</td>
            <td class="row actions">
              <button onclick={() => resetPassword(u)}>{t('admin.resetPassword')}</button>
              {#if u.id !== session.user.id}<button class="danger" onclick={() => removeUser(u)}>{t('remove')}</button>{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  <h2 class="heading">{t('admin.addUser')}</h2>
  <form class="card add" onsubmit={addUser}>
    <div class="row">
      <input bind:value={newName} placeholder={t('login.username')} required autocomplete="off" />
      <input type="password" bind:value={newPassword} placeholder={t('login.password')} minlength="8" required autocomplete="new-password" />
      <label class="check"><input type="checkbox" bind:checked={newAdmin} /> {t('admin.administrator')}</label>
      <button class="primary">{t('admin.add')}</button>
    </div>
  </form>
</section>

<style>
  section {
    margin-bottom: 2rem;
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(9rem, 1fr));
    gap: 0.8rem;
  }
  .stat {
    color: inherit;
    text-decoration: none;
  }
  .stat .value {
    font-size: 1.5rem;
    font-weight: 800;
    color: var(--accent);
  }
  a.stat:hover {
    text-decoration: none;
    background: var(--hover);
  }
  .stat.bad .value {
    color: var(--destructive);
  }
  .stat.warn .value {
    color: var(--warning, #e5a50a);
  }
  .hint {
    margin: -0.4rem 0 0.8rem;
    font-size: 0.9rem;
    max-width: 60rem;
  }
  .summary {
    margin: 0 0 0.8rem;
  }
  .group {
    margin-bottom: 1rem;
  }
  .group-head {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    margin-bottom: 6px;
  }
  .group-title {
    font-weight: 700;
    color: inherit;
  }
  .uuid {
    font-size: 0.78rem;
    margin-right: auto;
  }
  .copy {
    min-width: 0;
    color: inherit;
  }
  .copy:hover {
    text-decoration: none;
  }
  .copy .title {
    overflow-wrap: anywhere;
    font-size: 0.85rem;
  }
  thead th {
    border-bottom: 1px solid var(--border);
  }
  .jobs {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr));
    gap: 1rem;
  }
  .job p {
    margin: 0.3rem 0;
  }
  .job button {
    margin-top: 0.5rem;
  }
  .small {
    font-size: 0.85rem;
  }
  .folders {
    padding-left: 1.2rem;
  }
  code {
    word-break: break-all;
  }
  .table-wrap {
    overflow-x: auto;
  }
  .actions {
    justify-content: flex-end;
    flex-wrap: nowrap;
  }
  .add {
    margin-top: 1rem;
  }
  .who {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    font-weight: 600;
  }
  .login {
    margin-left: 6px;
    font-weight: normal;
    font-size: 0.85rem;
  }
  .add .row input {
    flex: 1 1 10rem;
  }
</style>
