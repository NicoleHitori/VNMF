<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  import { t, i18n, LANGUAGES } from '../lib/i18n.svelte.js';
  import { session, updateSettings } from '../lib/session.svelte.js';

  const prefs = $derived(session.settings.reader);
  const reader = (patch) => updateSettings({ reader: patch });

  // Sliders show the value while dragging and save it when released.
  let volume = $state(null);
  let stripWidth = $state(null);
</script>

<div class="clamp">
  <h1>{t('nav.settings')}</h1>

  <h2 class="heading">{t('settings.interface')}</h2>
  <div class="boxed-list">
    <label class="list-row">
      <span class="grow">{t('settings.language')}</span>
      <select value={i18n.lang} onchange={(e) => updateSettings({ language: e.currentTarget.value })}>
        {#each Object.entries(LANGUAGES) as [code, name] (code)}<option value={code}>{name}</option>{/each}
      </select>
    </label>
    <label class="list-row">
      <span class="grow">{t('settings.theme')}</span>
      <select value={session.settings.theme ?? 'system'} onchange={(e) => updateSettings({ theme: e.currentTarget.value })}>
        <option value="system">{t('settings.theme.system')}</option>
        <option value="light">{t('settings.theme.light')}</option>
        <option value="dark">{t('settings.theme.dark')}</option>
      </select>
    </label>
  </div>

  <h2 class="heading">{t('settings.reader')}</h2>
  <p class="dim hint">{t('settings.readerHint')}</p>
  <div class="boxed-list">
    <label class="list-row">
      <span class="grow">{t('reader.mode')}</span>
      <select value={prefs.mode} onchange={(e) => reader({ mode: e.currentTarget.value })}>
        {#each ['auto', 'single', 'double', 'strip'] as v (v)}<option value={v}>{t(`reader.mode.${v}`)}</option>{/each}
      </select>
    </label>
    <label class="list-row">
      <span class="grow">{t('reader.direction')}</span>
      <select value={prefs.direction} onchange={(e) => reader({ direction: e.currentTarget.value })}>
        {#each ['auto', 'ltr', 'rtl'] as v (v)}<option value={v}>{t(`reader.direction.${v}`)}</option>{/each}
      </select>
    </label>
    <label class="list-row">
      <span class="grow">{t('reader.fit')}</span>
      <select value={prefs.fit} onchange={(e) => reader({ fit: e.currentTarget.value })}>
        <option value="contain">{t('reader.fit.contain')}</option>
        <option value="width">{t('reader.fit.width')}</option>
      </select>
    </label>
    <label class="list-row">
      <span class="grow">{t('reader.coverAlone')}</span>
      <input type="checkbox" class="switch" checked={prefs.coverAlone} onchange={(e) => reader({ coverAlone: e.currentTarget.checked })} />
    </label>
    <label class="list-row">
      <span class="grow">{t('reader.dark')}<span class="subtitle">{t('settings.darkHint')}</span></span>
      <input type="checkbox" class="switch" checked={prefs.dark} onchange={(e) => reader({ dark: e.currentTarget.checked })} />
    </label>
    <label class="list-row">
      <span class="grow">{t('reader.stripWidth', { px: stripWidth ?? prefs.stripWidth })}</span>
      <input
        type="range"
        min="360"
        max="1600"
        step="20"
        value={prefs.stripWidth}
        oninput={(e) => (stripWidth = Number(e.currentTarget.value))}
        onchange={(e) => {
          reader({ stripWidth: Number(e.currentTarget.value) });
          stripWidth = null;
        }}
      />
    </label>
  </div>

  <h2 class="heading">{t('settings.sound')}</h2>
  <div class="boxed-list">
    <label class="list-row">
      <span class="grow">{t('reader.sound')}<span class="subtitle">{t('settings.soundHint')}</span></span>
      <input type="checkbox" class="switch" checked={prefs.sound} onchange={(e) => reader({ sound: e.currentTarget.checked })} />
    </label>
    <label class="list-row">
      <span class="grow">{t('reader.volume', { p: Math.round((volume ?? prefs.volume) * 100) })}</span>
      <input
        type="range"
        min="0"
        max="1"
        step="0.05"
        value={prefs.volume}
        oninput={(e) => (volume = Number(e.currentTarget.value))}
        onchange={(e) => {
          reader({ volume: Number(e.currentTarget.value) });
          volume = null;
        }}
      />
    </label>
  </div>

  <p class="dim more">{t('settings.accountLink')} <a href="/account">{t('nav.account')}</a></p>
</div>

<style>
  .hint {
    margin: -4px 0 8px;
    font-size: 0.88rem;
  }
  .more {
    margin-top: 24px;
  }
</style>
