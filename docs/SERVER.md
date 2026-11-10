# VNMF Server

`vnmf-server` serves a library of `.vnmf` files over HTTP: reading (pages, strips, spreads, music,
sound effects, text layers, dark pages), uploads (`.vnmf` as is, CBZ/ZIP converted), metadata and
cover editing, protection and repair, per-user reading progress and bookmarks.

The files are the source of truth. The database (SQLite) keeps users, progress, bookmarks and an
index of the library that a rescan can always rebuild. Images and sound are sent straight from the
memory-mapped files — no copying, no recompression.

> **Status: beta.** The API is versioned (`/api/v1`) and may still get compatible additions.

## Running

```sh
cargo build --release -p vnmf-server
vnmf-server user add nicole --admin            # asks for the password (or VNMF_PASSWORD)
vnmf-server --library ~/Manga                  # http://127.0.0.1:8470
```

| Option | Env | Default | Meaning |
|---|---|---|---|
| `--data DIR` | `VNMF_DATA` | `$XDG_DATA_HOME/vnmf-server` | database, uploaded books, caches |
| `--listen ADDR` | `VNMF_LISTEN` | `127.0.0.1:8470` | address and port |
| `--library DIR` (repeatable) | `VNMF_LIBRARIES` (`:`-separated) | — | extra folders scanned recursively |
| `--writable-libraries` | | off | allow changing/deleting files in `--library` folders |
| `--max-upload-mib N` | | 4096 | upload size limit |
| `--scan-interval MIN` | | 0 | periodic rescan (0 — at startup and on request) |
| `--verify-interval DAYS` | `VNMF_VERIFY_INTERVAL` | 7 | every book gets a full integrity check at least this often, in the background (checked hourly; books never fully checked or changed since go first; a restart doesn't reset it). Silent disk damage is found while the redundancy can still repair it. 0 — only on request |
| `--cors-origin URL` (repeatable) | | — | browser apps on other origins |
| `--secure-cookies` | | off | `Secure` session cookie (behind HTTPS) |

The web interface is at the same address (`/`): the library, the reader, uploads, administration.
It's built from `vnmf-web/` and embedded into the binary — see [vnmf-web/README.md](../vnmf-web/README.md).

Log level: `VNMF_LOG` (`vnmf_server=debug,tower_http=debug`).

Other commands: `user add|passwd|list|remove`, `scan` (index once and exit).

Data folder layout:

```text
<data>/vnmf-server.db   database
<data>/library/         uploaded books — the server's own library
<data>/incoming/        uploads in progress (cleared at startup)
<data>/cache/export/    CBZ / ZIP exports (rebuilt when a book changes)
<data>/cache/tiles/     JPEG tiles of strip fragments taller than browsers decode
```

**Own library vs. extra folders.** Uploaded books live in `<data>/library`; the server may rewrite
and delete them. Books found in `--library` folders are read-only unless `--writable-libraries`.
Hidden files and folders (`.name`) are skipped.

**Identity.** A book is its file path. A file that disappears is kept as *missing* (with progress);
a file with the same UUID appearing elsewhere is taken as moved and keeps its id, progress and
bookmarks. Rewrites (metadata, covers, protection) keep the UUID and all pages, so positions stay valid.

## Conventions

- Base URL: `/api/v1`. Every other path serves the web interface (unknown paths get `index.html`). JSON in and out unless stated otherwise.
- **Numbers are zero-based indices as in the file:** pages, chunks (images), aux records
  (attachments), previews (covers). Attachments are chunks too, so page *n* is not chunk *n* — use
  `pages[n].chunks[k].index`.
- Times are Unix seconds.
- Errors: `{"error": "<code>", "message": "<text>"}` with the HTTP status. Codes: `bad_request`,
  `unauthorized`, `bad_credentials`, `forbidden`, `not_found`, `exists`, `busy`, `missing` (410), `last_admin`,
  `different_copy`, `unreadable`, `damaged`, `unsupported`, `too_large`, `rewrite_failed`, `repair_failed`, `internal`.

### Authentication

`POST /auth/login` returns a token. Apps send `Authorization: Bearer <token>`. Browsers also get an
`HttpOnly` cookie `vnmf_session`; with the cookie, `POST`/`PUT`/`DELETE` requests must carry the header
`X-VNMF-CSRF: 1`. Everything except `GET /server` and `/auth/login` requires signing in. Actions marked
**admin** require an administrator.

### Binary data and caching

Image, sound and cover endpoints return the bytes exactly as stored, with:

- `Content-Type` detected from the bytes; `ETag` (CRC + length); `Accept-Ranges: bytes` and `Range`
  support (206 / 416) — audio players seek with it;
- `X-VNMF-CRC: ok | bad` — `bad` means the data failed its checksum (sent anyway: a damaged page is
  better than none; offer a check);
- `Cache-Control: private, no-cache` (revalidate with `If-None-Match` → 304), **or** a year and
  `immutable` when the URL carries `?v=<version>` equal to the book's current `version` (from the book
  summary or `info`). The version changes whenever the file does, so versioned URLs never go stale.

