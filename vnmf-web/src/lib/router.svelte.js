// SPDX-License-Identifier: GPL-3.0-or-later
// History-based routing: the server answers every non-API path with index.html.

function read() {
  return { path: location.pathname, query: Object.fromEntries(new URLSearchParams(location.search)) };
}

export const route = $state(read());

function sync() {
  const r = read();
  route.path = r.path;
  route.query = r.query;
}

export function navigate(to, { replace = false } = {}) {
  const url = new URL(to, location.href);
  if (url.pathname === location.pathname && url.search === location.search) return;
  if (replace) history.replaceState(null, '', url);
  else history.pushState(null, '', url);
  sync();
  if (!replace) window.scrollTo(0, 0);
}

/** Updates the query of the current page without a new history entry. */
export function setQuery(params) {
  const search = new URLSearchParams(location.search);
  for (const [k, v] of Object.entries(params)) {
    if (v === '' || v == null || v === false) search.delete(k);
    else search.set(k, String(v));
  }
  const s = search.toString();
  navigate(location.pathname + (s ? `?${s}` : ''), { replace: true });
}

addEventListener('popstate', sync);

// Plain <a href="/…"> links navigate without reloading.
document.addEventListener('click', (e) => {
  if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  const a = e.target.closest?.('a[href]');
  if (!a || a.target || a.hasAttribute('download')) return;
  const url = new URL(a.href, location.href);
  if (url.origin !== location.origin || url.pathname.startsWith('/api/')) return;
  e.preventDefault();
  navigate(url.pathname + url.search);
});
