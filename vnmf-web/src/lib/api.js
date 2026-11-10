// SPDX-License-Identifier: GPL-3.0-or-later
// The vnmf-server API (docs/SERVER.md). Authentication is the session cookie set by /auth/login;
// changing requests carry the X-VNMF-CSRF header the server requires with the cookie.

const BASE = '/api/v1';

export class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

/** Called when the session has ended (401) — set by the session module. */
let onUnauthorized = () => {};
export function setUnauthorizedHandler(fn) {
  onUnauthorized = fn;
}

async function failure(res) {
  let data = null;
  try {
    data = await res.json();
  } catch {
    /* not JSON */
  }
  return new ApiError(res.status, data?.error ?? 'http', data?.message ?? `${res.status} ${res.statusText}`);
}

/**
 * A request to the API. `json` — a body to send as JSON; `body` — any other body.
 * Returns parsed JSON, text, or null for empty responses.
 */
export async function api(path, { method = 'GET', json, body, headers = {}, signal, keepalive } = {}) {
  const h = { ...headers };
  if (method !== 'GET' && method !== 'HEAD') h['X-VNMF-CSRF'] = '1';
  if (json !== undefined) {
    h['Content-Type'] = 'application/json';
    body = JSON.stringify(json);
  }
  const res = await fetch(BASE + path, { method, headers: h, body, signal, keepalive, credentials: 'same-origin' });
  if (res.status === 401 && !path.startsWith('/auth/')) onUnauthorized();
  if (!res.ok) throw await failure(res);
  if (res.status === 204 || res.status === 202) return null;
  const type = res.headers.get('content-type') ?? '';
  return type.includes('json') ? res.json() : res.text();
}

/** An upload with progress (fetch can't report it). `onProgress(fraction)`. */
export function upload(path, body, { contentType, onProgress } = {}) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', BASE + path);
    xhr.withCredentials = true;
    xhr.setRequestHeader('X-VNMF-CSRF', '1');
    if (contentType) xhr.setRequestHeader('Content-Type', contentType);
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress?.(e.loaded / e.total);
    xhr.onerror = () => reject(new ApiError(0, 'network', 'network error'));
    xhr.onload = () => {
      let data = null;
      try {
        data = JSON.parse(xhr.responseText);
      } catch {
        /* empty */
      }
      if (xhr.status >= 200 && xhr.status < 300) resolve(data);
      else {
        if (xhr.status === 401) onUnauthorized();
        reject(new ApiError(xhr.status, data?.error ?? 'http', data?.message ?? `${xhr.status}`));
      }
    };
    xhr.send(body);
  });
}

/** URLs of binary data. `v` — the book version: such URLs are cached for good. */
const q = (v) => (v ? `?v=${encodeURIComponent(v)}` : '');
export const urls = {
  chunk: (id, c, v) => `${BASE}/books/${id}/chunks/${c}${q(v)}`,
  tile: (id, c, t, v) => `${BASE}/books/${id}/chunks/${c}/tiles/${t}${q(v)}`,
  dark: (id, page, part, v) => `${BASE}/books/${id}/pages/${page}/dark/${part}${q(v)}`,
  aux: (id, k, v) => `${BASE}/books/${id}/aux/${k}${q(v)}`,
  cover: (id, v) => `${BASE}/books/${id}/cover${q(v)}`,
  coverAt: (id, i, v) => `${BASE}/books/${id}/covers/${i}${q(v)}`,
  file: (id) => `${BASE}/books/${id}/file`,
  export: (id, format) => `${BASE}/books/${id}/export?format=${format}`,
  comicinfo: (id) => `${BASE}/books/${id}/comicinfo.xml`,
};
