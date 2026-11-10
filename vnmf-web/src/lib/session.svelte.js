// SPDX-License-Identifier: GPL-3.0-or-later
// The signed-in user, the server info and the user's settings (stored on the server: they follow the
// user to every device).

import { api, setUnauthorizedHandler } from './api.js';
import { setLanguage } from './i18n.svelte.js';

/** Reader defaults; the stored settings override them. */
export const READER_DEFAULTS = {
  mode: 'auto', // auto | single | double | strip
  direction: 'auto', // auto | ltr | rtl
  fit: 'contain', // contain | width
  coverAlone: true,
  dark: false,
  sound: false,
  volume: 0.8,
  stripWidth: 800,
};

export const session = $state({
  ready: false,
  server: null,
  user: null,
  settings: { reader: { ...READER_DEFAULTS } },
});

function applyTheme() {
  const theme = session.settings.theme ?? 'system';
  if (theme === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

function applySettings(stored) {
  session.settings = { ...stored, reader: { ...READER_DEFAULTS, ...(stored?.reader ?? {}) } };
  if (stored?.language) setLanguage(stored.language);
  applyTheme();
}

setUnauthorizedHandler(() => {
  session.user = null;
});

export async function loadSession() {
  try {
    session.server = await api('/server');
  } catch {
    session.server = null;
  }
  try {
    session.user = await api('/me');
    applySettings(await api('/me/settings'));
  } catch {
    session.user = null;
  }
  session.ready = true;
}

export async function login(username, password) {
  const res = await api('/auth/login', { method: 'POST', json: { username, password, label: navigator.userAgent.slice(0, 100) } });
  session.user = res.user;
  applySettings(await api('/me/settings'));
}

export async function logout() {
  try {
    await api('/auth/logout', { method: 'POST' });
  } finally {
    session.user = null;
  }
}

let saveTimer = null;

/** Merges `patch` into the settings and saves them (debounced). `reader` is merged one level deeper. */
export function updateSettings(patch) {
  const next = { ...session.settings, ...patch };
  if (patch.reader) next.reader = { ...session.settings.reader, ...patch.reader };
  session.settings = next;
  if (patch.language) setLanguage(patch.language);
  applyTheme();
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    api('/me/settings', { method: 'PUT', json: $state.snapshot(session.settings) }).catch(() => {});
  }, 600);
}
