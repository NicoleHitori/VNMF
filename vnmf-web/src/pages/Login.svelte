<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { session, login } from '../lib/session.svelte.js';
  import { t } from '../lib/i18n.svelte.js';
  import LanguageMenu from '../components/LanguageMenu.svelte';

  let username = $state('');
  let password = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(e) {
    e.preventDefault();
    busy = true;
    error = '';
    try {
      await login(username.trim(), password);
    } catch (err) {
      error = err.code === 'bad_credentials' ? t('login.wrong') : err.message;
    } finally {
      busy = false;
    }
  }
</script>

<div class="page">
  <div class="lang"><LanguageMenu /></div>
  <form onsubmit={submit}>
    <img src="/favicon.svg" alt="" width="96" height="96" />
    <h1>{t('login.welcome')}</h1>
    <p class="dim">{t('login.subtitle')}</p>
    {#if session.server === null}
      <p class="error-text">{t('login.noServer')}</p>
    {:else if !session.server.has_users}
      <p class="dim">{t('login.noUsers')}</p>
      <pre>vnmf-server user add NAME --admin</pre>
    {/if}
    <div class="boxed-list">
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={username} placeholder={t('login.username')} aria-label={t('login.username')} autocomplete="username" autocapitalize="none" required autofocus />
      <input type="password" bind:value={password} placeholder={t('login.password')} aria-label={t('login.password')} autocomplete="current-password" required />
    </div>
    {#if error}<p class="error-text">{error}</p>{/if}
    <button class="primary pill" disabled={busy}>{t('login.submit')}</button>
  </form>
</div>

<style>
  .page {
    min-height: 100dvh;
    display: grid;
    place-items: center;
    padding: 16px;
  }
  .lang {
    position: fixed;
    top: 8px;
    right: 8px;
  }
  form {
    width: min(22rem, 100%);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    text-align: center;
  }
  h1 {
    margin: 4px 0 0;
  }
  p {
    margin: 0;
  }
  .boxed-list {
    width: 100%;
  }
  .boxed-list input {
    width: 100%;
    min-height: 52px;
    padding: 0 14px;
    border-radius: 0;
    background: transparent;
  }
  .boxed-list input:focus {
    outline-offset: -3px;
    border-radius: 12px;
  }
  button {
    margin-top: 8px;
    min-width: 10rem;
  }
  pre {
    margin: 0;
    padding: 8px 12px;
    background: var(--entry-bg);
    border-radius: var(--radius);
    overflow-x: auto;
    font-size: 0.85rem;
  }
</style>