## Endpoints

### Server and account

| | |
|---|---|
| `GET /server` | name, version, API and format versions, `has_users`, limits, features — no sign-in |
| `POST /auth/login` | `{username, password, label?}` → `{token, user}` (+ cookie) |
| `POST /auth/logout` | ends this session |
| `GET /me` | `{id, name, is_admin, avatar}` — `avatar`: the picture's address or `null` |
| `PUT /me/password` | `{old_password, new_password}` — ends the user's other sessions |
| `PUT /me` | `{display_name}` — the nickname others see (up to 64 characters; empty or `null` — the login is shown). The login (`name`) doesn't change |
| `GET /me/stats` | `{reading, finished, pages_read, bookmarks, last_read_at}` |
| `GET /me/sessions` | where the user is signed in: `{items: [{id, current, label, created_at, last_used_at}]}` |
| `DELETE /me/sessions/{id}` | signs that device out |
| `DELETE /me/sessions` | signs out everywhere except this session → `{ended}` |
| `PUT /me/avatar` | the body is a PNG, JPEG, WebP, GIF or AVIF image up to 1 MiB (the web interface sends a 256×256 square) |
| `DELETE /me/avatar` | |
| `GET /users/{id}/avatar` | any signed-in user; `user.avatar` holds the versioned address (cached for good) |
| `GET`/`PUT /me/settings` | the user's reader settings: any JSON object up to 64 KiB, stored as is |
| `GET /users` **admin** | users with `last_seen_at` |
| `POST /users` **admin** | `{name, password, is_admin?}` |
| `PUT /users/{id}` **admin** | `{password?, is_admin?}` — a new password ends their sessions; the last administrator can't lose the rights (`409 last_admin`), one's own can be given up while there's another |
| `DELETE /users/{id}` **admin** | with their progress and bookmarks |

Passwords: at least 8 characters. Names: 1–64 letters, digits, `.`, `_`, `-` (case-insensitive).

### Library

| | |
|---|---|
| `GET /library` | counts (books, missing, bytes, pages, damaged, not fully checked, protected, `oldest_full_check`), background job state, `verify_interval_days` (`null` — off); folders for admins |
| `POST /library/scan` **admin** | rescan in the background — 202, or 409 if running |
| `POST /library/verify` **admin** | full integrity check of every book in the background (CRC of all data, redundancy) |
| `GET /library/duplicates` **admin** | books in several files (same UUID): `{groups: [{uuid, title, copies: [{id, path, size, managed, writable, missing, added_at, readers}]}]}`; `GET /library` counts them in `duplicates` (and `duplicate_bytes` — the size of the extra copies); `books` counts each book once |
| `GET /books` | list (below) |
| `GET /books/facets` | values to filter by with the number of books: genres, tags, authors, kinds, statuses, languages, countries, age ratings, publications, demographics, colors, translations; `ranges` — min / max of chapters, pages, year |
| `GET /series` | books grouped by title (one file per volume): `{title, count, book_ids, page_count}` |
| `GET /books/{id}` | summary + `info` (below) + `copies`: other files with the same UUID, `[{id, path (admins), size, managed, writable, missing, same_size}]` |
| `DELETE /books/{id}` **admin** | deletes the file and the book; a missing book is just forgotten |
| `DELETE /books/{id}/copies/{other}` **admin** | deletes the copy `other` of the book if the files are byte-identical (`409 different_copy` otherwise; a missing copy is just forgotten). Every user's progress (the newer wins), bookmarks and lists move to `{id}` |

