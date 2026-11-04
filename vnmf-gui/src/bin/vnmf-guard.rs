// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! VNMF Guard — unobtrusive Reed–Solomon redundancy in video, audio and disk images (like `vnmfguard`).
//!
//! A list of files (open or drop several), a redundancy percentage and four actions on the whole
//! list: check, protect, repair, remove protection. Files are modified in place (atomically).
//! Each file is processed in the background; its state is shown in its row.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use vnmf_guard::{Format, Inspection, Options, ProtectOutcome, RepairOutcome, StripOutcome};
use vnmf_gui::{AppWindow, human_size, widgets};
use vnmf_i18n::{tr, trf};

const APP_ID: &str = "org.vnmf.Guard";

fn main() -> glib::ExitCode {
    vnmf_gui::run_app(APP_ID, GuardWindow::new)
}

/// File state — determines the row icon.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Unknown,
    Unprotected,
    Clean,
    Damaged,
    Lost,
    Error,
}

impl State {
    fn icon(self) -> &'static str {
        match self {
            State::Unknown => "content-loading-symbolic",
            State::Unprotected => "security-low-symbolic",
            State::Clean => "security-high-symbolic",
            State::Damaged => "dialog-warning-symbolic",
            State::Lost | State::Error => "dialog-error-symbolic",
        }
    }

    fn css(self) -> Option<&'static str> {
        match self {
            State::Clean => Some("success"),
            State::Damaged => Some("warning"),
            State::Lost | State::Error => Some("error"),
            _ => None,
        }
    }
}

/// An action on the list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    Check,
    Protect(u8),
    Repair,
    Strip,
}

fn describe(i: &Inspection) -> (State, String) {
    let state = if i.is_clean() {
        State::Clean
    } else if i.is_recoverable() {
        State::Damaged
    } else {
        State::Lost
    };
    let size = human_size(i.file_len);
    let text = if i.is_clean() {
        trf!("Protected {0}% · {1} · {2} · intact", i.plan.redundancy, i.plan.format.name(), size)
    } else {
        format!("{} · {size}", i.summary())
    };
    (state, text)
}

/// Performs an action on a single file (on a background thread).
fn run(job: Job, path: &Path) -> (State, String) {
    let result = (|| -> anyhow::Result<(State, String)> {
        Ok(match job {
            Job::Check => match vnmf_guard::inspect(path)? {
                Some(i) => describe(&i),
                None => unprotected(path)?,
            },
            Job::Protect(redundancy) => {
                let opts = Options { redundancy, ..Options::default() };
                match vnmf_guard::protect(path, None, &opts)? {
                    ProtectOutcome::Nothing => (State::Unknown, tr("0% — nothing changed").into()),
                    ProtectOutcome::AlreadyProtected(r) => (State::Clean, trf!("Already protected {0}%", r)),
                    ProtectOutcome::Protected { plan, original_len, new_len, previous } => {
                        let again = previous.map(|p| trf!(" (was {0}%)", p)).unwrap_or_default();
                        let burst = plan.layout().map(|l| human_size(l.max_burst_bytes())).unwrap_or_default();
                        let text = trf!(
                            "Protected {0}%{1} · {2} · {3} → {4} · recovers up to {5} in a row",
                            plan.redundancy,
                            again,
                            plan.format.name(),
                            human_size(original_len),
                            human_size(new_len),
                            burst
                        );
                        (State::Clean, text)
                    }
                }
            }
            Job::Repair => match vnmf_guard::repair(path, None)? {
                RepairOutcome::Unprotected => unprotected(path)?,
                RepairOutcome::Intact(i) => describe(&i),
                RepairOutcome::Repaired { before, report, .. } => match report {
                    Some(r) if !r.is_complete() => (State::Lost, trf!("Partially repaired: {0} blocks, the rest is lost", r.repaired_data)),
                    Some(r) => (State::Clean, trf!("Repaired: {0} data blocks, {1} parity", r.repaired_data, r.repaired_parity)),
                    None => (State::Clean, trf!("Auxiliary fields repaired · {0}", before.plan.format.name())),
                },
            },
            Job::Strip => match vnmf_guard::strip(path, None)? {
                StripOutcome::Unprotected => unprotected(path)?,
                StripOutcome::Stripped { identical, len } => {
                    let note = if identical { tr("as before protection, byte for byte") } else { tr("bytes differ from the original") };
                    (State::Unprotected, trf!("Protection removed · {0} · {1}", human_size(len), note))
                }
            },
        })
    })();
    result.unwrap_or_else(|e| (State::Error, trf!("Error: {0}", format!("{:#}", e))))
}

