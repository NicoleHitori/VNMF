// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Shared code for the VNMF GTK4/libadwaita applications: `vnmf-info`, `vnmf-reader`, `vnmf-creator`.
//!
//! All heavy work (decoding images, integrity checks, building the file) runs
//! on background threads via [`gio::spawn_blocking`]; the UI only awaits the result.

pub mod aspect_picture;
pub mod audio;
pub mod health_ui;
pub mod imaging;
pub mod widgets;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};
use vnmf_i18n::tr;

pub use vnmf::human_size;

/// Extra styles (on top of Adwaita).
const CSS: &str = r#"
.page-card { padding: 6px; border-radius: 12px; }
.page-card picture { border-radius: 8px; }
.page-card.drop-before { box-shadow: inset 4px 0 0 0 @accent_color; }
.page-card.drop-after { box-shadow: inset -4px 0 0 0 @accent_color; }
.section-header { padding: 4px 8px; border-radius: 10px; }
.section-header.drop-before { box-shadow: inset 0 4px 0 0 @accent_color; }
.group-drop-zone { padding: 18px; border-radius: 12px; border: 2px dashed alpha(@accent_color, 0.6); }
.group-drop-zone.drop-before { background: alpha(@accent_color, 0.15); }
.damage-badge { padding: 1px 8px; border-radius: 99px; font-size: smaller; background: @error_bg_color; color: @error_fg_color; }
.page-badge { padding: 2px 8px; border-radius: 99px; font-weight: bold; font-feature-settings: "tnum"; }
.role-badge { padding: 1px 8px; border-radius: 99px; font-size: smaller; }
.aux-badge { padding: 2px 6px; border-radius: 99px; }
.page-text { padding: 12px 16px; }
.page-text-panel { padding: 12px 14px; border-radius: 14px; background: alpha(@window_bg_color, 0.94); box-shadow: 0 2px 8px alpha(black, 0.25); }
.reader-surface { background: @view_bg_color; }
.reader-bg-black .reader-surface { background: #000000; }
.reader-bg-gray .reader-surface { background: #3a3a3a; }
.reader-bg-white .reader-surface { background: #ffffff; }
.floating-bar { border-radius: 99px; padding: 4px 10px; }
"#;

/// Attaches [`CSS`] to the display (once).
pub fn load_css() {
    thread_local!(static LOADED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if LOADED.with(|l| l.replace(true)) {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// Version for "About" dialogs.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Path to a sibling tool (next to the current executable), otherwise from `PATH`.
pub fn sibling_binary(name: &str) -> PathBuf {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(name))).filter(|p| p.exists()).unwrap_or_else(|| PathBuf::from(name))
}

/// Launches another VNMF application with a file (e.g. Reader after saving in Creator).
pub fn launch_sibling(name: &str, file: &std::path::Path) -> std::io::Result<()> {
    let mut command = std::process::Command::new(sibling_binary(name));
    command.arg(file);
    // The child applies the language chosen now, not the one this process started with.
    vnmf_i18n::restore_env(&mut command);
    command.spawn().map(drop)
}

/// Restarts the application in a new process (e.g. after changing the interface language):
/// closes `window` with its dialogs (its `close_request` saves what it needs) and starts the same program with `args`.
pub fn restart_app(window: &impl IsA<gtk::Window>, args: impl IntoIterator<Item = PathBuf>) {
    let window = window.as_ref();
    // With a dialog open (the restart button lives in one) libadwaita's window closes the dialog
    // instead of itself: the old window stayed next to the new one. Dialogs go first.
    let visible_dialog = || {
        window
            .downcast_ref::<adw::ApplicationWindow>()
            .and_then(|w| w.visible_dialog())
            .or_else(|| window.downcast_ref::<adw::Window>().and_then(|w| w.visible_dialog()))
    };
    let mut last = None;
    while let Some(dialog) = visible_dialog() {
        if last.as_ref() == Some(&dialog) {
            break; // didn't close — don't loop forever
        }
        dialog.force_close();
        last = Some(dialog);
    }
    window.close();
    let started = std::env::current_exe().and_then(|exe| {
        let mut command = std::process::Command::new(exe);
        command.args(args);
        vnmf_i18n::restore_env(&mut command);
        command.spawn()
    });
    if let Err(e) = started {
        eprintln!("could not restart: {e}");
    }
}

/// A VNMF application window: can present itself and open a file.
pub trait AppWindow: Clone + 'static {
    fn present(&self);
    fn open(&self, path: PathBuf);
    /// Several files from a file manager — into one window (`open_many`), not a window per file.
    const ALL_IN_ONE: bool = false;
    fn open_many(&self, paths: Vec<PathBuf>) {
        for path in paths {
            self.open(path);
        }
    }
    /// The main GTK window (for the debug snapshot).
    fn gtk_window(&self) -> gtk::Window;
    fn after_present(&self) {
        snapshot_if_requested(&self.gtk_window());
    }
}

/// VNMF file type (see `data/vnmf-mime.xml`).
pub const MIME_TYPE: &str = "application/x-vnmf";
const READER_DESKTOP: &str = "org.vnmf.Reader.desktop";

/// If the user hasn't chosen an application for .vnmf yet — make it VNMF Reader.
///
/// A system package can't set the default application itself (the shared `mimeapps.list`
/// belongs to another package), and with no choice GNOME picks the alphabetically first one — VNMF Info.
/// An explicit user choice (any entry in `~/.config/mimeapps.list`) is left alone;
/// without an installed `.desktop` (running via `cargo run`) nothing is done.
pub fn ensure_default_handler() {
    let list = glib::user_config_dir().join("mimeapps.list");
    let keys = glib::KeyFile::new();
    if keys.load_from_file(&list, glib::KeyFileFlags::NONE).is_ok() && keys.has_key("Default Applications", MIME_TYPE).unwrap_or(false) {
        return;
    }
    let Some(reader) = gio::AppInfo::all().into_iter().find(|a| a.id().as_deref() == Some(READER_DESKTOP)) else { return };
    if let Err(e) = reader.set_as_default_for_type(MIME_TYPE) {
        eprintln!("could not make VNMF Reader the default application: {e}");
    }
}

/// Standard application scaffold: id, opening files from the command line, keyboard shortcuts.
pub fn run_app<W: AppWindow>(app_id: &str, build: impl Fn(&adw::Application) -> W + 'static) -> glib::ExitCode {
    // Translations: the system language, catalogs from `<prefix>/share/locale` (see `po/README.md`).
    vnmf_i18n::init(None);
    let app = adw::Application::builder().application_id(app_id).flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE).build();

    app.connect_startup(|_| ensure_default_handler());

    let build = std::rc::Rc::new(build);
    {
        let build = build.clone();
        app.connect_activate(move |app| {
            load_css();
            let window = build(app);
            window.present();
            window.after_present();
        });
    }
    app.connect_open(move |app, files, _| {
        load_css();
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        let batches = if W::ALL_IN_ONE { vec![paths] } else { paths.into_iter().map(|p| vec![p]).collect() };
        for batch in batches {
            let window = build(app);
            window.present();
            window.open_many(batch);
            window.after_present();
        }
    });

    app.set_accels_for_action("win.open", &["<Ctrl>o"]);
    app.set_accels_for_action("window.close", &["<Ctrl>w"]);
    app.run()
}

/// Debugging: if `VNMF_GUI_SNAPSHOT=<file.png>` is set, after `VNMF_GUI_SNAPSHOT_DELAY_MS`
/// (default 3000) ms the window contents are rendered to PNG and the application exits.
/// `VNMF_GUI_SNAPSHOT_ACTION=win.action` — run an action before the snapshot (open a dialog, etc.);
/// `VNMF_GUI_SNAPSHOT_SCROLL=0..1` — scroll the lists.
/// Allows checking the UI without a screen (e.g. with `GDK_BACKEND=broadway`).
pub fn snapshot_if_requested(window: &impl IsA<gtk::Window>) {
    let Ok(path) = std::env::var("VNMF_GUI_SNAPSHOT") else { return };
    // Without a screen the frame clock may stall — animations (dialogs appearing) would freeze at the start.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
    let delay = std::env::var("VNMF_GUI_SNAPSHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    let window = window.as_ref().clone();
    // `VNMF_GUI_SNAPSHOT_SCROLL=0.6` — scroll all lists by a fraction of their height (to see the bottom of the page).
    if let Some(fraction) = std::env::var("VNMF_GUI_SNAPSHOT_SCROLL").ok().and_then(|v| v.parse::<f64>().ok()) {
        let w = window.clone();
        // `VNMF_GUI_SNAPSHOT_SCROLL_MS` — when to scroll (default 3/4 of the delay).
        let at = std::env::var("VNMF_GUI_SNAPSHOT_SCROLL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(delay * 3 / 4);
        glib::timeout_add_local_once(std::time::Duration::from_millis(at), move || {
            fn walk(widget: &gtk::Widget, fraction: f64) {
                if let Some(scroller) = widget.downcast_ref::<gtk::ScrolledWindow>() {
                    let adj = scroller.vadjustment();
                    adj.set_value((adj.upper() - adj.page_size()) * fraction);
                }
                let mut child = widget.first_child();
                while let Some(c) = child {
                    walk(&c, fraction);
                    child = c.next_sibling();
                }
            }
            walk(w.upcast_ref(), fraction);
        });
    }
    // Optional actions before the snapshot (comma-separated), e.g. `win.preferences` or `win.double,win.right`.
    if let Ok(actions) = std::env::var("VNMF_GUI_SNAPSHOT_ACTION") {
        let w = window.clone();
        // A comma inside parentheses is part of the parameter: `win.role((uint64 7, byte 1))`.
        let mut names: Vec<String> = Vec::new();
        let (mut depth, mut current) = (0i32, String::new());
        for c in actions.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    names.push(std::mem::take(&mut current));
                    continue;
                }
                _ => {}
            }
            current.push(c);
        }
        names.push(current);
        let names: Vec<String> = names.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
        let step = delay / 2 / (names.len() as u64 + 1);
        for (i, action) in names.into_iter().enumerate() {
            let w = w.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(step * (i as u64 + 1)), move || {
                // Support for "detailed" names with a parameter: `win.mode::strip`.
                let (name, target) = gio::Action::parse_detailed_name(&action).map(|(n, t)| (n.to_string(), t)).unwrap_or_else(|_| (action.clone(), None));
                if let Err(e) = w.activate_action(&name, target.as_ref()) {
                    eprintln!("VNMF_GUI_SNAPSHOT_ACTION {action}: {e}");
                }
            });
        }
    }
    let attempts = std::rc::Rc::new(std::cell::Cell::new(0));
    glib::timeout_add_local(std::time::Duration::from_millis(delay), move || {
        let result = (|| -> Result<(), &'static str> {
            // If a dialog is open in a separate window — snapshot it.
            let target = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .rfind(|w| w != &window && w.is_visible())
                .unwrap_or_else(|| window.clone());
            let (w, h) = (target.width(), target.height());
            let render = |widget: &gtk::Widget, background: bool| {
                let snapshot = gtk::Snapshot::new();
                if background {
                    let rect = gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
                    snapshot.append_color(&gtk::gdk::RGBA::new(0.13, 0.13, 0.15, 1.0), &rect);
                }
                gtk::WidgetPaintable::new(Some(widget)).snapshot(&snapshot, f64::from(w), f64::from(h));
                snapshot.to_node()
            };
            // The whole window (including the background); if it doesn't render (happens with an open AdwDialog) —
            // the dialog itself.
            let node = render(target.upcast_ref(), false)
                .or_else(|| {
                    let dialog = find_widget(target.upcast_ref(), |w| w.is::<adw::Dialog>() && w.is_mapped())?;
                    render(&dialog, true)
                })
                .ok_or(tr("empty render node"))?;
            let renderer = target.native().and_then(|n| n.renderer()).ok_or(tr("no renderer"))?;
            let texture = renderer.render_texture(&node, None);
            texture.save_to_png(&path).map_err(|_| tr("could not write PNG"))
        })();
        // The widget may not be drawn yet — several retries.
        attempts.set(attempts.get() + 1);
        if result.is_err() && attempts.get() < 10 {
            return glib::ControlFlow::Continue;
        }
        if let Err(e) = result {
            eprintln!("VNMF_GUI_SNAPSHOT: {e}");
        }
        if let Some(app) = window.application() {
            app.quit();
        }
        glib::ControlFlow::Break
    });
}

/// First descendant that satisfies the predicate (widget tree traversal).
fn find_widget(root: &gtk::Widget, pred: impl Fn(&gtk::Widget) -> bool) -> Option<gtk::Widget> {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if pred(&w) {
            return Some(w);
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            stack.push(c);
        }
    }
    None
}
