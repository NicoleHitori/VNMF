# VNMF Web

The web interface of `vnmf-server`: Svelte 5 in plain JavaScript, plain CSS, no other libraries.

The built files go into `../vnmf-server/web/` and are **embedded into the server binary** at
`cargo build` time (`vnmf-server/build.rs`). Running the server, and building it with Cargo,
needs no JavaScript tooling; Node.js (or Deno) is needed only to rebuild the interface after
changing it here.

```sh
npm install && npm run build      # or: deno install && deno task build
cargo build --release -p vnmf-server
```

Development with hot reload — the API is proxied to a running server on `127.0.0.1:8470`:

```sh
vnmf-server &          # in another terminal
npm run dev            # http://localhost:5173
```

## Layout

| | |
|---|---|
| `src/lib/api.js` | API client (session cookie + `X-VNMF-CSRF`), upload with progress, data URLs |
| `src/lib/session.svelte.js` | the signed-in user, server info, settings stored on the server |
| `src/lib/router.svelte.js` | history routing (the server answers unknown paths with `index.html`) |
| `src/lib/i18n.svelte.js` | English and Ukrainian, plural forms |
| `src/lib/audio.js` | page music and sound effects |
| `src/pages/` | library, book, reader, upload, administration, settings |

Interface text goes through `t('key')`; add new keys to both dictionaries in `i18n.svelte.js`.
