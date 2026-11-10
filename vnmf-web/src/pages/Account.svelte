<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { onMount } from 'svelte';
  import { api } from '../lib/api.js';
  import { t } from '../lib/i18n.svelte.js';
  import { session, logout } from '../lib/session.svelte.js';
  import { toast, toastError } from '../lib/toast.svelte.js';
  import { date, displayName } from '../lib/format.js';
  import Icon from '../components/Icon.svelte';
  import Avatar from '../components/Avatar.svelte';
  import Menu from '../components/Menu.svelte';
  import { squareAvatar } from '../lib/image.js';

  let sessions = $state([]);
  let nickname = $state(session.user.display_name ?? '');
  let oldPassword = $state('');
  let newPassword = $state('');
  let repeat = $state('');
  let busy = $state('');
  let avatarInput = $state();

  async function load() {
    try {
      sessions = (await api('/me/sessions')).items;
    } catch (e) {
      toastError(e);
    }
  }

  onMount(load);

  async function run(what, action) {
    busy = what;
    try {
      await action();
    } catch (e) {
      toastError(e);
    } finally {
      busy = '';
    }
  }

  function chooseAvatar(e) {
    const file = e.currentTarget.files?.[0];
    e.currentTarget.value = '';
    if (!file) return;
    run('avatar', async () => {
      let blob;
      try {
        blob = await squareAvatar(file);
      } catch {
        throw new Error(t('account.notImage'));
      }
      session.user = await api('/me/avatar', { method: 'PUT', body: blob, headers: { 'Content-Type': blob.type } });
      toast(t('account.avatarChanged'), 'success');
    });
  }

  function removeAvatar() {
    run('avatar', async () => {
      session.user = await api('/me/avatar', { method: 'DELETE' });
    });
  }

  function rename(e) {
    e.preventDefault();
    run('name', async () => {
      session.user = await api('/me', { method: 'PUT', json: { display_name: nickname.trim() || null } });
      nickname = session.user.display_name ?? '';
      toast(t('account.renamed'), 'success');
    });
  }

  function changePassword(e) {
    e.preventDefault();
    if (newPassword !== repeat) return toast(t('settings.mismatch'), 'error');
    run('password', async () => {
      await api('/me/password', { method: 'PUT', json: { old_password: oldPassword, new_password: newPassword } });
      oldPassword = newPassword = repeat = '';
      toast(t('account.passwordChanged'), 'success');
      await load();
    });
  }

  function endSession(s) {
    run(s.id, async () => {
      await api(`/me/sessions/${s.id}`, { method: 'DELETE' });
      sessions = sessions.filter((x) => x.id !== s.id);
    });
  }

  function endOthers() {
    if (!confirm(t('account.endOthersConfirm'))) return;
    run('others', async () => {
      const r = await api('/me/sessions', { method: 'DELETE' });
      toast(t('account.ended', { n: r.ended }), 'success');
      await load();
    });
  }

  /** "Firefox · Linux" from a browser's User-Agent; any other label as is. */
  function device(label) {
    if (!label) return { text: t('account.unknownDevice'), mobile: false };
    if (!label.startsWith('Mozilla/')) return { text: label, mobile: /phone|android/i.test(label) };
    const browser = [
      ['Firefox', /Firefox\//],
      ['Edge', /Edg\//],
      ['Opera', /OPR\//],
      ['Chrome', /Chrome\//],
      ['Safari', /Safari\//],
    ].find(([, re]) => re.test(label))?.[0];
    const os = [
      ['Android', /Android/],
      ['iOS', /iPhone|iPad/],
      ['Windows', /Windows/],
      ['macOS', /Mac OS X/],
      ['Linux', /Linux/],
    ].find(([, re]) => re.test(label))?.[0];
    return { text: [browser, os].filter(Boolean).join(' · ') || label, mobile: /Android|iPhone|Mobile/.test(label) };
  }
</script>

<div class="clamp">
  <div class="profile">
    <div class="avatar-edit">
      <Avatar user={session.user} size={88} />
      <div class="avatar-menu">
        <Menu label={t('account.avatar')} align="start" flat={false}>
          {#snippet button()}<Icon name="camera" />{/snippet}
          <button onclick={() => avatarInput.click()}><Icon name="image" /> {session.user.avatar ? t('account.changeAvatar') : t('account.addAvatar')}</button>
          {#if session.user.avatar}
            <button onclick={removeAvatar}><Icon name="trash" /> {t('account.removeAvatar')}</button>
          {/if}
        </Menu>
      </div>
      <input bind:this={avatarInput} type="file" accept="image/*" class="visually-hidden" onchange={chooseAvatar} />
    </div>
    <div>
      <h1>{displayName(session.user)}</h1>
      <p class="dim">
        {[session.user.display_name && `@${session.user.name}`, session.user.is_admin ? t('account.administrator') : t('account.reader')].filter(Boolean).join(' · ')}
      </p>
    </div>
  </div>

  <h2 class="heading">{t('account.profile')}</h2>
  <form class="boxed-list" onsubmit={rename}>
    <label class="list-row">
      <span class="grow">
        <span class="title">{t('account.nickname')}</span>
        <span class="subtitle">{t('account.nicknameHint')}</span>
      </span>
      <input bind:value={nickname} placeholder={session.user.name} maxlength="64" autocomplete="nickname" />
    </label>
    <div class="list-row">
      <span class="grow">
        <span class="title">{t('account.login')}</span>
        <span class="subtitle">{t('account.loginHint')}</span>
      </span>
      <span class="dim mono">{session.user.name}</span>
    </div>
    <div class="list-row end">
      <button class="primary" disabled={busy === 'name' || nickname.trim() === (session.user.display_name ?? '')}>{t('save')}</button>
    </div>
  </form>

  <h2 class="heading">{t('account.password')}</h2>
  <form class="boxed-list" onsubmit={changePassword}>
    <input type="text" autocomplete="username" value={session.user.name} hidden readonly />
    <label class="list-row">
      <span class="grow">{t('settings.oldPassword')}</span>
      <input type="password" bind:value={oldPassword} autocomplete="current-password" required />
    </label>
    <label class="list-row">
      <span class="grow">{t('settings.newPassword')}</span>
      <input type="password" bind:value={newPassword} autocomplete="new-password" minlength="8" required />
    </label>
    <label class="list-row">
      <span class="grow">{t('settings.repeatPassword')}</span>
      <input type="password" bind:value={repeat} autocomplete="new-password" minlength="8" required />
    </label>
    <div class="list-row end">
      <span class="grow dim small">{t('account.passwordHint')}</span>
      <button class="primary" disabled={busy === 'password'}>{t('settings.changePassword')}</button>
    </div>
  </form>

  <h2 class="heading">{t('account.sessions')}</h2>
  <ul class="boxed-list">
    {#each sessions as s (s.id)}
      {@const d = device(s.label)}
      <li class="list-row">
        <Icon name={d.mobile ? 'phone' : 'computer'} size={20} />
        <div class="grow">
          <span class="title">{d.text}{#if s.current}<span class="chip accent here">{t('account.thisDevice')}</span>{/if}</span>
          <span class="subtitle">{t('account.lastActive', { when: date(s.last_used_at) })} · {t('account.signedIn', { when: date(s.created_at) })}</span>
        </div>
        {#if s.current}
          <button onclick={logout}>{t('nav.logout')}</button>
        {:else}
          <button class="danger" onclick={() => endSession(s)} disabled={busy === s.id}>{t('account.end')}</button>
        {/if}
      </li>
    {/each}
  </ul>
  {#if sessions.length > 1}
    <div class="row after">
      <button class="danger" onclick={endOthers} disabled={busy === 'others'}>{t('account.endOthers')}</button>
    </div>
  {/if}
</div>


<style>
  .profile {
    display: flex;
    align-items: center;
    gap: 16px;
    margin: 8px 0 20px;
  }
  .profile h1 {
    margin: 0;
  }
  .profile p {
    margin: 0;
  }
  .avatar-edit {
    position: relative;
    flex: none;
  }
  /* A round button on the avatar's corner, like in GNOME Settings → Users. */
  .avatar-menu {
    position: absolute;
    right: -4px;
    bottom: -4px;
  }
  .avatar-menu :global(.menu > button) {
    width: 34px;
    min-width: 34px;
    padding: 0;
    border-radius: 50%;
    background: var(--headerbar-bg);
    box-shadow: var(--card-shadow);
  }
  .avatar-menu :global(.menu > button:hover) {
    background: var(--popover-bg);
  }
  .end {
    justify-content: flex-end;
  }
  .small {
    font-size: 0.85rem;
  }
  .here {
    margin-left: 8px;
    font-size: 0.75rem;
    padding: 0 8px;
  }
  .after {
    justify-content: flex-end;
    margin-top: 10px;
  }
  @media (max-width: 600px) {
    label.list-row {
      flex-direction: column;
      align-items: stretch;
      gap: 6px;
    }
    .list-row input {
      max-width: none !important;
    }
  }
</style>
