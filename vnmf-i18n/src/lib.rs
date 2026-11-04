// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori)

//! Runtime translations for the VNMF applications.
//!
//! Strings in the code stay in English and serve as message ids. Translations live in
//! gettext catalogs (`po/<lang>.po` in the source tree, compiled with `msgfmt` into
//! `<prefix>/share/locale/<lang>/LC_MESSAGES/vnmf.mo`), so a new language needs only a new
//! `.po` file — the programs themselves are not rebuilt. See `po/README.md`.
//!
//! Translation is off until [`init`] is called: the GUI applications call it, the console
//! tools don't (their output stays English and script-friendly).
//!
//! - [`tr`] — translate a string;
//! - [`ntr`] — choose a plural form by `n` (languages have different numbers of forms);
//! - [`trf!`] / [`ntrf!`] — the same with `{0}`, `{1}`… placeholders, so a translation may
//!   reorder them;
//! - [`n_`] — mark a string in a `const` table for extraction; translate it with [`tr`] where it's shown.
//! - [`language`] / [`set_language`] / [`available`] — the interface language chosen by the user
//!   (`~/.config/vnmf/language`, shared by all applications), applied by [`init`] on the next start.
//!
//! Backend: glibc's built-in libintl (no extra libraries). Elsewhere (Android, musl, other
//! systems) every function returns the English original.

use std::fmt::{Display, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The gettext domain: catalogs are named `vnmf.mo`.
pub const DOMAIN: &str = "vnmf";

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Marks a string for extraction without translating it (for `const` tables).
/// Translate it with [`tr`] where it is shown.
pub const fn n_(msgid: &'static str) -> &'static str {
    msgid
}

/// Translation of `msgid`, or `msgid` itself if there's none.
pub fn tr(msgid: &'static str) -> &'static str {
    if msgid.is_empty() || !ENABLED.load(Ordering::Relaxed) {
        return msgid;
    }
    sys::dgettext(msgid)
}

/// The plural form for `n`: `singular`/`plural` in English, as many forms as the language has in a translation.
pub fn ntr(singular: &'static str, plural: &'static str, n: u64) -> &'static str {
    if !ENABLED.load(Ordering::Relaxed) {
        return if n == 1 { singular } else { plural };
    }
    sys::dngettext(singular, plural, n)
}

/// Substitutes `{0}`, `{1}`… in `template` with `args` (`{{` / `}}` — literal braces).
/// An unknown index is left as is, so a broken translation never panics.
pub fn format(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16 * args.len());
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if let Some(end) = tail.starts_with('{').then(|| tail.find('}')).flatten()
            && let Ok(k) = tail[1..end].parse::<usize>()
            && let Some(arg) = args.get(k)
        {
            let _ = write!(out, "{arg}");
            rest = &tail[end + 1..];
            continue;
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// [`tr`] + [`format`]: `trf!("Page {0} of {1}", page, total)`.
#[macro_export]
macro_rules! trf {
    ($msgid:literal $(, $arg:expr)* $(,)?) => {
        $crate::format($crate::tr($msgid), &[$(&$arg as &dyn ::std::fmt::Display),*])
    };
}

/// [`ntr`] + [`format`]: `ntrf!("{0} page", "{0} pages", n, n)` — `n` chooses the form, the rest are `{0}`, `{1}`….
#[macro_export]
macro_rules! ntrf {
    ($singular:literal, $plural:literal, $n:expr $(, $arg:expr)* $(,)?) => {
        $crate::format($crate::ntr($singular, $plural, ($n) as u64), &[$(&$arg as &dyn ::std::fmt::Display),*])
    };
}

/// Turns translations on: sets the locale from the environment (`LANG`, `LC_ALL`, `LC_MESSAGES`)
/// and binds the catalogs. `localedir` — where `<lang>/LC_MESSAGES/vnmf.mo` lives; `None` — the first
/// existing of `$VNMF_LOCALEDIR`, `<exe>/../share/locale` (the installation prefix) and `/usr/share/locale`.
///
/// The language chosen in the settings ([`set_language`]) takes precedence over the system one.
/// Call it at startup, before other threads exist and before the toolkit is initialised (GTK's own
/// strings follow the same choice).
pub fn init(localedir: Option<&Path>) {
    let dir = localedir.map(Path::to_path_buf).or_else(default_localedir);
    let _ = LOCALEDIR.set(dir.clone());
    let _ = SYSTEM_LANGUAGE.set(std::env::var_os("LANGUAGE"));
    if let Some(lang) = language() {
        // `LANGUAGE` is the gettext override of the message language: it also switches GTK and
        // libadwaita, while dates, numbers and the like stay with the system locale.
        // SAFETY: called once at startup, before any other threads exist (see above).
        unsafe { std::env::set_var("LANGUAGE", &lang) };
    }
    if sys::init(dir.as_deref()) {
        ENABLED.store(true, Ordering::Relaxed);
    }
}

static LOCALEDIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
/// `LANGUAGE` as it was before [`init`] replaced it with the chosen language.
static SYSTEM_LANGUAGE: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();

/// Gives a child VNMF process the original `LANGUAGE`, so it applies the current choice itself
/// instead of inheriting the one this process started with (e.g. a restart after switching back to the system language).
pub fn restore_env(command: &mut std::process::Command) {
    match SYSTEM_LANGUAGE.get() {
        Some(Some(value)) => command.env("LANGUAGE", value),
        Some(None) => command.env_remove("LANGUAGE"),
        None => command,
    };
}

fn default_localedir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("VNMF_LOCALEDIR") {
        return Some(dir.into());
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let prefix = exe.parent()?.parent()?;
    [prefix.join("share/locale"), "/usr/share/locale".into()].into_iter().find(|d| d.is_dir())
}

// ---------------------------------------------------------------------------------------------
// The interface language chosen by the user
// ---------------------------------------------------------------------------------------------

/// `~/.config/vnmf/language`: one line with a language code (`uk`, `en`, `pt_BR`…), shared by
/// all VNMF applications. No file — the system language.
fn language_file() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("vnmf").join("language"))
}

/// The language chosen in the settings; `None` — follow the system.
pub fn language() -> Option<String> {
    let text = std::fs::read_to_string(language_file()?).ok()?;
    let code = text.trim();
    is_code(code).then(|| code.to_owned())
}

/// Saves the interface language for all VNMF applications (`None` — the system language).
/// Takes effect on the next start of an application.
pub fn set_language(code: Option<&str>) -> std::io::Result<()> {
    let file = language_file().ok_or_else(|| std::io::Error::other("no configuration directory"))?;
    match code.filter(|c| is_code(c)) {
        Some(code) => {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&file, format!("{code}\n"))
        }
        None => match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

/// A plausible gettext language code: `uk`, `pt_BR`, `sr@latin` — nothing that could escape a path.
fn is_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 16 && code.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '-'))
}

