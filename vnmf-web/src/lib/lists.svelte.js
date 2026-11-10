// SPDX-License-Identifier: GPL-3.0-or-later
// The user's reading lists, shared by the library, the book page and the editor.

import { api } from './api.js';
import { t } from './i18n.svelte.js';

export const lists = $state({ items: [], in_lists: 0, library: 0, loaded: false });

/** Applies a `/lists` answer. */
export function setLists(value) {
  lists.items = value.items;
  lists.in_lists = value.in_lists;
  lists.library = value.library;
  lists.loaded = true;
}

export async function loadLists() {
  setLists(await api('/lists'));
}

/** A basic list shows its default name in the interface language until renamed. */
export function listName(list) {
  return list.name ?? t(`list.${list.kind}`);
}
