// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! The API end to end: requests go through the real router (no network), files are generated here.

use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use vnmf_core::{AuxFlags, AuxInput, AuxKind, Builder, ChunkSource, PageRole};
use vnmf_server::{AppState, Config, auth};

fn png_bytes(w: u32, h: u32, seed: u8, padding: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend(w.to_be_bytes());
    bytes.extend(h.to_be_bytes());
    bytes.extend([8, 2, 0, 0, 0]);
    bytes.extend(std::iter::repeat_n(seed, padding.max(1)));
    bytes
}

fn png(w: u32, h: u32, seed: u8) -> ChunkSource {
    ChunkSource::from_bytes(png_bytes(w, h, seed, 3000)).unwrap()
}

fn ogg(tag: &[u8]) -> ChunkSource {
    ChunkSource::from_bytes([b"OggS".as_slice(), tag].concat()).unwrap()
}

struct Env {
    _dir: TempDir,
    data: PathBuf,
    extra: PathBuf,
    state: AppState,
    app: Router,
    admin: String,
    reader: String,
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("vnmf-server-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = root.join("data");
    let extra = root.join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    let mut config = Config::new(data.clone());
    config.libraries = vec![extra.clone()];
    let state = AppState::new(config).unwrap();
    let (admin, reader) = state
        .db
        .blocking(|c| {
            let a = auth::create_user(c, "admin", "admin-password", true)?;
            let r = auth::create_user(c, "reader", "reader-password", false)?;
            Ok((auth::create_session(c, a, None)?, auth::create_session(c, r, None)?))
        })
        .unwrap();
    let app = vnmf_server::app(state.clone());
    Env { _dir: TempDir(root), data, extra, state, app, admin, reader }
}

/// A book with everything the format has: volumes, roles, metadata, music, effects, text, a dark
/// version, covers and redundancy.
fn rich_book(path: &Path) {
    let mut b = Builder::new().with_uuid([0x42; 16]).with_redundancy(10).with_tail_duplicate(true);
    b.set_info(
        serde_json::to_vec(&json!({
            "schema": "vnmf-metadata/1",
            "title": "Test Saga",
            "genres": ["Fantasy", "Comedy"],
            "tags": ["cats"],
            "credits": [{"name": "Author Person", "roles": ["story", "art"]}],
            "reading_direction": "rtl",
            "contents": {"volumes": [{"volume": 1, "title": "Beginning", "chapters": [{"chapter": 1, "title": "Rain"}]}]}
        }))
        .unwrap(),
    );
    b.set_volume(1);
    b.add_page(png(800, 1200, 1), PageRole::FrontCover, 1, 1, vnmf_core::PageBitFlags::READING_RTL).unwrap();
    let mut music = AuxInput::new(AuxKind::Music, ogg(b"rain"));
    music.flags = AuxFlags::LOOP;
    music.duration_ms = 9000;
    b.attach_aux(music).unwrap();
    b.add_page(png(800, 1200, 2), PageRole::Normal, 1, 2, vnmf_core::PageBitFlags::READING_RTL).unwrap();
    b.attach_aux(AuxInput::new(AuxKind::Sfx, ogg(b"thunder"))).unwrap();
    b.attach_aux(AuxInput::new(
        AuxKind::Text,
        ChunkSource::from_bytes(br#"{"schema": "vnmf-text/1", "regions": [{"x": 1, "y": 2, "w": 3, "h": 4, "text": "Grr"}]}"#.to_vec()).unwrap(),
    ))
    .unwrap();
    b.attach_aux(AuxInput::new(AuxKind::DarkImage, png(800, 1200, 9))).unwrap();
    b.add_page(png(800, 1200, 3), PageRole::Normal, 1, 3, vnmf_core::PageBitFlags::READING_RTL).unwrap();
    b.set_volume(2);
    b.add_page(png(800, 1200, 4), PageRole::Normal, 1, 1, vnmf_core::PageBitFlags::READING_RTL).unwrap();
    b.add_preview(png(300, 400, 7), 0, 0).unwrap();
    b.add_preview(png(300, 400, 8), 2, 0).unwrap();
    b.build(path).unwrap();
}

fn webtoon_book(path: &Path) {
    let mut b = Builder::new();
    b.add_webtoon(vec![png(800, 1000, 1), png(800, 1000, 2), png(800, 1000, 3)], 1, 1, 0).unwrap();
    let mut sfx = AuxInput::new(AuxKind::Sfx, ogg(b"door"));
    sfx.trigger_y = 1500;
    b.attach_aux(sfx).unwrap();
    b.build(path).unwrap();
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON ({}): {}", self.status, String::from_utf8_lossy(&self.body)))
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    Resp { status, headers, body }
}

fn req(method: &str, uri: &str, token: Option<&str>) -> axum::http::request::Builder {
    let b = Request::builder().method(method).uri(uri);
    match token {
        Some(t) => b.header(header::AUTHORIZATION, format!("Bearer {t}")),
        None => b,
    }
}

async fn get(app: &Router, uri: &str, token: &str) -> Resp {
    send(app, req("GET", uri, Some(token)).body(Body::empty()).unwrap()).await
}

async fn json_req(app: &Router, method: &str, uri: &str, token: &str, body: Value) -> Resp {
    send(app, req(method, uri, Some(token)).header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap()).await
}

async fn upload(app: &Router, uri: &str, token: &str, bytes: Vec<u8>) -> Resp {
    send(app, req("POST", uri, Some(token)).header(header::CONTENT_TYPE, "application/octet-stream").body(Body::from(bytes)).unwrap()).await
}

/// Uploads the rich book; returns its id.
async fn upload_rich(e: &Env) -> i64 {
    let path = e.data.join("rich.vnmf");
    rich_book(&path);
    let r = upload(&e.app, "/api/v1/books?filename=rich.vnmf", &e.admin, std::fs::read(&path).unwrap()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&r.body));
    r.json()["books"][0]["id"].as_i64().unwrap()
}

#[tokio::test]
async fn sign_in_and_access_rules() {
    let e = env("auth");
    let r = send(&e.app, req("GET", "/api/v1/books", None).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(&e.app, req("GET", "/api/v1/server", None).body(Body::empty()).unwrap()).await;
    assert_eq!((r.status, r.json()["has_users"].as_bool()), (StatusCode::OK, Some(true)));

    let bad = send(
        &e.app,
        req("POST", "/api/v1/auth/login", None)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"reader","password":"nope"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    let ok = send(
        &e.app,
        req("POST", "/api/v1/auth/login", None)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"READER","password":"reader-password","label":"test"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK);
    let token = ok.json()["token"].as_str().unwrap().to_string();
    assert!(ok.headers.get(header::SET_COOKIE).unwrap().to_str().unwrap().contains("HttpOnly"));
    assert_eq!(get(&e.app, "/api/v1/me", &token).await.json()["name"], "reader");

    // The cookie works for reading; a changing request also needs the CSRF header.
    let cookie = format!("vnmf_session={token}");
    let r = send(&e.app, Request::get("/api/v1/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK);
    let put = |csrf: bool| {
        let mut b = Request::put("/api/v1/me/settings").header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json");
        if csrf {
            b = b.header("x-vnmf-csrf", "1");
        }
        b.body(Body::from(r#"{"theme":"dark"}"#)).unwrap()
    };
    assert_eq!(send(&e.app, put(false)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(send(&e.app, put(true)).await.status, StatusCode::OK);
    assert_eq!(get(&e.app, "/api/v1/me/settings", &token).await.json()["theme"], "dark");

    // Readers can't upload or manage users.
    assert_eq!(upload(&e.app, "/api/v1/books", &e.reader, b"x".to_vec()).await.status, StatusCode::FORBIDDEN);
    assert_eq!(get(&e.app, "/api/v1/users", &e.reader).await.status, StatusCode::FORBIDDEN);
    let users = get(&e.app, "/api/v1/users", &e.admin).await.json();
    assert_eq!(users["users"].as_array().unwrap().len(), 2);

    // Logout ends the session.
    let r = send(&e.app, req("POST", "/api/v1/auth/logout", Some(&token)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(get(&e.app, "/api/v1/me", &token).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn upload_list_and_read_everything() {
    let e = env("read");
    let id = upload_rich(&e).await;

    // List, search, filters, facets.
    let list = get(&e.app, "/api/v1/books?q=saga", &e.reader).await.json();
    assert_eq!(list["total"], 1);
    let book = &list["items"][0];
    assert_eq!((book["title"].as_str(), book["rtl"].as_bool(), book["has_music"].as_bool()), (Some("Test Saga"), Some(true), Some(true)));
    assert_eq!(book["authors"], json!(["Author Person"]));
    assert_eq!(book["health"]["status"], "ok");
    assert_eq!(get(&e.app, "/api/v1/books?q=nothing+like+this", &e.reader).await.json()["total"], 0);
    assert_eq!(get(&e.app, "/api/v1/books?genre=fantasy", &e.reader).await.json()["total"], 1);
    assert_eq!(get(&e.app, "/api/v1/books?reading=reading", &e.reader).await.json()["total"], 0);
    let facets = get(&e.app, "/api/v1/books/facets", &e.reader).await.json();
    assert_eq!(facets["genres"].as_array().unwrap().len(), 2);
    assert_eq!(get(&e.app, "/api/v1/series", &e.reader).await.json()["items"][0]["count"], 1);

    // The manifest: info, structure, pages, attachments, covers.
    let m = get(&e.app, &format!("/api/v1/books/{id}/manifest"), &e.reader).await.json();
    assert_eq!(m["info"]["uuid"], "42424242-4242-4242-4242-424242424242");
    assert_eq!((m["info"]["page_count"].as_u64(), m["info"]["redundancy"].as_u64()), (Some(4), Some(10)));
    assert!(m["info"]["parity"]["max_burst_bytes"].as_u64().unwrap() > 0);
    assert_eq!(m["contents"]["volumes"][0]["title"], "Beginning");
    assert_eq!(m["contents"]["volumes"][0]["chapters"][0]["title"], "Rain");
    assert_eq!(m["contents"]["volumes"].as_array().unwrap().len(), 2);
    assert_eq!(m["pages"][0]["role"], "FrontCover");
    assert_eq!(m["pages"][1]["dark_parts"], json!([0]));
    assert_eq!(m["pages"][3]["volume_start"], true);
    assert_eq!(m["aux"].as_array().unwrap().len(), 4);
    assert_eq!(m["covers"][1]["volume"], 2);

    // Image bytes as stored, with caching and ranges.
    let version = m["info"]["version"].as_str().unwrap().to_string();
    // Attachments are chunks too, so page 1's image isn't chunk 1.
    let c1 = m["pages"][1]["chunks"][0]["index"].as_u64().unwrap();
    assert_ne!(c1, 1);
    let r = get(&e.app, &format!("/api/v1/books/{id}/pages/1/image"), &e.reader).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, png_bytes(800, 1200, 2, 3000));
    assert_eq!(r.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(r.headers["x-vnmf-crc"], "ok");
    assert_eq!(r.headers[header::CACHE_CONTROL], "private, no-cache");
    let etag = r.headers[header::ETAG].to_str().unwrap().to_string();
    let r =
        send(&e.app, req("GET", &format!("/api/v1/books/{id}/chunks/{c1}"), Some(&e.reader)).header(header::IF_NONE_MATCH, &etag).body(Body::empty()).unwrap())
            .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    let r = send(
        &e.app,
        req("GET", &format!("/api/v1/books/{id}/chunks/{c1}?v={version}"), Some(&e.reader)).header(header::RANGE, "bytes=0-7").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!((r.status, r.body.as_slice()), (StatusCode::PARTIAL_CONTENT, b"\x89PNG\r\n\x1a\n".as_slice()));
    assert!(r.headers[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/chunks/99"), &e.reader).await.status, StatusCode::NOT_FOUND);

    // Paged mode: the cover alone, spreads don't cross volumes, right to left.
    let g = get(&e.app, &format!("/api/v1/books/{id}/groups"), &e.reader).await.json();
    assert_eq!(g["rtl"], true);
    assert_eq!(g["groups"].as_array().unwrap().iter().map(|g| g["pages"].clone()).collect::<Vec<_>>(), vec![json!([0]), json!([1, 2]), json!([3])]);
    assert_eq!(g["groups"][1]["visual"], json!([2, 1]));

    // Sound, text and the dark version.
    let p = get(&e.app, &format!("/api/v1/books/{id}/pages/1/playback"), &e.reader).await.json();
    assert_eq!((p["music"]["kind"].as_str(), p["music"]["looped"].as_bool(), p["music"]["duration_ms"].as_u64()), (Some("music"), Some(true), Some(9000)));
    assert_eq!(p["sfx"][0]["mime"], "audio/ogg");
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/pages/3/playback"), &e.reader).await.json()["music"], Value::Null);
    let sfx = p["sfx"][0]["index"].as_u64().unwrap();
    let r = get(&e.app, &format!("/api/v1/books/{id}/aux/{sfx}"), &e.reader).await;
    assert_eq!((r.body.as_slice(), r.headers[header::CONTENT_TYPE].to_str().unwrap()), (b"OggSthunder".as_slice(), "audio/ogg"));
    let t = get(&e.app, &format!("/api/v1/books/{id}/pages/1/text"), &e.reader).await.json();
    assert_eq!(t["text"], "Grr");
    assert_eq!(t["layers"][0]["content"]["regions"][0]["w"], 3);
    let r = get(&e.app, &format!("/api/v1/books/{id}/pages/1/dark/0"), &e.reader).await;
    assert_eq!(r.body, png_bytes(800, 1200, 9, 3000));
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/pages/0/dark/0"), &e.reader).await.status, StatusCode::NOT_FOUND);

    // Covers, metadata, ComicInfo, the file itself.
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/cover"), &e.reader).await.body, png_bytes(300, 400, 7, 3000));
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/covers/1"), &e.reader).await.body, png_bytes(300, 400, 8, 3000));
    let meta = get(&e.app, &format!("/api/v1/books/{id}/metadata"), &e.reader).await.json();
    assert_eq!((meta["info_format"].as_str(), meta["metadata"]["title"].as_str()), (Some("json"), Some("Test Saga")));
    let ci = get(&e.app, &format!("/api/v1/books/{id}/comicinfo.xml"), &e.reader).await;
    assert!(String::from_utf8_lossy(&ci.body).contains("<Series>Test Saga</Series>"));
    let f = get(&e.app, &format!("/api/v1/books/{id}/file"), &e.reader).await;
    assert_eq!(&f.body[..4], b"vnmf");
    assert!(f.headers[header::CONTENT_DISPOSITION].to_str().unwrap().contains("Test Saga.vnmf"));

    // Exports: CBZ for other readers, ZIP with everything.
    let cbz = get(&e.app, &format!("/api/v1/books/{id}/export?format=cbz"), &e.reader).await;
    assert_eq!((cbz.status, &cbz.body[..2]), (StatusCode::OK, b"PK".as_slice()));
    let names = zip_names(&cbz.body);
    assert!(names.iter().any(|n| n == "ComicInfo.xml") && !names.iter().any(|n| n.starts_with("aux/")));
    let full = get(&e.app, &format!("/api/v1/books/{id}/export?format=zip"), &e.reader).await;
    let names = zip_names(&full.body);
    assert!(names.iter().any(|n| n.starts_with("aux/")) && names.iter().any(|n| n.starts_with("previews/")) && names.iter().any(|n| n == "vnmf.json"));

    // Quick and full checks.
    let h = get(&e.app, &format!("/api/v1/books/{id}/health?full=true"), &e.reader).await.json();
    assert_eq!((h["ok"].as_bool(), h["full"].as_bool(), h["repairable"].as_bool()), (Some(true), Some(true), Some(true)));
}

fn zip_names(bytes: &[u8]) -> Vec<String> {
    // A minimal central-directory walk: enough to list names without another dependency.
    let mut names = Vec::new();
    let mut i = 0;
    while i + 46 <= bytes.len() {
        if bytes[i..i + 4] == *b"PK\x01\x02" {
            let len = u16::from_le_bytes([bytes[i + 28], bytes[i + 29]]) as usize;
            let extra = u16::from_le_bytes([bytes[i + 30], bytes[i + 31]]) as usize;
            let comment = u16::from_le_bytes([bytes[i + 32], bytes[i + 33]]) as usize;
            names.push(String::from_utf8_lossy(&bytes[i + 46..i + 46 + len]).into_owned());
            i += 46 + len + extra + comment;
        } else {
            i += 1;
        }
    }
    names
}

#[tokio::test]
async fn webtoon_strip_effects() {
    let e = env("webtoon");
    let path = e.data.join("wt.vnmf");
    webtoon_book(&path);
    let r = upload(&e.app, "/api/v1/books?filename=episode.vnmf", &e.admin, std::fs::read(&path).unwrap()).await;
    let id = r.json()["books"][0]["id"].as_i64().unwrap();
    let m = get(&e.app, &format!("/api/v1/books/{id}/manifest"), &e.reader).await.json();
    assert_eq!((m["info"]["webtoon"].as_bool(), m["pages"][0]["chunks"].as_array().unwrap().len()), (Some(true), 3));
    let s = get(&e.app, &format!("/api/v1/books/{id}/strip-sfx"), &e.reader).await.json();
    assert_eq!((s["sfx"][0]["chunk"].as_u64(), s["sfx"][0]["fraction"].as_f64()), (Some(1), Some(0.5)));
    let r = get(&e.app, &format!("/api/v1/books/{id}/pages/0/image?part=2"), &e.reader).await;
    assert_eq!(r.body, png_bytes(800, 1000, 3, 3000));
}

#[tokio::test]
async fn progress_bookmarks_and_sync() {
    let e = env("progress");
    let id = upload_rich(&e).await;
    let uri = format!("/api/v1/books/{id}/progress");
    assert_eq!(get(&e.app, &uri, &e.reader).await.json(), Value::Null);
    let r = json_req(&e.app, "PUT", &uri, &e.reader, json!({ "page": 2, "updated_at": 1_000 })).await;
    assert_eq!(r.json()["page"], 2);
    // An older position from an offline device doesn't overwrite a newer one.
    let r = json_req(&e.app, "PUT", &uri, &e.reader, json!({ "page": 1, "updated_at": 500 })).await;
    assert_eq!(r.json()["page"], 2);
    assert_eq!(json_req(&e.app, "PUT", &uri, &e.reader, json!({ "page": 10 })).await.status, StatusCode::BAD_REQUEST);
    // Progress is per user.
    assert_eq!(get(&e.app, &uri, &e.admin).await.json(), Value::Null);
    let recent = get(&e.app, "/api/v1/progress", &e.reader).await.json();
    assert_eq!(recent["items"][0]["progress"]["page"], 2);
    assert_eq!(get(&e.app, "/api/v1/books?reading=reading", &e.reader).await.json()["total"], 1);

    let b = json_req(&e.app, "POST", &format!("/api/v1/books/{id}/bookmarks"), &e.reader, json!({ "page": 1, "note": "thunder!" })).await;
    assert_eq!(b.status, StatusCode::CREATED);
    let bid = b.json()["id"].as_i64().unwrap();
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/bookmarks"), &e.reader).await.json()["items"][0]["note"], "thunder!");
    // Someone else's bookmark can't be deleted.
    let r = send(&e.app, req("DELETE", &format!("/api/v1/bookmarks/{bid}"), Some(&e.admin)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(&e.app, req("DELETE", &format!("/api/v1/bookmarks/{bid}"), Some(&e.reader)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn edit_metadata_covers_and_protection_keep_identity() {
    let e = env("edit");
    let id = upload_rich(&e).await;
    json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/progress"), &e.reader, json!({ "page": 3 })).await;

    let uri = format!("/api/v1/books/{id}/metadata");
    assert_eq!(json_req(&e.app, "PUT", &uri, &e.reader, json!({ "title": "X" })).await.status, StatusCode::FORBIDDEN);
    let r = json_req(&e.app, "PUT", &uri, &e.admin, json!({ "title": "Renamed Saga", "genres": ["Drama"], "reading_direction": "ltr" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!((r.json()["title"].as_str(), r.json()["rtl"].as_bool()), (Some("Renamed Saga"), Some(false)));
    let m = get(&e.app, &format!("/api/v1/books/{id}/manifest"), &e.reader).await.json();
    assert_eq!(m["info"]["uuid"], "42424242-4242-4242-4242-424242424242");
    // Chapter titles were kept (no `contents` in the request), the pages are the same.
    assert_eq!(m["contents"]["volumes"][0]["chapters"][0]["title"], "Rain");
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/pages/2/image"), &e.reader).await.body, png_bytes(800, 1200, 3, 3000));
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/progress"), &e.reader).await.json()["page"], 3);
    assert_eq!(get(&e.app, "/api/v1/books?genre=drama", &e.reader).await.json()["total"], 1);

    // ComicInfo.xml is accepted too.
    let xml = "<?xml version=\"1.0\"?><ComicInfo><Series>From XML</Series><Writer>W</Writer></ComicInfo>";
    let r = send(&e.app, req("PUT", &uri, Some(&e.admin)).header(header::CONTENT_TYPE, "application/xml").body(Body::from(xml)).unwrap()).await;
    assert_eq!(r.json()["title"], "From XML");

    // A new book cover becomes the main one; the volume cover stays.
    let cover = png_bytes(100, 150, 5, 100);
    let r = send(&e.app, req("POST", &format!("/api/v1/books/{id}/covers"), Some(&e.admin)).body(Body::from(cover.clone())).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/cover"), &e.reader).await.body, cover);
    let covers = get(&e.app, &format!("/api/v1/books/{id}/covers"), &e.reader).await.json();
    assert_eq!(covers["covers"].as_array().unwrap().iter().map(|c| c["volume"].as_u64().unwrap()).collect::<Vec<_>>(), vec![0, 2]);
    let r = send(&e.app, req("DELETE", &format!("/api/v1/books/{id}/covers/1"), Some(&e.admin)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/covers"), &e.reader).await.json()["covers"].as_array().unwrap().len(), 1);
    let r = send(&e.app, req("POST", &format!("/api/v1/books/{id}/covers"), Some(&e.admin)).body(Body::from("not an image")).unwrap()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Protection.
    let r = json_req(&e.app, "POST", &format!("/api/v1/books/{id}/protect"), &e.admin, json!({ "redundancy": 25, "block_size": 8192 })).await;
    assert_eq!(r.json()["redundancy"], 25);
    let info = get(&e.app, &format!("/api/v1/books/{id}"), &e.reader).await.json();
    assert_eq!((info["info"]["block_size"].as_u64(), info["info"]["tail_copy"].as_bool()), (Some(8192), Some(true)));
    assert_eq!(json_req(&e.app, "POST", &format!("/api/v1/books/{id}/protect"), &e.admin, json!({ "redundancy": 31 })).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn damage_is_found_and_repaired() {
    let e = env("repair");
    let id = upload_rich(&e).await;
    let path = e.data.join("library").join("Test Saga.vnmf");
    let reader = vnmf_core::VnmfReader::open(&path).unwrap();
    let offset = reader.chunks()[2].offset() as usize;
    drop(reader);
    let good = std::fs::read(&path).unwrap();
    let mut bad = good.clone();
    for b in &mut bad[offset + 20..offset + 220] {
        *b ^= 0x5A;
    }
    std::fs::write(&path, &bad).unwrap();

    let r = get(&e.app, &format!("/api/v1/books/{id}/chunks/2"), &e.reader).await;
    assert_eq!(r.headers["x-vnmf-crc"], "bad");
    let h = get(&e.app, &format!("/api/v1/books/{id}/health?full=true"), &e.reader).await.json();
    assert_eq!((h["ok"].as_bool(), h["damaged_chunks"].clone(), h["repairable"].as_bool()), (Some(false), json!([2]), Some(true)));
    assert_eq!(get(&e.app, "/api/v1/books?health=damaged", &e.reader).await.json()["total"], 1);
    // A damaged file isn't rewritten (that would seal the damage with fresh checksums).
    assert_eq!(
        json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/metadata"), &e.admin, json!({ "title": "X" })).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let r = send(&e.app, req("POST", &format!("/api/v1/books/{id}/repair"), Some(&e.admin)).body(Body::empty()).unwrap()).await.json();
    assert_eq!((r["outcome"].as_str(), r["replaced"].as_bool()), (Some("repaired"), Some(true)), "{r}");
    assert_eq!(std::fs::read(&path).unwrap(), good);
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/chunks/2"), &e.reader).await.headers["x-vnmf-crc"], "ok");
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/health?full=true"), &e.reader).await.json()["ok"], true);
}

#[tokio::test]
async fn uploads_duplicates_cbz_and_deletion() {
    let e = env("upload");
    let id = upload_rich(&e).await;
    let bytes = std::fs::read(e.data.join("rich.vnmf")).unwrap();
    let dup = upload(&e.app, "/api/v1/books?filename=again.vnmf", &e.admin, bytes.clone()).await;
    assert_eq!((dup.status, dup.json()["error"].as_str()), (StatusCode::CONFLICT, Some("exists")));
    let rep = upload(&e.app, "/api/v1/books?filename=again.vnmf&replace=true", &e.admin, bytes).await;
    assert_eq!(rep.json()["books"][0]["id"].as_i64(), Some(id));
    assert_eq!(upload(&e.app, "/api/v1/books?filename=x.txt", &e.admin, b"hello".to_vec()).await.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // A CBZ with ComicInfo is converted: volume and chapter from the name, metadata, a cover.
    let cbz = {
        use vnmf::unpack::{Sink, ZipWriterSink};
        let mut sink = ZipWriterSink::new(std::io::Cursor::new(Vec::new()));
        sink.put("01.png", &png_bytes(600, 900, 1, 500), false).unwrap();
        sink.put("02.png", &png_bytes(600, 900, 2, 500), false).unwrap();
        sink.put("ComicInfo.xml", b"<?xml version=\"1.0\"?><ComicInfo><Series>Cbz Series</Series><Genre>Horror</Genre></ComicInfo>", true).unwrap();
        sink.into_inner().unwrap().into_inner()
    };
    let r = upload(&e.app, "/api/v1/books?filename=Vol.3%20Ch.7.cbz&redundancy=5", &e.admin, cbz).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&r.body));
    let book = &r.json()["books"][0];
    assert_eq!((book["title"].as_str(), book["page_count"].as_u64(), book["redundancy"].as_u64()), (Some("Cbz Series"), Some(2), Some(5)));
    let cid = book["id"].as_i64().unwrap();
    let m = get(&e.app, &format!("/api/v1/books/{cid}/manifest"), &e.reader).await.json();
    assert_eq!((m["contents"]["volumes"][0]["number"].as_u64(), m["contents"]["volumes"][0]["chapters"][0]["number"].as_str()), (Some(3), Some("7")));
    assert_eq!(m["info"]["preview_count"], 1);
    assert!(e.data.join("library").join("Cbz Series.vnmf").exists());

    // Several files in one multipart request, combined into one book.
    let boundary = "XyZ";
    let mut body = Vec::new();
    for (name, seed) in [("Ch.1.cbz", 1u8), ("Ch.2.cbz", 2)] {
        use vnmf::unpack::{Sink, ZipWriterSink};
        let mut sink = ZipWriterSink::new(std::io::Cursor::new(Vec::new()));
        sink.put("p.png", &png_bytes(600, 900, seed, 200), false).unwrap();
        let zip = sink.into_inner().unwrap().into_inner();
        body.extend(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/zip\r\n\r\n").as_bytes(),
        );
        body.extend(zip);
        body.extend(b"\r\n");
    }
    body.extend(format!("--{boundary}--\r\n").as_bytes());
    let r = send(
        &e.app,
        req("POST", "/api/v1/books?combine=true", Some(&e.admin))
            .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!((r.json()["books"][0]["chapter_count"].as_u64(), r.json()["books"][0]["page_count"].as_u64()), (Some(2), Some(2)));

    // Deleting removes the file and the book.
    let path = e.data.join("library").join("Cbz Series.vnmf");
    assert_eq!(send(&e.app, req("DELETE", &format!("/api/v1/books/{cid}"), Some(&e.reader)).body(Body::empty()).unwrap()).await.status, StatusCode::FORBIDDEN);
    assert_eq!(send(&e.app, req("DELETE", &format!("/api/v1/books/{cid}"), Some(&e.admin)).body(Body::empty()).unwrap()).await.status, StatusCode::NO_CONTENT);
    assert!(!path.exists());
    assert_eq!(get(&e.app, &format!("/api/v1/books/{cid}"), &e.reader).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scanned_library_is_read_only_and_moves_are_followed() {
    let e = env("scan");
    std::fs::create_dir_all(e.extra.join("sub")).unwrap();
    rich_book(&e.extra.join("sub/a.vnmf"));
    webtoon_book(&e.extra.join(".hidden.vnmf"));
    let st = e.state.clone();
    tokio::task::spawn_blocking(move || vnmf_server::library::scan(&st)).await.unwrap();
    let list = get(&e.app, "/api/v1/books", &e.reader).await.json();
    assert_eq!(list["total"], 1);
    let id = list["items"][0]["id"].as_i64().unwrap();
    assert_eq!((list["items"][0]["managed"].as_bool(), list["items"][0]["writable"].as_bool()), (Some(false), Some(false)));
    assert_eq!(json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/metadata"), &e.admin, json!({ "title": "X" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(send(&e.app, req("DELETE", &format!("/api/v1/books/{id}"), Some(&e.admin)).body(Body::empty()).unwrap()).await.status, StatusCode::FORBIDDEN);

    // Moved: same book id, progress kept.
    json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/progress"), &e.reader, json!({ "page": 1 })).await;
    std::fs::rename(e.extra.join("sub/a.vnmf"), e.extra.join("b.vnmf")).unwrap();
    let st = e.state.clone();
    tokio::task::spawn_blocking(move || vnmf_server::library::scan(&st)).await.unwrap();
    let list = get(&e.app, "/api/v1/books", &e.reader).await.json();
    assert_eq!((list["total"].as_u64(), list["items"][0]["id"].as_i64()), (Some(1), Some(id)));
    assert_eq!(list["items"][0]["progress"]["page"], 1);

    // Gone: kept as missing, 410 for its data.
    std::fs::remove_file(e.extra.join("b.vnmf")).unwrap();
    let st = e.state.clone();
    tokio::task::spawn_blocking(move || vnmf_server::library::scan(&st)).await.unwrap();
    assert_eq!(get(&e.app, "/api/v1/books", &e.reader).await.json()["total"], 0);
    assert_eq!(get(&e.app, "/api/v1/books?missing=true", &e.reader).await.json()["total"], 1);
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/manifest"), &e.reader).await.status, StatusCode::GONE);
    let status = get(&e.app, "/api/v1/library", &e.admin).await.json();
    assert_eq!((status["counts"]["missing"].as_u64(), status["scan"]["missing"].as_u64()), (Some(1), Some(1)));
    // A missing book can be forgotten.
    let r = send(&e.app, req("DELETE", &format!("/api/v1/books/{id}"), Some(&e.admin)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn web_interface_and_unknown_api_paths() {
    let e = env("web");
    for path in ["/", "/library", "/book/1/read"] {
        let r = send(&e.app, Request::get(path).body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
        assert!(r.headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"), "{path}");
        assert!(r.headers.contains_key(header::CONTENT_SECURITY_POLICY));
    }
    for path in ["/api/v1/nope", "/api/v2/books", "/api"] {
        let r = send(&e.app, req("GET", path, Some(&e.reader)).body(Body::empty()).unwrap()).await;
        assert_eq!((r.status, r.json()["error"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")), "{path}");
    }
}

#[tokio::test]
async fn account_sessions_rename_and_stats() {
    let e = env("account");
    let id = upload_rich(&e).await;
    // A second session of the same user.
    let login = || {
        req("POST", "/api/v1/auth/login", None)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"reader","password":"reader-password","label":"Phone"}"#))
            .unwrap()
    };
    let phone = send(&e.app, login()).await.json()["token"].as_str().unwrap().to_string();
    let list = get(&e.app, "/api/v1/me/sessions", &e.reader).await.json();
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let other = list["items"].as_array().unwrap().iter().find(|s| s["current"] == false).unwrap();
    assert_eq!(other["label"], "Phone");
    let sid = other["id"].as_str().unwrap();
    let r = send(&e.app, req("DELETE", &format!("/api/v1/me/sessions/{sid}"), Some(&e.reader)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(get(&e.app, "/api/v1/me", &phone).await.status, StatusCode::UNAUTHORIZED);
    // Someone else's session id can't be ended.
    let admin_sid = get(&e.app, "/api/v1/me/sessions", &e.admin).await.json()["items"][0]["id"].as_str().unwrap().to_string();
    let r = send(&e.app, req("DELETE", &format!("/api/v1/me/sessions/{admin_sid}"), Some(&e.reader)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Signing out everywhere else.
    let phone = send(&e.app, login()).await.json()["token"].as_str().unwrap().to_string();
    let r = send(&e.app, req("DELETE", "/api/v1/me/sessions", Some(&e.reader)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.json()["ended"], 1);
    assert_eq!(get(&e.app, "/api/v1/me", &phone).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(get(&e.app, "/api/v1/me", &e.reader).await.status, StatusCode::OK);

    // A nickname: anything readable, the login stays.
    let me = json_req(&e.app, "PUT", "/api/v1/me", &e.reader, json!({ "display_name": "  Читачка 🌸 " })).await.json();
    assert_eq!((me["name"].as_str(), me["display_name"].as_str()), (Some("reader"), Some("Читачка 🌸")));
    assert_eq!(get(&e.app, "/api/v1/me", &e.reader).await.json()["display_name"], "Читачка 🌸");
    assert_eq!(json_req(&e.app, "PUT", "/api/v1/me", &e.reader, json!({ "display_name": "a\nb" })).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(json_req(&e.app, "PUT", "/api/v1/me", &e.reader, json!({ "display_name": "x".repeat(65) })).await.status, StatusCode::BAD_REQUEST);
    let users = get(&e.app, "/api/v1/users", &e.admin).await.json();
    assert!(users["users"].as_array().unwrap().iter().any(|u| u["display_name"] == "Читачка 🌸"));
    assert_eq!(json_req(&e.app, "PUT", "/api/v1/me", &e.reader, json!({ "display_name": "" })).await.json()["display_name"], Value::Null);

    // The last administrator keeps the rights; with another one, one's own can be given up.
    let me_admin = get(&e.app, "/api/v1/me", &e.admin).await.json()["id"].as_i64().unwrap();
    let reader_id = get(&e.app, "/api/v1/me", &e.reader).await.json()["id"].as_i64().unwrap();
    let r = json_req(&e.app, "PUT", &format!("/api/v1/users/{me_admin}"), &e.admin, json!({ "is_admin": false })).await;
    assert_eq!((r.status, r.json()["error"].as_str()), (StatusCode::CONFLICT, Some("last_admin")));
    json_req(&e.app, "PUT", &format!("/api/v1/users/{reader_id}"), &e.admin, json!({ "is_admin": true })).await;
    assert_eq!(json_req(&e.app, "PUT", &format!("/api/v1/users/{me_admin}"), &e.admin, json!({ "is_admin": false })).await.status, StatusCode::NO_CONTENT);
    assert_eq!(get(&e.app, "/api/v1/users", &e.admin).await.status, StatusCode::FORBIDDEN);
    assert_eq!(json_req(&e.app, "PUT", &format!("/api/v1/users/{reader_id}"), &e.reader, json!({ "is_admin": false })).await.status, StatusCode::CONFLICT);

    json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/progress"), &e.reader, json!({ "page": 2 })).await;
    json_req(&e.app, "POST", &format!("/api/v1/books/{id}/bookmarks"), &e.reader, json!({ "page": 1 })).await;
    let s = get(&e.app, "/api/v1/me/stats", &e.reader).await.json();
    assert_eq!((s["reading"].as_i64(), s["finished"].as_i64(), s["pages_read"].as_i64(), s["bookmarks"].as_i64()), (Some(1), Some(0), Some(3), Some(1)));
}

#[tokio::test]
async fn avatars() {
    let e = env("avatar");
    assert_eq!(get(&e.app, "/api/v1/me", &e.reader).await.json()["avatar"], Value::Null);
    let png = png_bytes(256, 256, 7, 100);
    let r = send(&e.app, req("PUT", "/api/v1/me/avatar", Some(&e.reader)).body(Body::from(png.clone())).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.body));
    let url = r.json()["avatar"].as_str().unwrap().to_string();
    assert!(url.contains("/avatar?v="));
    assert_eq!(get(&e.app, "/api/v1/me", &e.reader).await.json()["avatar"], url.as_str());
    // Others see it too (in the users list and by the address); the versioned address is cached for good.
    let r = get(&e.app, &url, &e.admin).await;
    assert_eq!((r.body.clone(), r.headers[header::CONTENT_TYPE].to_str().unwrap()), (png, "image/png"));
    assert!(r.headers[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
    let users = get(&e.app, "/api/v1/users", &e.admin).await.json();
    assert!(users["users"].as_array().unwrap().iter().any(|u| u["avatar"] == url.as_str()));
    // Not an image, too big.
    let r = send(&e.app, req("PUT", "/api/v1/me/avatar", Some(&e.reader)).body(Body::from("text")).unwrap()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(&e.app, req("PUT", "/api/v1/me/avatar", Some(&e.reader)).body(Body::from(png_bytes(9, 9, 1, 1_100_000))).unwrap()).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    // Removed.
    let r = send(&e.app, req("DELETE", "/api/v1/me/avatar", Some(&e.reader)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.json()["avatar"], Value::Null);
    assert_eq!(get(&e.app, &url, &e.admin).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn catalog_filters() {
    let e = env("catalog");
    let book = |name: &str, meta: Value, chapters: u16| {
        let path = e.data.join(format!("{name}.vnmf"));
        let mut b = Builder::new().with_uuid([name.as_bytes()[0]; 16]);
        b.set_info(serde_json::to_vec(&meta).unwrap());
        for c in 1..=chapters {
            b.add_page(png(600, 900, c as u8), PageRole::Normal, c, 1, 0).unwrap();
        }
        b.build(&path).unwrap();
        std::fs::read(&path).unwrap()
    };
    let books = [
        (
            "a",
            json!({"schema": "vnmf-metadata/1", "title": "Alpha", "type": "manga", "status": "completed", "genres": ["Fantasy", "Comedy"],
                     "origin_country": "jp", "age_rating": "16+", "publication": "serial", "start_date": "2004-04-01",
                     "translation": {"type": "fan"}}),
            3,
        ),
        (
            "b",
            json!({"schema": "vnmf-metadata/1", "title": "Beta", "type": "manhwa", "status": "ongoing", "genres": ["Fantasy"],
                     "origin_country": "KR", "age_rating": "13+", "color": "color", "release_date": "2019", "translation": {"type": "official"}}),
            8,
        ),
        (
            "c",
            json!({"schema": "vnmf-metadata/1", "title": "Gamma", "type": "manga", "status": "ongoing", "genres": ["Drama"],
                     "origin_country": "JP", "publication": "one_shot"}),
            1,
        ),
    ];
    for (name, meta, chapters) in books {
        let r = upload(&e.app, &format!("/api/v1/books?filename={name}.vnmf"), &e.admin, book(name, meta, chapters)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&r.body));
    }
    let titles = |uri: &'static str| {
        let app = e.app.clone();
        let token = e.reader.clone();
        async move {
            let r = get(&app, uri, &token).await.json();
            r["items"].as_array().unwrap_or_else(|| panic!("{r}")).iter().map(|b| b["title"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    };
    // Genres: all of them; other lists: any of them.
    assert_eq!(titles("/api/v1/books?genre=fantasy").await, ["Alpha", "Beta"]);
    assert_eq!(titles("/api/v1/books?genre=Fantasy,Comedy").await, ["Alpha"]);
    assert_eq!(titles("/api/v1/books?kind=manhwa,manga&status=ongoing").await, ["Beta", "Gamma"]);
    assert_eq!(titles("/api/v1/books?country=JP").await, ["Alpha", "Gamma"]);
    assert_eq!(titles("/api/v1/books?age=13%2B,16%2B").await, ["Alpha", "Beta"]);
    assert_eq!(titles("/api/v1/books?publication=one_shot").await, ["Gamma"]);
    assert_eq!(titles("/api/v1/books?color=color").await, ["Beta"]);
    assert_eq!(titles("/api/v1/books?translation=official").await, ["Beta"]);
    assert_eq!(titles("/api/v1/books?chapters_min=2&chapters_max=5").await, ["Alpha"]);
    assert_eq!(titles("/api/v1/books?year_min=2010").await, ["Beta"]);
    assert_eq!(titles("/api/v1/books?year_max=2010&kind=manga").await, ["Alpha"]);

    let f = get(&e.app, "/api/v1/books/facets", &e.reader).await.json();
    assert_eq!(f["countries"][0], json!({"value": "JP", "count": 2}));
    assert_eq!(f["ranges"]["year"], json!({"min": 2004, "max": 2019}));
    assert_eq!(f["ranges"]["chapters"], json!({"min": 1, "max": 8}));
    assert_eq!(f["translations"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn reading_lists() {
    let e = env("lists");
    let id = upload_rich(&e).await;
    let lists = get(&e.app, "/api/v1/lists", &e.reader).await.json();
    let kinds: Vec<&str> = lists["items"].as_array().unwrap().iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["reading", "planned", "completed", "on_hold", "dropped"]);
    let basic = |k: usize| lists["items"][k]["id"].as_i64().unwrap();
    let (reading, planned) = (basic(0), basic(1));

    // A basic list at a time; own lists on top of it.
    let put = |list: i64| req("PUT", &format!("/api/v1/books/{id}/lists/{list}"), Some(&e.reader)).body(Body::empty()).unwrap();
    assert_eq!(send(&e.app, put(reading)).await.json()["lists"], json!([reading]));
    assert_eq!(send(&e.app, put(planned)).await.json()["lists"], json!([planned]));
    let fav = json_req(&e.app, "POST", "/api/v1/lists", &e.reader, json!({ "name": "Улюблені" })).await;
    assert_eq!(fav.status, StatusCode::CREATED);
    let fav = fav.json()["id"].as_i64().unwrap();
    assert_eq!(send(&e.app, put(fav)).await.json()["lists"], json!([planned, fav]));
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/lists"), &e.reader).await.json()["lists"], json!([planned, fav]));

    // Counts and the catalog filter.
    let lists = get(&e.app, "/api/v1/lists", &e.reader).await.json();
    assert_eq!((lists["in_lists"].as_i64(), lists["library"].as_i64()), (Some(1), Some(1)));
    assert_eq!(lists["items"][5]["count"], 1);
    assert_eq!(get(&e.app, &format!("/api/v1/books?list={fav}"), &e.reader).await.json()["total"], 1);
    assert_eq!(get(&e.app, &format!("/api/v1/books?list={reading}"), &e.reader).await.json()["total"], 0);
    assert_eq!(get(&e.app, "/api/v1/books?list=any", &e.reader).await.json()["total"], 1);

    // Lists are personal.
    assert_eq!(
        send(&e.app, req("PUT", &format!("/api/v1/books/{id}/lists/{fav}"), Some(&e.admin)).body(Body::empty()).unwrap()).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(get(&e.app, &format!("/api/v1/books?list={fav}"), &e.admin).await.json()["total"], 0);

    // Rename, reorder, delete; basic lists can be renamed back but not deleted.
    let r = json_req(&e.app, "PUT", &format!("/api/v1/lists/{reading}"), &e.reader, json!({ "name": "Зараз" })).await.json();
    assert_eq!(r["items"][0]["name"], "Зараз");
    let r = json_req(&e.app, "PUT", &format!("/api/v1/lists/{reading}"), &e.reader, json!({ "name": null })).await.json();
    assert_eq!(r["items"][0]["name"], Value::Null);
    let r = json_req(&e.app, "PUT", "/api/v1/lists/order", &e.reader, json!({ "ids": [fav, planned] })).await.json();
    assert_eq!((r["items"][0]["id"].as_i64(), r["items"][1]["id"].as_i64(), r["items"][2]["id"].as_i64()), (Some(fav), Some(planned), Some(reading)));
    assert_eq!(
        send(&e.app, req("DELETE", &format!("/api/v1/lists/{reading}"), Some(&e.reader)).body(Body::empty()).unwrap()).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(json_req(&e.app, "POST", "/api/v1/lists", &e.reader, json!({ "name": "  " })).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(send(&e.app, req("DELETE", &format!("/api/v1/lists/{fav}"), Some(&e.reader)).body(Body::empty()).unwrap()).await.status, StatusCode::NO_CONTENT);
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/lists"), &e.reader).await.json()["lists"], json!([planned]));
    let r = send(&e.app, req("DELETE", &format!("/api/v1/books/{id}/lists/{planned}"), Some(&e.reader)).body(Body::empty()).unwrap()).await;
    assert_eq!(r.json()["lists"], json!([]));

    // Reading moves the book by itself: started — "reading", finished — "completed".
    let completed = basic(2);
    let on_hold = basic(3);
    let lists_of = || async { get(&e.app, &format!("/api/v1/books/{id}/lists"), &e.reader).await.json()["lists"].clone() };
    json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/progress"), &e.reader, json!({ "page": 1 })).await;
    assert_eq!(lists_of().await, json!([reading]));
    json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/progress"), &e.reader, json!({ "page": 2, "finished": true })).await;
    assert_eq!(lists_of().await, json!([completed]));
    // Unread: out of the automatic lists. A book on hold stays on hold while read.
    send(&e.app, req("DELETE", &format!("/api/v1/books/{id}/progress"), Some(&e.reader)).body(Body::empty()).unwrap()).await;
    assert_eq!(lists_of().await, json!([]));
    send(&e.app, put(on_hold)).await;
    json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/progress"), &e.reader, json!({ "page": 1 })).await;
    assert_eq!(lists_of().await, json!([on_hold]));
    // Completed by hand: read to the end.
    send(&e.app, put(completed)).await;
    let p = get(&e.app, &format!("/api/v1/books/{id}/progress"), &e.reader).await.json();
    let last = get(&e.app, &format!("/api/v1/books/{id}"), &e.reader).await.json()["page_count"].as_i64().unwrap() - 1;
    assert_eq!((p["finished"].as_bool(), p["furthest"].as_i64()), (Some(true), Some(last)));
}

#[tokio::test]
async fn copies_are_listed_once_and_identical_ones_deleted() {
    let e = env("copies");
    let uploaded = upload_rich(&e).await;
    let path = e.state.db.blocking(move |c| vnmf_server::library::book_row(c, uploaded)).unwrap().path;
    std::fs::copy(&path, e.extra.join("copy.vnmf")).unwrap();
    let st = e.state.clone();
    tokio::task::spawn_blocking(move || vnmf_server::library::scan(&st)).await.unwrap();
    assert_eq!(get(&e.app, "/api/v1/books?copies=true", &e.reader).await.json()["total"], 2);

    // One per UUID: the uploaded copy, unless the user reads the other one.
    let list = get(&e.app, "/api/v1/books", &e.reader).await.json();
    assert_eq!((list["total"].as_u64(), list["items"][0]["id"].as_i64()), (Some(1), Some(uploaded)));
    let book = get(&e.app, &format!("/api/v1/books/{uploaded}"), &e.admin).await.json();
    let copy = book["copies"][0]["id"].as_i64().unwrap();
    assert_eq!((book["copies"][0]["same_size"].as_bool(), book["copies"][0]["writable"].as_bool()), (Some(true), Some(false)));
    json_req(&e.app, "PUT", &format!("/api/v1/books/{copy}/progress"), &e.reader, json!({ "page": 2 })).await;
    assert_eq!(get(&e.app, "/api/v1/books", &e.reader).await.json()["items"][0]["id"], copy);
    assert_eq!(get(&e.app, "/api/v1/books", &e.admin).await.json()["items"][0]["id"], uploaded);
    assert_eq!(get(&e.app, "/api/v1/books/facets", &e.reader).await.json()["genres"][0]["count"], 1);
    let status = get(&e.app, "/api/v1/library", &e.admin).await.json();
    assert_eq!((status["counts"]["books"].as_u64(), status["counts"]["duplicates"].as_u64()), (Some(1), Some(1)));
    assert!(status["counts"]["duplicate_bytes"].as_u64().unwrap() > 0);
    assert_eq!(get(&e.app, "/api/v1/library/duplicates", &e.reader).await.status, StatusCode::FORBIDDEN);
    let dups = get(&e.app, "/api/v1/library/duplicates", &e.admin).await.json();
    assert_eq!(dups["groups"].as_array().unwrap().len(), 1);
    assert_eq!(dups["groups"][0]["copies"].as_array().unwrap().len(), 2);

    let delete = |keep: i64, drop: i64, token: String| {
        let app = e.app.clone();
        async move { send(&app, req("DELETE", &format!("/api/v1/books/{keep}/copies/{drop}"), Some(&token)).body(Body::empty()).unwrap()).await.status }
    };
    // The scanned copy is read-only; a reader can't delete; the uploaded one can go.
    assert_eq!(delete(uploaded, copy, e.admin.clone()).await, StatusCode::FORBIDDEN);
    assert_eq!(delete(copy, uploaded, e.reader.clone()).await, StatusCode::FORBIDDEN);
    // Different bytes are refused.
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(delete(copy, uploaded, e.admin.clone()).await, StatusCode::CONFLICT);
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();

    // Progress moves to the kept book (the newer one wins).
    json_req(&e.app, "PUT", &format!("/api/v1/books/{uploaded}/progress"), &e.admin, json!({ "page": 3 })).await;
    assert_eq!(delete(copy, uploaded, e.admin.clone()).await, StatusCode::NO_CONTENT);
    assert!(!path.exists());
    assert_eq!(get(&e.app, &format!("/api/v1/books/{copy}/progress"), &e.admin).await.json()["page"], 3);
    assert_eq!(get(&e.app, &format!("/api/v1/books/{copy}/progress"), &e.reader).await.json()["page"], 2);
    assert_eq!(get(&e.app, "/api/v1/books?copies=true", &e.reader).await.json()["total"], 1);
    assert_eq!(get(&e.app, "/api/v1/library/duplicates", &e.admin).await.json()["groups"], json!([]));

    // Going back keeps the furthest page reached; finishing reaches the last page.
    let back = json_req(&e.app, "PUT", &format!("/api/v1/books/{copy}/progress"), &e.admin, json!({ "page": 1 })).await.json();
    assert_eq!((back["page"].as_i64(), back["furthest"].as_i64()), (Some(1), Some(3)));
    let pages = get(&e.app, &format!("/api/v1/books/{copy}"), &e.admin).await.json()["page_count"].as_i64().unwrap();
    let done = json_req(&e.app, "PUT", &format!("/api/v1/books/{copy}/progress"), &e.admin, json!({ "page": 0, "finished": true })).await.json();
    assert_eq!(done["furthest"].as_i64(), Some(pages - 1));
}

#[tokio::test]
async fn fragments_too_tall_for_browsers_are_cut_into_tiles() {
    let e = env("tiles");
    let tall = image::RgbImage::from_fn(16, 40_000, |_, y| image::Rgb([(y / 8192 * 50) as u8, 0, 0]));
    let mut bytes = Vec::new();
    tall.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    let mut b = Builder::new();
    b.add_webtoon(vec![ChunkSource::from_bytes(bytes).unwrap(), png(16, 1000, 1)], 1, 1, 0).unwrap();
    b.build(e.extra.join("tall.vnmf")).unwrap();
    let st = e.state.clone();
    tokio::task::spawn_blocking(move || vnmf_server::library::scan(&st)).await.unwrap();
    let id = get(&e.app, "/api/v1/books", &e.reader).await.json()["items"][0]["id"].as_i64().unwrap();

    let manifest = get(&e.app, &format!("/api/v1/books/{id}/manifest"), &e.reader).await.json();
    let chunks = &manifest["pages"][0]["chunks"];
    assert_eq!((chunks[0]["tiles"].as_u64(), chunks[1].get("tiles")), (Some(5), None));
    let chunk = chunks[0]["index"].as_u64().unwrap();
    let r = get(&e.app, &format!("/api/v1/books/{id}/chunks/{chunk}/tiles/4"), &e.reader).await;
    assert_eq!((r.status, r.headers[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "image/jpeg"));
    let tile = image::load_from_memory(&r.body).unwrap().to_rgb8();
    assert_eq!(tile.dimensions(), (16, 40_000 - 4 * 8192));
    assert!(tile.get_pixel(0, 0)[0] > 190, "the tile is the fragment's last part");
    assert_eq!(get(&e.app, &format!("/api/v1/books/{id}/chunks/{chunk}/tiles/5"), &e.reader).await.status, StatusCode::NOT_FOUND);
    // Served from the cache afterwards.
    assert_eq!(std::fs::read_dir(e.data.join("cache/tiles")).unwrap().next().unwrap().unwrap().path().read_dir().unwrap().count(), 5);
}

/// A small book with the given metadata and number of pages.
fn book_with(path: &Path, uuid: u8, info: Value, pages: u8) {
    let mut b = Builder::new().with_uuid([uuid; 16]);
    b.set_info(serde_json::to_vec(&info).unwrap());
    for p in 0..pages {
        b.add_page(png(800, 1200, p + 1), PageRole::Normal, 1, u16::from(p) + 1, 0).unwrap();
    }
    b.build(path).unwrap();
}

#[tokio::test]
async fn finishing_an_ongoing_translation_is_catching_up() {
    let e = env("ongoing");
    // The series is complete, its translation isn't; the other book is complete.
    let ongoing = json!({ "schema": "vnmf-metadata/1", "title": "Ongoing", "status": "completed", "translation": { "status": "ongoing" } });
    let complete = json!({ "schema": "vnmf-metadata/1", "title": "Complete", "status": "completed" });
    book_with(&e.extra.join("ongoing.vnmf"), 1, ongoing.clone(), 2);
    book_with(&e.extra.join("complete.vnmf"), 2, complete.clone(), 2);
    let scan = || {
        let st = e.state.clone();
        async move { tokio::task::spawn_blocking(move || vnmf_server::library::scan(&st)).await.unwrap() }
    };
    scan().await;
    let items = get(&e.app, "/api/v1/books", &e.reader).await.json()["items"].clone();
    let id_of = |title: &str| items.as_array().unwrap().iter().find(|b| b["title"] == title).unwrap()["id"].as_i64().unwrap();
    let (a, b) = (id_of("Ongoing"), id_of("Complete"));
    let lists = get(&e.app, "/api/v1/lists", &e.reader).await.json();
    let (reading, completed) = (lists["items"][0]["id"].as_i64().unwrap(), lists["items"][2]["id"].as_i64().unwrap());
    let lists_of = |id: i64| {
        let app = e.app.clone();
        let token = e.reader.clone();
        async move { get(&app, &format!("/api/v1/books/{id}/lists"), &token).await.json()["lists"].clone() }
    };

    for id in [a, b] {
        json_req(&e.app, "PUT", &format!("/api/v1/books/{id}/progress"), &e.reader, json!({ "page": 1, "finished": true })).await;
    }
    assert_eq!(lists_of(a).await, json!([reading]), "caught up with an ongoing translation");
    assert_eq!(lists_of(b).await, json!([completed]));

    // New chapters came out: not finished any more, back in "reading".
    book_with(&e.extra.join("complete.vnmf"), 2, complete, 3);
    scan().await;
    let p = get(&e.app, &format!("/api/v1/books/{b}/progress"), &e.reader).await.json();
    assert_eq!((p["finished"].as_bool(), p["furthest"].as_i64()), (Some(false), Some(1)));
    assert_eq!(lists_of(b).await, json!([reading]));
}

#[tokio::test]
async fn books_not_checked_lately_are_verified_by_themselves() {
    let e = env("scrub");
    let id = upload_rich(&e).await;
    let week = std::time::Duration::from_secs(7 * 86400);
    let full = |st: AppState| async move {
        st.db.blocking(move |c| Ok(c.query_row("SELECT health_full FROM books WHERE id = ?1", [id], |r| r.get::<_, Option<bool>>(0))?)).unwrap()
    };
    assert_ne!(full(e.state.clone()).await, Some(true));
    assert!(vnmf_server::library::start_verify_due(&e.state, week));
    for _ in 0..100 {
        if !get(&e.app, "/api/v1/library", &e.admin).await.json()["verify"]["running"].as_bool().unwrap() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(full(e.state.clone()).await, Some(true));
    // Checked within the week: nothing to do.
    assert!(!vnmf_server::library::start_verify_due(&e.state, week));
    assert!(get(&e.app, "/api/v1/library", &e.admin).await.json()["counts"]["oldest_full_check"].as_i64().is_some());
}
