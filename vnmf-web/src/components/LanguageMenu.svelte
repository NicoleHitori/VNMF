<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { t, i18n, LANGUAGES, setLanguage } from '../lib/i18n.svelte.js';
  import { session, updateSettings } from '../lib/session.svelte.js';
  import Icon from './Icon.svelte';
  import Menu from './Menu.svelte';

  function choose(lang) {
    // Signed in: also stored with the settings, so it follows the user.
    if (session.user) updateSettings({ language: lang });
    else setLanguage(lang);
  }
</script>

<Menu label={t('settings.language')}>
  {#snippet button()}
    <Icon name="language" />
    <span class="code">{i18n.lang.toUpperCase()}</span>
  {/snippet}
  {#each Object.entries(LANGUAGES) as [code, name] (code)}
    <button role="menuitemradio" aria-checked={i18n.lang === code} class:current={i18n.lang === code} onclick={() => choose(code)}>
      {name}
    </button>
  {/each}
</Menu>

<style>
  .code {
    font-size: 0.8rem;
    font-weight: 800;
  }
</style>