`GET /books` query:

- `kind` is the metadata's type, else inferred: a genre or tag naming a type ("Manga", "Манхва"),
  the country of origin (JP / KR / CN), the webtoon flag;
- `q` — all words must match titles, alternative titles, credits, genres, tags, characters,
  publisher, ISBN;
- lists, comma-separated: `genre`, `tag` (the book has **all** of them); `kind`, `status`,
  `language`, `country` (ISO code), `age`, `publication`, `demographic`, `color`, `translation`
  (official / fan / machine) — **any** of them;
- ranges (inclusive): `chapters_min` / `chapters_max`, `pages_min` / `pages_max`, `year_min` /
  `year_max` (the year of `start_date`, otherwise `release_date`);
- `series` (exact title), `webtoon`, `health` (`ok` / `damaged` / `unchecked`), `reading`
  (`unread` / `reading` / `finished`), `missing` (include missing), `copies` (list every file of
  a book; by default copies — files with the same UUID — are listed once: the one the user reads,
  else an uploaded one, else the first found);
- `sort` (`title` — natural order, `added`, `updated`, `size`, `pages`, `read` — last read first),
  `order` (`asc` / `desc`), `limit` (≤ 500, default 50), `offset`.

Response: `{total, offset, limit, items: [summary]}`.

A **book summary**:

```json
{
  "id": 1, "uuid": "…", "title": "…", "authors": ["…"], "kind": "manga", "status": "ongoing",
  "language": "uk", "genres": [], "tags": [], "page_count": 180, "volume_count": 2, "chapter_count": 12,
  "webtoon": false, "rtl": true, "has_music": true, "has_sfx": true, "has_text": false, "has_dark": false,
  "redundancy": 10, "format_version": "2.0", "size": 123456789, "version": "75bcd15-…",
  "managed": true, "writable": true, "missing": false, "recovered": false,
  "health": {"status": "ok", "issues": [], "full": true, "checked_at": 1790000000},
  "added_at": 1790000000, "updated_at": 1790000000,
  "progress": {"page": 42, "chunk": null, "offset": null, "finished": false, "updated_at": 1790000000},
  "cover_url": "/api/v1/books/1/cover?v=75bcd15-…"
}
```

`health` is `null` before the first check; `progress` is the signed-in user's.

### Uploading **admin**

`POST /books` with either a raw body (`Content-Type: application/octet-stream`, name in `?filename=`)
or `multipart/form-data` with one or more files. The type is detected by contents:

- `.vnmf` — checked and moved into the library as is (named after its title). A file whose UUID is
  already in the library is refused with 409 `exists`, unless `?replace=true` (the book's file is
  replaced, progress kept).
- CBZ / ZIP — converted like `vnmfpack` does: volume and chapter numbers from names (`Vol.2 Ch.10.cbz`),
  metadata from `vnmf.json` / `ComicInfo.xml` inside, page roles from ComicInfo, attachments from `aux/`.

Options (query): `combine` (all files → one book; each CBZ a chapter; images and metadata files
allowed), `webtoon`, `mixed_formats`, `rtl`, `redundancy` (0–30 %), `volume`, `cover` (`first` /
`none`), `replace`.

Response 201: `{books: [summary], errors: [{file, error, message}], warnings: []}`. If nothing could
be imported, the first error is returned instead.

### Reading

