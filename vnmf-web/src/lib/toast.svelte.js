// SPDX-License-Identifier: GPL-3.0-or-later
// Short messages in the corner.

export const toasts = $state([]);
let next = 1;

export function toast(text, kind = 'info', ms = 4000) {
  const id = next++;
  toasts.push({ id, text, kind });
  setTimeout(() => {
    const i = toasts.findIndex((t) => t.id === id);
    if (i >= 0) toasts.splice(i, 1);
  }, ms);
}

export const toastError = (e) => toast(e?.message ?? String(e), 'error', 7000);