/// Languages the interface is available in: English (the original) and every language with an
/// installed `vnmf.mo` catalog. Sorted by code, English first.
pub fn available() -> Vec<String> {
    let mut found: Vec<String> = LOCALEDIR
        .get()
        .cloned()
        .flatten()
        .or_else(default_localedir)
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().join("LC_MESSAGES").join(format!("{DOMAIN}.mo")).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|code| is_code(code) && code != "en")
        .collect();
    found.sort();
    found.insert(0, "en".to_owned());
    found
}

/// The name of a language in that language itself (so it's recognisable whatever the current one is).
pub fn language_name(code: &str) -> String {
    let name = match code {
        "en" => "English",
        "uk" => "Українська",
        "be" => "Беларуская",
        "cs" => "Čeština",
        "de" => "Deutsch",
        "es" => "Español",
        "fr" => "Français",
        "it" => "Italiano",
        "ja" => "日本語",
        "ka" => "ქართული",
        "ko" => "한국어",
        "lt" => "Lietuvių",
        "lv" => "Latviešu",
        "nl" => "Nederlands",
        "pl" => "Polski",
        "pt" => "Português",
        "pt_BR" => "Português (Brasil)",
        "ro" => "Română",
        "sk" => "Slovenčina",
        "sv" => "Svenska",
        "tr" => "Türkçe",
        "zh_CN" => "简体中文",
        "zh_TW" => "繁體中文",
        _ => return code.to_owned(),
    };
    name.to_owned()
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod sys {
    use ffi::{bind_textdomain_codeset, bindtextdomain, setlocale};
    use std::ffi::{CStr, CString, c_char, c_int, c_ulong};
    use std::path::Path;

    mod ffi {
        use std::ffi::{c_char, c_int, c_ulong};
        unsafe extern "C" {
            pub fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
            pub fn bindtextdomain(domain: *const c_char, dir: *const c_char) -> *mut c_char;
            pub fn bind_textdomain_codeset(domain: *const c_char, codeset: *const c_char) -> *mut c_char;
            pub fn dgettext(domain: *const c_char, msgid: *const c_char) -> *mut c_char;
            pub fn dngettext(domain: *const c_char, msgid: *const c_char, plural: *const c_char, n: c_ulong) -> *mut c_char;
        }
    }

    /// `LC_ALL` in glibc.
    const LC_ALL: c_int = 6;
    /// `LC_MESSAGES` in glibc.
    const LC_MESSAGES: c_int = 5;
    const DOMAIN: &CStr = c"vnmf";

    pub fn init(dir: Option<&Path>) -> bool {
        use std::os::unix::ffi::OsStrExt;
        // SAFETY: called once at startup, before any other threads read the locale.
        unsafe {
            setlocale(LC_ALL, c"".as_ptr());
            // gettext ignores `LANGUAGE` (the language chosen in the settings) under the plain "C"
            // locale, e.g. on a system without any configured locale: then — "C.UTF-8", which it honours.
            if std::env::var_os("LANGUAGE").is_some() {
                let current = setlocale(LC_MESSAGES, std::ptr::null());
                if !current.is_null() && matches!(CStr::from_ptr(current).to_bytes(), b"C" | b"POSIX") {
                    setlocale(LC_MESSAGES, c"C.UTF-8".as_ptr());
                }
            }
            if let Some(dir) = dir.and_then(|d| CString::new(d.as_os_str().as_bytes()).ok()) {
                bindtextdomain(DOMAIN.as_ptr(), dir.as_ptr());
            }
            bind_textdomain_codeset(DOMAIN.as_ptr(), c"UTF-8".as_ptr());
        }
        true
    }

    /// The result of gettext: either our own argument (no translation) or a string inside the
    /// loaded catalog, which stays mapped until the process exits.
    fn result(ptr: *const c_char, originals: &[(&CString, &'static str)]) -> &'static str {
        if let Some((_, s)) = originals.iter().find(|(c, _)| c.as_ptr() == ptr) {
            return s;
        }
        // SAFETY: gettext returned a NUL-terminated string from the catalog; catalogs are never unloaded.
        let s = unsafe { CStr::from_ptr(ptr) };
        match s.to_str() {
            Ok(s) => unsafe { &*std::ptr::from_ref::<str>(s) },
            Err(_) => originals[0].1,
        }
    }

    pub fn dgettext(msgid: &'static str) -> &'static str {
        let Ok(c) = CString::new(msgid) else { return msgid };
        // SAFETY: both strings are valid and NUL-terminated.
        let ptr = unsafe { ffi::dgettext(DOMAIN.as_ptr(), c.as_ptr()) };
        result(ptr, &[(&c, msgid)])
    }

    pub fn dngettext(singular: &'static str, plural: &'static str, n: u64) -> &'static str {
        let (Ok(s), Ok(p)) = (CString::new(singular), CString::new(plural)) else {
            return if n == 1 { singular } else { plural };
        };
        // SAFETY: all strings are valid and NUL-terminated.
        let ptr = unsafe { ffi::dngettext(DOMAIN.as_ptr(), s.as_ptr(), p.as_ptr(), n as c_ulong) };
        result(ptr, &[(&s, singular), (&p, plural)])
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod sys {
    use std::path::Path;

    pub fn init(_dir: Option<&Path>) -> bool {
        false
    }
    pub fn dgettext(msgid: &'static str) -> &'static str {
        msgid
    }
    pub fn dngettext(singular: &'static str, plural: &'static str, n: u64) -> &'static str {
        if n == 1 { singular } else { plural }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_placeholders() {
        assert_eq!(format("Page {0} of {1}", &[&3, &"10"]), "Page 3 of 10");
        assert_eq!(format("{1} — {0}", &[&"a", &"b"]), "b — a");
        assert_eq!(format("{{0}} {0}", &[&1]), "{0} 1");
        // A broken translation doesn't panic: unknown placeholders stay as they are.
        assert_eq!(format("{5} {x} {", &[&1]), "{5} {x} {");
    }

    #[test]
    fn untranslated_without_init() {
        assert_eq!(tr("Open"), "Open");
        assert_eq!(ntr("page", "pages", 1), "page");
        assert_eq!(ntr("page", "pages", 5), "pages");
        assert_eq!(trf!("{0} of {1}", 1, 2), "1 of 2");
        assert_eq!(ntrf!("{0} page", "{0} pages", 3, 3), "3 pages");
    }

    #[test]
    fn language_codes_and_names() {
        assert!(is_code("uk") && is_code("pt_BR") && is_code("sr@latin"));
        assert!(!is_code("") && !is_code("../x") && !is_code("a/b") && !is_code("uk\n"));
        assert_eq!(language_name("uk"), "Українська");
        assert_eq!(language_name("xx"), "xx");
        assert_eq!(available().first().map(String::as_str), Some("en"));
    }
}