| | |
|---|---|
| `GET /books/{id}/manifest` | `{id, info, contents, pages, aux, covers}` — everything to start reading |
| `GET /books/{id}/contents` | volumes → chapters with titles, dates, ISBN, first page, page count |
| `GET /books/{id}/pages` | `{pages}` |
| `GET /books/{id}/groups?double&cover_alone&rtl` | display groups for paged mode (see below) |
| `GET /books/{id}/pages/{n}/image?part=k` | image `k` of page `n` (default 0) |
| `GET /books/{id}/chunks/{c}` | chunk `c` |
| `GET /books/{id}/chunks/{c}/tiles/{t}` | tile `t` of a chunk taller than 32767 px (browsers don't decode such images): the manifest gives such a chunk `tiles` — that many JPEG tiles of 8192 px, the last shorter. Cut on the first request, then cached |
| `GET /books/{id}/pages/{n}/dark/{k}` | dark version of image `k` of page `n` |
| `GET /books/{id}/pages/{n}/playback` | what plays and what's shown on the page (below) |
| `GET /books/{id}/pages/{n}/text` | `{page, text, layers: [{aux, content}]}` |
| `GET /books/{id}/strip-sfx` | strip sound effects with trigger points |
| `GET /books/{id}/aux` | the attachment table |
| `GET /books/{id}/aux/{k}` | attachment bytes |
| `GET /books/{id}/cover` | the main cover (the first preview, otherwise the first page) |
| `GET /books/{id}/covers` | previews `{index, volume, width, height, mime}`; volume 0 — the book |
| `GET /books/{id}/covers/{i}` | preview bytes |
| `GET /books/{id}/metadata` | `{schema, info_format, metadata}` — `vnmf-metadata/1` (ComicInfo converted) or `null` |
| `GET /books/{id}/info` | the Info section exactly as stored |
| `GET /books/{id}/comicinfo.xml` | ComicInfo generated from the metadata |
| `GET /books/{id}/file` | the `.vnmf` itself (resumable) |
| `GET /books/{id}/export?format=cbz\|zip\|vnmf` | `cbz` — images + ComicInfo for other readers; `zip` — the full `vnmfunpack` layout (attachments, covers, `vnmf.json`) that `vnmfpack` packs back losslessly |

**`info`** — the file itself: `uuid`, `title`, `version`, `format_version`, `legacy` (1.x),
`webtoon`, `rtl`, `page_count`, `chunk_count`, `aux_count`, `preview_count`, `has_volumes`,
`volume_count`, `chapter_count`, `has_metadata`, `info_format`, `has_music` / `has_sfx` / `has_text` /
`has_dark` / `has_attachments`, header `flags`, `block_size`, `redundancy`,
`parity: {bytes, max_burst_bytes, shard_size}`, `tail_copy`, `recovered`, `file_size`.

**A page**: `index`, `width`, `height`, `role` (`Normal`, `FrontCover`, `BackCover`, `TitlePage`,
`SpreadLeft`, `SpreadRight`, `SpreadSingle`, `Credits`, `TableOfContents`, `OmakeBonus`,
`Advertisement`), `volume`, `chapter`, `number_in_chapter`, `chapter_start`, `volume_start`, `flags`,
`chunks: [{index, width, height, format, mime}]` (top to bottom; a webtoon episode has many), `aux`
(its attachment indices), `has_music`, `has_sfx`, `has_text`, `dark_parts`.

**An attachment**: `index`, `kind` (`music`, `sfx`, `text`, `dark`, `attachment`), `page`, `format`,
`mime`, `size`, `looped`, `flags`, `gain` (0–1, times the master volume), `delay_ms`, `duration_ms`
(0 — unlimited), `pages` (music: on how many pages from its own; 0 — until it changes), `trigger_y`
(strip effect: page line in px), `part` (dark version: which image).

The playback rules are the same as in the desktop and Android readers:

- **Music** of a page is the last music from the start of its chapter up to it, so it continues until
  another one; music with `pages = N` stops after N pages. `playback.music` is what should be playing.
- **Effects** play when the page is opened (`playback.sfx`), or in a strip when the trigger line comes
  on screen: `strip-sfx` gives `{aux, chunk, fraction}` — the fragment and the fraction of its height.
- **Dark versions** replace page images (`dark_parts`).
- **Text layers** are plain UTF-8 or `vnmf-text/1` JSON with regions (`x, y, w, h, text`) for overlays;
  `text` joins them as plain text.

**Groups** (`/groups`): `double` (spreads, default `true`), `cover_alone` (the first page and volume
covers alone, default `true`), `rtl` (default — the book's direction). Each group is
`{pages: [reading order], visual: [left to right]}`. A spread set by the file is always together;
wide pages and covers stay alone; pairs never cross chapters.

### Changing books **admin**

Only books the server may change (`writable`). A change rewrites the file next to itself and renames
it into place: UUID, pages, attachments and structure are kept. A file with damaged data is not
rewritten (422 `rewrite_failed`) — repair it first. One change per book at a time (409 `busy`).
Each returns the updated summary.

| | |
|---|---|
| `PUT /books/{id}/metadata` | `vnmf-metadata/1` JSON (`schema` may be omitted) or ComicInfo XML. Without `contents`, volume/chapter titles are kept; `reading_direction` also sets the direction of every page |
| `POST /books/{id}/covers?volume=N&replace=true` | the body is an image; a book cover (volume 0) becomes the main one |
| `DELETE /books/{id}/covers/{i}` | |
| `POST /books/{id}/protect` | `{redundancy?: 0–30, tail_copy?: bool, block_size?: 4096\|8192\|16384\|32768}` |

### Integrity

| | |
|---|---|
| `GET /books/{id}/health?full=true` | `{ok, issues, prognosis, full, damaged_chunks, repairable, checked_at}`; without `full` only both metadata copies are checked (instant), with it every chunk and the redundancy. Stored in the index |
| `POST /books/{id}/repair` **admin** | restores the metadata copies and repairs data with Reed–Solomon (like `vnmfrecovery`). `{outcome: intact\|repaired\|partial, replaced, damaged_left, log, book}`. A partial result is discarded unless `?accept_partial=true`. Extracting what survived of a hopeless file is left to `vnmfrecovery --mode carve` |

### Reading lists

Per user. Five basic lists always exist — `reading`, `planned`, `completed`, `on_hold`, `dropped` —
and a book is in at most one of them (adding it to one takes it out of the others). Any number of
the user's own lists can be added; a book can be in several. A basic list's `name` is `null` until
renamed (clients show the default name in their language).

| | |
|---|---|
| `GET /lists` | `{items: [{id, kind, name, position, count}], in_lists, library}` |
| `POST /lists` | `{name}` — a new list of the user's own |
| `PUT /lists/{id}` | `{name}`; for a basic list `null` brings the default name back |
| `DELETE /lists/{id}` | own lists only; the books stay in the library |
| `PUT /lists/order` | `{ids}` — the new order |
| `GET /books/{id}/lists` | `{lists: [ids]}` — the user's lists that have the book |
| `PUT` / `DELETE /books/{id}/lists/{list}` | add / remove → `{lists}` |

The catalog shows a list's books with `GET /books?list=<id>` (`list=any` — in any list).

### Progress and bookmarks

| | |
|---|---|
| `GET /books/{id}/progress` | the position or `null` |
| `PUT /books/{id}/progress` | `{page, chunk?, offset?, finished?, updated_at?}` → the stored position |
| `DELETE /books/{id}/progress` | mark unread |
| `GET /progress?limit&finished` | "continue reading": summaries, last read first |
| `GET /books/{id}/bookmarks` | `{items: [{id, book_id, page, chunk, offset, note, created_at}]}` |
| `POST /books/{id}/bookmarks` | `{page, chunk?, offset?, note?}` |
| `DELETE /bookmarks/{id}` | |

In strip reading a position is the fragment (`chunk`) and the fraction of its height at the top of
the screen (`offset`, 0–1). Offline clients send `updated_at` (when the position was reached): a
position older than the stored one is not applied — the stored one is returned, so the client can
jump to where the user got on another device. A position also has `furthest`: the furthest page
reached (the last page once finished) — it never goes back, so chapters before it stay read.

Progress also moves the book between the basic lists: starting it puts it in "reading" (from no
basic list or "planned"), finishing — in "completed" (also from "reading"); on hold and dropped
books stay. `DELETE …/progress` takes it out of "reading" / "completed". Putting a book in
"completed" by hand marks it read. `GET /me/stats` counts `reading` / `finished` by those lists. A book whose translation
(`translation.status`, else the series' `status`) is still coming out — `ongoing`, `hiatus`,
`upcoming` — stays in "reading" when finished: the reader caught up. When a file gets new pages,
readers who had finished it are back to unfinished and in "reading".

## Not here (yet)

- Thumbnails: covers are served as stored (previews are meant to be small); the server never decodes images.
- OPDS and compatibility layers for other apps; source plugins (downloading from sites).
- Extracting surviving data from a file beyond repair — `vnmfrecovery`.