fn unprotected(path: &Path) -> anyhow::Result<(State, String)> {
    let head = {
        use std::io::Read;
        let mut buf = vec![0u8; 0x8010];
        let n = std::fs::File::open(path)?.read(&mut buf)?;
        buf.truncate(n);
        buf
    };
    let size = human_size(std::fs::metadata(path)?.len());
    let text = match vnmf_guard::formats::detect(&head) {
        Some(f) => trf!("Not protected · {0} · {1}", f.name(), size),
        None => trf!("Not protected · unknown format · {0}", size),
    };
    Ok((State::Unprotected, text))
}

/// A file row.
struct Entry {
    path: PathBuf,
    row: adw::ActionRow,
    icon: gtk::Image,
    spinner: adw::Spinner,
}

impl Entry {
    fn set(&self, state: State, text: &str) {
        self.icon.set_icon_name(Some(state.icon()));
        for class in ["success", "warning", "error"] {
            self.icon.remove_css_class(class);
        }
        if let Some(class) = state.css() {
            self.icon.add_css_class(class);
        }
        self.row.set_subtitle(&glib::markup_escape_text(text));
    }

    fn busy(&self, busy: bool) {
        self.spinner.set_visible(busy);
        self.icon.set_visible(!busy);
    }
}

#[derive(Clone)]
struct GuardWindow(Rc<Inner>);

struct Inner {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    list: gtk::ListBox,
    redundancy: adw::SpinRow,
    toasts: adw::ToastOverlay,
    actions: gtk::Box,
    entries: RefCell<Vec<Rc<Entry>>>,
    busy: Cell<bool>,
}

