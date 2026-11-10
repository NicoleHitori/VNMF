// SPDX-License-Identifier: GPL-3.0-or-later
import { i18n } from './i18n.svelte.js';

const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];

export function size(bytes) {
  let v = Number(bytes) || 0;
  let u = 0;
  while (v >= 1024 && u < UNITS.length - 1) {
    v /= 1024;
    u++;
  }
  return u === 0 ? `${v} B` : `${v.toFixed(1)} ${UNITS[u]}`;
}

export function date(seconds) {
  if (!seconds) return '';
  return new Intl.DateTimeFormat(i18n.lang, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(seconds * 1000));
}

export function number(n) {
  return new Intl.NumberFormat(i18n.lang).format(n);
}

/** What a user is shown as: the nickname, else the login. */
export function displayName(user) {
  return user?.display_name || user?.name || '';
}
