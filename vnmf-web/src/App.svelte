<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { displayName } from './lib/format.js';
  import { onMount } from 'svelte';
  import { route } from './lib/router.svelte.js';
  import { session, loadSession, logout } from './lib/session.svelte.js';
  import { t, i18n } from './lib/i18n.svelte.js';
  import { toasts } from './lib/toast.svelte.js';
  import Icon from './components/Icon.svelte';
  import Menu from './components/Menu.svelte';
  import LanguageMenu from './components/LanguageMenu.svelte';
  import Avatar from './components/Avatar.svelte';
  import Login from './pages/Login.svelte';
  import Library from './pages/Library.svelte';
  import Book from './pages/Book.svelte';
  import Reader from './pages/Reader.svelte';
  import Upload from './pages/Upload.svelte';
  import Admin from './pages/Admin.svelte';
  import Settings from './pages/Settings.svelte';
  import Account from './pages/Account.svelte';
  import MyLibrary from './pages/MyLibrary.svelte';

  onMount(loadSession);

  const view = $derived.by(() => {
    const p = route.path.replace(/\/+$/, '') || '/';
    let m;
    if (p === '/') return { page: 'library' };
    if ((m = p.match(/^\/book\/(\d+)$/))) return { page: 'book', id: Number(m[1]) };
    if ((m = p.match(/^\/book\/(\d+)\/read$/))) return { page: 'reader', id: Number(m[1]) };
    if (p === '/upload') return { page: 'upload' };
    if (p === '/admin') return { page: 'admin' };
    if (p === '/settings') return { page: 'settings' };
    if (p === '/account') return { page: 'account' };
    if (p === '/my') return { page: 'my' };
    return { page: 'notfound' };
  });

  const tabs = $derived([
    { page: 'library', href: '/', icon: 'library', label: t('nav.library') },
    { page: 'my', href: '/my', icon: 'bookmark', label: t('nav.my') },
    ...(session.user?.is_admin
      ? [
          { page: 'upload', href: '/upload', icon: 'upload', label: t('nav.upload') },
          { page: 'admin', href: '/admin', icon: 'admin', label: t('nav.admin') },
        ]
      : []),
  ]);

  $effect(() => {
    document.documentElement.lang = i18n.lang;
  });
</script>

{#if !session.ready}
  <div class="center dim">{t('loading')}</div>
{:else if !session.user}
  <Login />
{:else if view.page === 'reader'}
  {#key view.id}
    <Reader id={view.id} />
  {/key}
{:else}
  <header class="headerbar">
    <div class="start">
      <a class="brand" href="/" aria-label="VNMF">
        <img src="/favicon.svg" alt="" width="24" height="24" />
        <span>VNMF</span>
      </a>
    </div>
    <nav class="switcher" aria-label="VNMF">
      {#each tabs as tab (tab.page)}
        <a href={tab.href} class:active={view.page === tab.page} aria-current={view.page === tab.page ? 'page' : undefined}>
          <Icon name={tab.icon} />
          <span>{tab.label}</span>
        </a>
      {/each}
    </nav>
    <div class="end">
      <LanguageMenu />
      <Menu label={t('nav.menu')}>
        {#snippet button()}
          <Avatar user={session.user} size={24} />
          <span class="username">{displayName(session.user)}</span>
        {/snippet}
        <a href="/account"><Icon name="user" /> {t('nav.account')}</a>
        <a href="/settings"><Icon name="settings" /> {t('nav.settings')}</a>
        <hr />
        <button onclick={logout}><Icon name="back" /> {t('nav.logout')}</button>
      </Menu>
    </div>
  </header>
  <main>
    {#if view.page === 'library'}
      <Library />
    {:else if view.page === 'book'}
      {#key view.id}
        <Book id={view.id} />
      {/key}
    {:else if view.page === 'upload' && session.user.is_admin}
      <Upload />
    {:else if view.page === 'admin' && session.user.is_admin}
      <Admin />
    {:else if view.page === 'settings'}
      <Settings />
    {:else if view.page === 'account'}
      <Account />
    {:else if view.page === 'my'}
      <MyLibrary />
    {:else}
      <div class="center">
        <p>{t('notFound')}</p>
        <a href="/">{t('nav.library')}</a>
      </div>
    {/if}
  </main>
{/if}

<div class="toasts" aria-live="polite">
  {#each toasts as item (item.id)}
    <div class="toast {item.kind}">{item.text}</div>
  {/each}
</div>

<style>
  .headerbar {
    position: sticky;
    top: 0;
    z-index: 10;
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    gap: 8px;
    min-height: 47px;
    padding: 6px max(8px, env(safe-area-inset-left));
    background: var(--headerbar-bg);
    box-shadow: inset 0 -1px var(--headerbar-shade);
  }
  .start,
  .end {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .end {
    justify-content: flex-end;
  }
  .brand {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    border-radius: var(--radius);
    color: var(--fg);
    font-weight: 800;
    letter-spacing: 0.04em;
  }
  .brand:hover {
    background: var(--hover);
    text-decoration: none;
  }
  /* AdwViewSwitcher: flat buttons, the active one tinted. */
  .switcher {
    display: flex;
    gap: 4px;
  }
  .switcher a {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 34px;
    padding: 5px 14px;
    border-radius: var(--radius);
    color: var(--fg);
    font-weight: 700;
    white-space: nowrap;
  }
  .switcher a:hover {
    background: var(--hover);
    text-decoration: none;
  }
  .switcher a.active {
    background: var(--button-bg);
  }
  main {
    max-width: 1400px;
    margin: 0 auto;
    padding: 18px max(16px, env(safe-area-inset-left)) 48px;
  }
  /* AdwToast: a dark pill at the bottom. */
  .toasts {
    position: fixed;
    left: 50%;
    bottom: 24px;
    transform: translateX(-50%);
    z-index: 100;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    width: max-content;
    max-width: calc(100vw - 32px);
  }
  .toast {
    padding: 10px 20px;
    border-radius: 9999px;
    background: var(--toast-bg);
    color: #fff;
    box-shadow: var(--popover-shadow);
    font-weight: 600;
    animation: rise 0.2s ease-out;
  }
  .toast.error {
    background: var(--destructive-bg);
  }
  @keyframes rise {
    from {
      transform: translateY(12px);
      opacity: 0;
    }
  }
  @media (max-width: 720px) {
    .headerbar {
      grid-template-columns: auto 1fr auto;
    }
    .brand span,
    .username,
    .switcher a span {
      display: none;
    }
    .switcher {
      justify-content: center;
    }
  }
</style>