impl GuardWindow {
    fn new(app: &adw::Application) -> Self {
        let header = adw::HeaderBar::new();
        let add = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text(tr("Add files (Ctrl+O)")).build();
        add.set_action_name(Some("win.open"));
        header.pack_start(&add);
        header.pack_end(&widgets::menu_button(&[
            (tr("Clear list"), "win.clear"),
            (tr("Supported formats"), "win.formats"),
            (tr("Language…"), "win.language"),
            (tr("About VNMF Guard"), "win.about"),
        ]));

        let (empty, empty_button) = widgets::open_status_page(
            "security-high-symbolic",
            tr("Protect files from damage"),
            tr(
                "Add videos, audio or disk images, or drag them into the window.\nParity goes inside the file so programs won't notice it, and damaged parts can be recovered.",
            ),
        );
        empty_button.set_action_name(Some("win.open"));

        let redundancy = adw::SpinRow::with_range(0.0, f64::from(vnmf_guard::vnmf_parity::MAX_REDUNDANCY), 1.0);
        redundancy.set_title(tr("Redundancy, %"));
        redundancy.set_subtitle(tr("How much to add to the file. 10% recovers ≈9% of the file in a row, 30% — ≈23%. 0 — change nothing"));
        redundancy.set_value(10.0);
        let settings_group = adw::PreferencesGroup::new();
        settings_group.add(&redundancy);

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        let files_group = adw::PreferencesGroup::builder().title(tr("Files")).description(tr("Modified in place (atomically, via a temporary file)")).build();
        files_group.add(&list);

        let page = adw::PreferencesPage::new();
        page.add(&settings_group);
        page.add(&files_group);

        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&page, Some("files"));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let actions = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).margin_top(8).margin_bottom(8).margin_start(12).margin_end(12).build();
        for (label, action, style) in [
            (tr("Check"), "win.check", None),
            (tr("Repair"), "win.repair", None),
            (tr("Remove protection"), "win.strip", Some("destructive-action")),
            (tr("Protect"), "win.protect", Some("suggested-action")),
        ] {
            let button = gtk::Button::builder().label(label).action_name(action).build();
            button.add_css_class("pill");
            if let Some(style) = style {
                button.add_css_class(style);
            }
            actions.append(&button);
        }
        let bottom = gtk::ScrolledWindow::builder().vscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).child(&actions).build();

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&toasts));
        toolbar.add_bottom_bar(&bottom);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("VNMF Guard")
            .default_width(760)
            .default_height(640)
            .width_request(360)
            .height_request(300)
            .content(&toolbar)
            .build();

        let this = Self(Rc::new(Inner { window, stack, list, redundancy, toasts, actions, entries: RefCell::new(Vec::new()), busy: Cell::new(false) }));
        this.install_actions();
        widgets::install_about(
            &this.0.window,
            "VNMF Guard",
            tr("Unobtrusive Reed–Solomon redundancy in video, audio and disk images: files open as usual, and damaged parts are recovered."),
        );
        // No restart offer: the file list (and a running job) would be lost.
        widgets::install_language(&this.0.window, widgets::LanguageRestart::NextStart);
        this.refresh();
        this
    }

    fn install_actions(&self) {
        let window = &self.0.window;
        let open = gio::SimpleAction::new("open", None);
        let this = self.clone();
        open.connect_activate(move |_, _| {
            let this = this.clone();
            glib::spawn_future_local(async move {
                let paths = choose_files(&this.0.window).await;
                this.open_many(paths);
            });
        });
        window.add_action(&open);

        for (name, job) in [("check", Job::Check), ("repair", Job::Repair), ("strip", Job::Strip), ("protect", Job::Protect(0))] {
            let action = gio::SimpleAction::new(name, None);
            let this = self.clone();
            action.connect_activate(move |_, _| {
                let job = match job {
                    Job::Protect(_) => Job::Protect(this.0.redundancy.value() as u8),
                    other => other,
                };
                this.run_all(job);
            });
            window.add_action(&action);
        }

        let clear = gio::SimpleAction::new("clear", None);
        let this = self.clone();
        clear.connect_activate(move |_, _| {
            if this.0.busy.get() {
                return;
            }
            for entry in this.0.entries.take() {
                this.0.list.remove(&entry.row);
            }
            this.refresh();
        });
        window.add_action(&clear);

        let formats = gio::SimpleAction::new("formats", None);
        let this = self.clone();
        formats.connect_activate(move |_, _| this.show_formats());
        window.add_action(&formats);

        let this = self.clone();
        window.add_controller(widgets::file_drop_target(move |paths| this.open_many(paths)));
    }

    /// Empty list — placeholder; actions are unavailable while working or without files.
    fn refresh(&self) {
        let has_files = !self.0.entries.borrow().is_empty();
        self.0.stack.set_visible_child_name(if has_files { "files" } else { "empty" });
        let enabled = has_files && !self.0.busy.get();
        for name in ["check", "repair", "strip", "protect", "clear"] {
            if let Some(action) = self.0.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        }
        self.0.actions.set_sensitive(enabled);
        self.0.redundancy.set_sensitive(!self.0.busy.get());
    }

    fn add_file(&self, path: PathBuf) -> Option<Rc<Entry>> {
        if !path.is_file() || self.0.entries.borrow().iter().any(|e| e.path == path) {
            return None;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&name)).subtitle(tr("Checking…")).build();
        row.set_tooltip_text(Some(&path.display().to_string()));
        let icon = gtk::Image::from_icon_name(State::Unknown.icon());
        let spinner = adw::Spinner::new();
        spinner.set_visible(false);
        row.add_prefix(&icon);
        row.add_prefix(&spinner);
        let remove = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text(tr("Remove from list")).valign(gtk::Align::Center).build();
        remove.add_css_class("flat");
        row.add_suffix(&remove);
        self.0.list.append(&row);

        let entry = Rc::new(Entry { path, row, icon, spinner });
        let (this, weak) = (self.clone(), Rc::downgrade(&entry));
        remove.connect_clicked(move |_| {
            let Some(entry) = weak.upgrade() else { return };
            if this.0.busy.get() {
                return;
            }
            this.0.list.remove(&entry.row);
            this.0.entries.borrow_mut().retain(|e| !Rc::ptr_eq(e, &entry));
            this.refresh();
        });
        self.0.entries.borrow_mut().push(entry.clone());
        Some(entry)
    }

    /// Performs an action on all files of the list one by one.
    fn run_all(&self, job: Job) {
        let entries = self.0.entries.borrow().clone();
        self.run(job, entries);
    }

    fn run(&self, job: Job, entries: Vec<Rc<Entry>>) {
        if self.0.busy.get() || entries.is_empty() {
            return;
        }
        if job == Job::Protect(0) {
            widgets::toast(&self.0.toasts, tr("Redundancy 0% — files not changed"));
            return;
        }
        self.0.busy.set(true);
        self.refresh();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let mut problems = 0;
            for entry in &entries {
                entry.busy(true);
                let path = entry.path.clone();
                let (state, text) = gio::spawn_blocking(move || run(job, &path)).await.unwrap_or_else(|_| (State::Error, tr("Processing crashed").into()));
                entry.busy(false);
                entry.set(state, &text);
                problems += usize::from(matches!(state, State::Damaged | State::Lost | State::Error));
            }
            this.0.busy.set(false);
            this.refresh();
            if job != Job::Check || problems > 0 {
                let done = match job {
                    Job::Check => tr("Check finished"),
                    Job::Protect(_) => tr("Protection finished"),
                    Job::Repair => tr("Repair finished"),
                    Job::Strip => tr("Protection removal finished"),
                };
                let message = if problems > 0 { trf!("{0}, problems: {1}", done, problems) } else { done.to_string() };
                widgets::toast(&this.0.toasts, &message);
            }
        });
    }

    fn show_formats(&self) {
        let body = Format::ALL
            .iter()
            .map(|f| {
                let ext = if f.extensions().is_empty() { tr("only with vnmfguard --raw").to_string() } else { f.extensions().join(", ") };
                format!("• {} ({ext}) — {}", f.name(), f.placement())
            })
            .collect::<Vec<_>>()
            .join("\n");
        let body = trf!("{0}\n\nCompress .img images to .zst or .gz: .xz has no room for foreign data.", body);
        let dialog = adw::AlertDialog::new(Some(tr("Supported formats")), Some(&body));
        dialog.add_response("close", tr("Close"));
        dialog.present(Some(&self.0.window));
    }
}

impl AppWindow for GuardWindow {
    const ALL_IN_ONE: bool = true;

    fn present(&self) {
        self.0.window.present();
    }

    fn gtk_window(&self) -> gtk::Window {
        self.0.window.clone().upcast()
    }

    fn open(&self, path: PathBuf) {
        self.open_many(vec![path]);
    }

    /// Adds files and checks them right away (just added — to see their state before any action).
    fn open_many(&self, paths: Vec<PathBuf>) {
        let added: Vec<Rc<Entry>> = paths.into_iter().filter_map(|p| self.add_file(p)).collect();
        self.refresh();
        if added.is_empty() {
            return;
        }
        if self.0.busy.get() {
            for entry in &added {
                entry.set(State::Unknown, tr("Added — check it after the current action finishes"));
            }
            return;
        }
        self.run(Job::Check, added);
    }
}

/// A dialog for choosing several files with a filter for supported formats.
async fn choose_files(parent: &adw::ApplicationWindow) -> Vec<PathBuf> {
    let supported = gtk::FileFilter::new();
    supported.set_name(Some(tr("Supported files")));
    for ext in Format::ALL.iter().flat_map(|f| f.extensions()) {
        supported.add_suffix(ext);
    }
    let all = gtk::FileFilter::new();
    all.set_name(Some(tr("All files")));
    all.add_pattern("*");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&supported);
    filters.append(&all);
    let dialog = gtk::FileDialog::builder().title(tr("Add files")).modal(true).filters(&filters).build();
    let Ok(files) = dialog.open_multiple_future(Some(parent)).await else {
        return Vec::new();
    };
    (0..files.n_items()).filter_map(|i| files.item(i).and_downcast::<gio::File>()?.path()).collect()
}
