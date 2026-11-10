// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! VNMF Creator — building a `.vnmf` from images: drag and drop, reordering, page roles,
//! volumes and chapters, ComicInfo metadata, parameters (webtoon, direction, redundancy) and saving
//! in the background.
//!
//! **Editing.** An opened `.vnmf` becomes a project (pages, roles, volumes and chapters, metadata,
//! parameters); it is always saved into a **new** file — the source stays as it was, and the new one
//! can be built with different redundancy, block size, etc.
//!
//! **Structure.** Pages form a single list; any page can start a new chapter
//! or volume (the marker with a number and title "travels" with the page when dragged). The first
//! page always starts a chapter (and a volume, if there are volumes). A folder with volumes/chapters or several CBZs
//! are split into chapters automatically.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use vnmf::comicinfo;
use vnmf::contents::{self, Contents};
use vnmf::metadata::{self, Metadata};
use vnmf_core::{AuxFlags, AuxFormat, AuxInput, AuxKind, BlockSize, Builder, ChunkSource, PageBitFlags, PageFormat, PageRole, spec};
use vnmf_gui::{AppWindow, audio, human_size, imaging, widgets};
use vnmf_i18n::{n_, ntrf, tr, trf};

const APP_ID: &str = "org.vnmf.Creator";
/// "Covers" options (indices in the list) and their hints.
const COVER_NONE: u32 = 0;
const COVER_AUTO: u32 = 1;
const COVER_CUSTOM: u32 = 2;
const COVER_HINTS: [&str; 3] = [
    n_("No previews; the thumbnail is the first page"),
    n_("Custom ones and pages with the \"Cover\" role; without them — the first page"),
    n_("Custom covers only"),
];
/// How many thumbnails to decode at once.
const THUMB_WORKERS: usize = 3;
const CARD_WIDTH: i32 = 150;
const CARD_HEIGHT: i32 = 210;

fn main() -> glib::ExitCode {
    vnmf_gui::run_app(APP_ID, CreatorWindow::new)
}

/// Role labels in the order of the `PageRole` codes.
const ROLE_LABELS: [&str; 11] = [
    n_("Regular"),
    n_("Cover"),
    n_("Back cover"),
    n_("Title page"),
    n_("Spread: left"),
    n_("Spread: right"),
    n_("Spread"),
    n_("Credits"),
    n_("Contents"),
    n_("Bonus"),
    n_("Advertisement"),
];

/// Rebuilding the contents of the attachments dialog.
type Refill = Rc<dyn Fn()>;
/// The sound currently being previewed in the attachments dialog: (attachment number, sound).
type Preview = Rc<RefCell<Option<(usize, audio::Sound)>>>;

/// A page in the project.
struct Item {
    id: u64,
    name: String,
    source: ChunkSource,
    format: PageFormat,
    width: u32,
    height: u32,
    role: PageRole,
    thumb: Option<gdk::Texture>,
    /// This page starts a new chapter (number and title).
    chapter: Option<Section>,
    /// This page starts a new volume.
    volume: Option<Section>,
    /// Attachments: music, effects, text, dark version, files. In a webtoon — of this fragment:
    /// the dark version replaces exactly it, and the effect trigger line is counted from its top.
    aux: Vec<AuxInput>,
    /// Image flags from the opened `.vnmf` (alpha, lossless, animation).
    flags: u16,
    /// The image CRC in the source `.vnmf` didn't match.
    damaged: bool,
}

/// A custom cover: an image from a file or a project page; it doesn't become a page.
#[derive(Clone)]
struct CoverItem {
    id: u64,
    name: String,
    source: ChunkSource,
    /// 0 — the file cover, N — the cover of volume N.
    volume: u16,
    thumb: Option<gdk::Texture>,
}

/// A page read in the background (not yet in the project).
struct Loaded {
    name: String,
    source: ChunkSource,
    format: PageFormat,
    width: u32,
    height: u32,
    role: PageRole,
    flags: u16,
    aux: Vec<AuxInput>,
    damaged: bool,
    chapter: Option<Section>,
    volume: Option<Section>,
}

/// Parameters of the opened `.vnmf` — for editing mode.
struct FileOptions {
    path: PathBuf,
    webtoon: bool,
    rtl: bool,
    redundancy: u8,
    block: BlockSize,
    tail: bool,
    cover: bool,
    /// Custom covers (not from pages) and their volumes.
    custom_previews: Vec<(ChunkSource, u16)>,
    /// Some of the previews are from pages.
    page_previews: bool,
    recovered: bool,
    unknown_info: bool,
    damaged: usize,
    unreadable: usize,
}

/// Number and title of a chapter or volume. An empty number means automatic.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct Section {
    number: String,
    title: String,
    /// Contents fields that aren't in the form (from the opened file): the date; for a volume also the ISBN.
    date: Option<String>,
    isbn: Option<String>,
}

impl Section {
    fn new(number: Option<String>, title: Option<String>) -> Self {
        Self { number: number.unwrap_or_default(), title: title.unwrap_or_default(), ..Default::default() }
    }

    fn number(&self) -> Option<String> {
        Some(self.number.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn title(&self) -> Option<String> {
        Some(self.title.trim().to_string()).filter(|s| !s.is_empty())
    }
}

/// A chapter of the future file: a range of project pages and numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChapterPlan {
    pages: std::ops::Range<usize>,
    volume_id: u16,
    chapter_id: u16,
    number: Option<String>,
    title: Option<String>,
    /// The chapter opens a volume (only if there are volumes).
    volume_start: bool,
    volume_title: Option<String>,
    date: Option<String>,
    volume_date: Option<String>,
    volume_isbn: Option<String>,
}

impl ChapterPlan {
    fn label(&self) -> String {
        let n = self.number.clone().unwrap_or_else(|| self.chapter_id.to_string());
        match &self.title {
            Some(t) => trf!("Chapter {0} — {1}", n, t),
            None => trf!("Chapter {0}", n),
        }
    }

    fn volume_label(&self) -> String {
        match &self.volume_title {
            Some(t) => trf!("Volume {0} — {1}", self.volume_id, t),
            None => trf!("Volume {0}", self.volume_id),
        }
    }
}

/// Splits pages into volumes and chapters by the `(chapter, volume)` markers of each page.
/// `default_chapter` — the number of a single chapter without its own number (the metadata "Number" field).
fn plan_structure(markers: &[(Option<&Section>, Option<&Section>)], default_chapter: Option<u16>) -> (Vec<ChapterPlan>, bool) {
    let has_volumes = markers.iter().any(|(_, v)| v.is_some());
    let mut plans: Vec<ChapterPlan> = Vec::new();
    let mut volume = 0u16;
    for (i, (chapter, vol)) in markers.iter().enumerate() {
        let volume_start = has_volumes && (i == 0 || vol.is_some());
        let (mut volume_title, mut volume_date, mut volume_isbn) = (None, None, None);
        if volume_start {
            let next = if i == 0 { 1 } else { volume.saturating_add(1) };
            volume = vol.and_then(|v| v.number()?.parse().ok()).unwrap_or(next);
            volume_title = vol.and_then(Section::title);
            volume_date = vol.and_then(|v| v.date.clone());
            volume_isbn = vol.and_then(|v| v.isbn.clone());
        }
        if i == 0 || volume_start || chapter.is_some() {
            plans.push(ChapterPlan {
                pages: i..i,
                volume_id: volume,
                chapter_id: 0,
                number: chapter.and_then(Section::number),
                title: chapter.and_then(Section::title),
                volume_start,
                volume_title,
                date: chapter.and_then(|c| c.date.clone()),
                volume_date,
                volume_isbn,
            });
        }
        plans.last_mut().expect("the first page opens a chapter").pages.end = i + 1;
    }
    let volumes: Vec<u16> = plans.iter().map(|p| p.volume_id).collect();
    let numbers: Vec<Option<String>> = plans.iter().map(|p| p.number.clone()).collect();
    let mut ids = contents::assign_chapter_ids(&volumes, &numbers);
    if let ([id], [None], Some(default)) = (ids.as_mut_slice(), numbers.as_slice(), default_chapter) {
        *id = default;
    }
    for (plan, id) in plans.iter_mut().zip(ids) {
        plan.chapter_id = id;
    }
    (plans, has_volumes)
}

/// Volume and chapter titles for Info.
fn plan_contents(plans: &[ChapterPlan]) -> Contents {
    let mut contents = Contents::default();
    for p in plans {
        contents.set_chapter(p.volume_id, p.chapter_id, p.number.clone(), p.title.clone());
        if p.date.is_some() {
            contents.chapter_mut(p.volume_id, p.chapter_id).date.clone_from(&p.date);
        }
        if p.volume_start {
            contents.set_volume_title(p.volume_id, p.volume_title.clone());
            let volume = contents.volume_mut(p.volume_id);
            volume.date.clone_from(&p.volume_date);
            volume.isbn.clone_from(&p.volume_isbn);
        }
    }
    contents.prune();
    contents
}

#[derive(Clone)]
struct CreatorWindow(Rc<Inner>);

struct Inner {
    window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    /// Grid: volume/chapter headers and a grid of cards for each chapter.
    sections: gtk::Box,
    /// Chapter grids and the cards of all pages in project order (for selection and thumbnails).
    flows: RefCell<Vec<gtk::FlowBox>>,
    cards: RefCell<Vec<gtk::FlowBoxChild>>,
    /// Numbers on the cards (the same order as `cards`) — for renumbering without a rebuild.
    badges: RefCell<Vec<gtk::Label>>,
    /// Page counters in the headers of each chapter: (of the volume, if the chapter opens it; of the chapter).
    counts: RefCell<Vec<(Option<gtk::Label>, Option<gtk::Label>)>>,
    /// Thumbnails currently being decoded (page ids), and whether a check of the visible ones is scheduled.
    thumbs_loading: RefCell<std::collections::HashSet<u64>>,
    thumbs_pending: Cell<bool>,
    save_button: gtk::Button,
    items: RefCell<Vec<Item>>,
    next_id: Cell<u64>,
    /// Full ComicInfo imported from CBZ/XML: unknown fields are kept as is.
    /// Imported metadata: the form edits its fields, the rest (identifiers outside the form,
    /// names in the original language, `extra`, unknown keys) is kept as is.
    base: RefCell<Metadata>,
    form: MetaForm,
    webtoon: adw::SwitchRow,
    rtl: adw::SwitchRow,
    redundancy: adw::SpinRow,
    block: adw::ComboRow,
    cover: adw::ComboRow,
    /// Custom covers (as many as you like) and the parameters group that shows them.
    covers: RefCell<Vec<CoverItem>>,
    covers_group: adw::PreferencesGroup,
    cover_rows: RefCell<Vec<gtk::Widget>>,
    tail: adw::SwitchRow,
    busy: Cell<bool>,
    /// Grid scroll position to restore after a rebuild (see `refresh`).
    grid: gtk::ScrolledWindow,
    pending_scroll: Cell<Option<f64>>,
    /// The file opened for editing (the new file is saved next to it under a different name).
    editing: RefCell<Option<PathBuf>>,
    /// All `.vnmf` files pages were taken from: they can't be overwritten by the new file.
    sources: RefCell<Vec<PathBuf>>,
    /// The "to the end" zone for dragging chapters and volumes (visible only while dragging).
    end_zone: RefCell<Option<gtk::Widget>>,
}

impl CreatorWindow {
    fn new(app: &adw::Application) -> Self {
        let title = adw::WindowTitle::new("VNMF Creator", tr("New volume"));
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));

        let add_button =
            adw::SplitButton::builder().label(tr("Add")).tooltip_text(tr("Add images, CBZ or .vnmf (Ctrl+O)")).action_name("win.add-files").build();
        let add_menu = gio::Menu::new();
        add_menu.append(Some(tr("Open .vnmf for editing…")), Some("win.open-vnmf"));
        add_menu.append(Some(tr("Images, CBZ or .vnmf…")), Some("win.add-files"));
        add_menu.append(Some(tr("Folder…")), Some("win.add-folder"));
        add_menu.append(Some(tr("Import ComicInfo.xml / JSON…")), Some("win.import-info"));
        add_button.set_menu_model(Some(&add_menu));
        header.pack_start(&add_button);

        let save_button = gtk::Button::builder().label(tr("Save…")).tooltip_text(tr("Save .vnmf (Ctrl+S)")).build();
        save_button.add_css_class("suggested-action");
        save_button.set_action_name(Some("win.save"));
        save_button.set_sensitive(false);
        let sidebar_toggle =
            gtk::ToggleButton::builder().icon_name("document-properties-symbolic").tooltip_text(tr("Metadata and parameters (F9)")).active(true).build();
        header.pack_end(&widgets::menu_button(&[
            (tr("Clear project"), "win.clear"),
            (tr("Language…"), "win.language"),
            (tr("About VNMF Creator"), "win.about"),
        ]));
        header.pack_end(&save_button);
        header.pack_end(&sidebar_toggle);

        // Empty state.
        let empty = adw::StatusPage::builder()
            .icon_name("insert-image-symbolic")
            .title(tr("Drag pages here"))
            .description(tr("Images (PNG, JPEG, WebP, AVIF, JPEG XL, GIF), folders, CBZ or .vnmf.\nThe order can be changed by dragging cards, chapters and volumes.\nA .vnmf is opened for editing and saved into a new file."))
            .build();
        let empty_add = gtk::Button::builder().label(tr("Add files…")).halign(gtk::Align::Center).build();
        empty_add.add_css_class("pill");
        empty_add.add_css_class("suggested-action");
        empty_add.set_action_name(Some("win.add-files"));
        empty.set_child(Some(&empty_add));

        // Page grid: a grid per chapter, with volume and chapter headers above them.
        let sections = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::Start)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        // FIX: no autoscroll to focus. After the grid is rebuilt, focus lands on a card that isn't
        // laid out yet (or on the first one), and GTK scrolled to it — to the start of the project.
        // We hold the grid position ourselves (`refresh`), and scroll to the selected card explicitly (`select`).
        let viewport = gtk::Viewport::builder().scroll_to_focus(false).child(&sections).build();
        let grid = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&viewport).build();

        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vexpand(true).build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&grid, Some("pages"));

        // Sidebar: metadata and parameters.
        let (sidebar, form, opts) = build_sidebar();

        let split = adw::OverlaySplitView::builder()
            .sidebar(&sidebar)
            .content(&stack)
            .sidebar_position(gtk::PackType::End)
            .min_sidebar_width(320.0)
            .max_sidebar_width(420.0)
            .build();
        sidebar_toggle.bind_property("active", &split, "show-sidebar").bidirectional().sync_create().build();

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&toasts));

        let window = adw::ApplicationWindow::builder().application(app).title("VNMF Creator").default_width(1300).default_height(850).content(&toolbar).build();

        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, 800.0, adw::LengthUnit::Sp));
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        let this = Self(Rc::new(Inner {
            window,
            title,
            toasts,
            stack,
            sections,
            flows: RefCell::new(Vec::new()),
            cards: RefCell::new(Vec::new()),
            badges: RefCell::new(Vec::new()),
            counts: RefCell::new(Vec::new()),
            thumbs_loading: RefCell::new(std::collections::HashSet::new()),
            thumbs_pending: Cell::new(false),
            save_button,
            items: RefCell::new(Vec::new()),
            next_id: Cell::new(1),
            base: RefCell::new(Metadata::new()),
            form,
            webtoon: opts.webtoon,
            rtl: opts.rtl,
            redundancy: opts.redundancy,
            block: opts.block,
            cover: opts.cover,
            covers: RefCell::new(Vec::new()),
            covers_group: opts.covers_group,
            cover_rows: RefCell::new(Vec::new()),
            tail: opts.tail,
            busy: Cell::new(false),
            grid: grid.clone(),
            pending_scroll: Cell::new(None),
            editing: RefCell::new(None),
            sources: RefCell::new(Vec::new()),
            end_zone: RefCell::new(None),
        }));
        this.install_actions(app);
        widgets::install_about(&this.0.window, "VNMF Creator", tr("Creating VNMF files from images, folders and CBZ."));
        // No restart offer: it would throw away the project being assembled.
        widgets::install_language(&this.0.window, widgets::LanguageRestart::NextStart);

        // Redundancy requires the metadata backup copy — the switch is locked.
        let tail = this.0.tail.clone();
        this.0.redundancy.connect_value_notify(move |spin| {
            let on = spin.value() > 0.0;
            if on {
                tail.set_active(true);
            }
            tail.set_sensitive(!on);
        });
        this.0.redundancy.notify("value");

        // Restoring the scroll after a rebuild: both when the content height changed (`changed`),
        // and when GTK itself shifted the scroll while removing old cards (the height is the same — `changed`
        // doesn't arrive then).
        let restore = {
            let me = this.clone();
            move |adj: &gtk::Adjustment| {
                if let Some(target) = me.0.pending_scroll.get()
                    && adj.upper() - adj.page_size() + 0.5 >= target
                {
                    me.0.pending_scroll.set(None);
                    adj.set_value(target);
                }
            }
        };
        this.0.grid.vadjustment().connect_changed(restore.clone());
        this.0.grid.vadjustment().connect_value_changed(restore);
        // Thumbnails — only for visible cards (and after a layout change).
        let me = this.clone();
        this.0.grid.vadjustment().connect_value_changed(move |_| me.schedule_thumbs());
        let me = this.clone();
        this.0.grid.vadjustment().connect_changed(move |_| me.schedule_thumbs());

        // Autoscroll while dragging near the top / bottom edge of the grid — to
        // move a chapter or volume far away (to the start of a large project).
        let speed = Rc::new(Cell::new(0.0f64));
        let ticking = Rc::new(Cell::new(false));
        let motion = gtk::DropControllerMotion::new();
        // FIX: in the bubble phase, motion over cards and headers (they're drop targets themselves)
        // didn't reach here, and autoscroll down didn't work. Capture sees the motion first.
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (sp, grid_weak) = (speed.clone(), this.0.grid.downgrade());
        motion.connect_motion(move |m, _, y| {
            const EDGE: f64 = 80.0;
            let height = m.widget().map_or(0.0, |w| f64::from(w.height()));
            let v = if y < EDGE {
                -(EDGE - y)
            } else if y > height - EDGE {
                y - (height - EDGE)
            } else {
                0.0
            };
            sp.set(v / 3.0);
            if v == 0.0 || ticking.replace(true) {
                return;
            }
            let (sp, grid_weak, ticking) = (sp.clone(), grid_weak.clone(), ticking.clone());
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                let grid = grid_weak.upgrade();
                if sp.get() == 0.0 || grid.is_none() {
                    ticking.set(false);
                    return glib::ControlFlow::Break;
                }
                let adj = grid.expect("checked").vadjustment();
                adj.set_value((adj.value() + sp.get()).clamp(0.0, adj.upper() - adj.page_size()));
                glib::ControlFlow::Continue
            });
        });
        motion.connect_leave(move |_| speed.set(0.0));
        this.0.grid.add_controller(motion);

        // In webtoon mode page roles aren't used — update the cards.
        let me = this.clone();
        this.0.webtoon.connect_active_notify(move |_| me.refresh());
        this
    }

    // ------------------------------------------------------------------
    // Actions
    // ------------------------------------------------------------------

    fn action(&self, name: &str, f: impl Fn(&Self) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let this = self.clone();
        action.connect_activate(move |_, _| f(&this));
        self.0.window.add_action(&action);
    }

    fn install_actions(&self, app: &adw::Application) {
        self.action("add-files", |this| {
            let this = this.clone();
            glib::spawn_future_local(async move {
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(tr("Images, CBZ and VNMF")));
                for pattern in ["*.png", "*.jpg", "*.jpeg", "*.webp", "*.avif", "*.jxl", "*.gif", "*.cbz", "*.zip", "*.vnmf", "*.xml", "*.json"] {
                    filter.add_pattern(pattern);
                    filter.add_pattern(&pattern.to_uppercase());
                }
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                let dialog = gtk::FileDialog::builder().title(tr("Add pages")).filters(&filters).modal(true).build();
                if let Ok(files) = dialog.open_multiple_future(Some(&this.0.window)).await {
                    let paths = files.iter::<gio::File>().filter_map(|f| f.ok()?.path()).collect();
                    this.add_paths(paths);
                }
            });
        });
        self.action("open-vnmf", |this| {
            let this = this.clone();
            glib::spawn_future_local(async move {
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(tr("VNMF files")));
                filter.add_mime_type(vnmf_gui::MIME_TYPE);
                filter.add_pattern("*.vnmf");
                filter.add_pattern("*.VNMF");
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                let dialog = gtk::FileDialog::builder().title(tr("Open .vnmf for editing")).filters(&filters).modal(true).build();
                let Ok(file) = dialog.open_future(Some(&this.0.window)).await else { return };
                let Some(path) = file.path() else { return };
                if this.0.items.borrow().is_empty() {
                    return this.add_paths(vec![path]);
                }
                // The project isn't empty: replace it with the file or append the pages at the end.
                let dialog = adw::AlertDialog::new(
                    Some(tr("Open for editing?")),
                    Some(tr("The current project will be replaced by the file's contents. Or the file's pages can be added to the end of the project.")),
                );
                dialog.add_response("cancel", tr("Cancel"));
                dialog.add_response("append", tr("Add to the end"));
                dialog.add_response("open", tr("Replace"));
                dialog.set_response_appearance("open", adw::ResponseAppearance::Destructive);
                dialog.set_close_response("cancel");
                let response = dialog.choose_future(Some(&this.0.window)).await;
                match response.as_str() {
                    "open" => {
                        this.clear();
                        this.add_paths(vec![path]);
                    }
                    "append" => this.add_paths(vec![path]),
                    _ => {}
                }
            });
        });
        self.action("add-covers", |this| {
            let this = this.clone();
            glib::spawn_future_local(async move { this.choose_covers().await });
        });
        self.action("add-folder", |this| {
            let this = this.clone();
            glib::spawn_future_local(async move {
                let dialog = gtk::FileDialog::builder().title(tr("Add a folder of pages")).modal(true).build();
                if let Ok(folder) = dialog.select_folder_future(Some(&this.0.window)).await
                    && let Some(path) = folder.path()
                {
                    this.add_paths(vec![path]);
                }
            });
        });
        self.action("import-info", |this| {
            let this = this.clone();
            glib::spawn_future_local(async move {
                let dialog = gtk::FileDialog::builder().title(tr("Import metadata")).modal(true).build();
                if let Ok(file) = dialog.open_future(Some(&this.0.window)).await
                    && let Some(path) = file.path()
                {
                    this.import_info(&path);
                }
            });
        });
        self.action("save", |this| {
            let this = this.clone();
            glib::spawn_future_local(async move { this.save().await });
        });
        self.action("clear", |this| {
            if this.0.items.borrow().is_empty() {
                return;
            }
            let dialog = adw::AlertDialog::new(Some(tr("Clear the project?")), Some(tr("All pages and metadata will be removed.")));
            dialog.add_response("cancel", tr("Cancel"));
            dialog.add_response("clear", tr("Clear"));
            dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
            dialog.set_close_response("cancel");
            let me = this.clone();
            dialog.connect_response(Some("clear"), move |_, _| me.clear());
            dialog.present(Some(&this.0.window));
        });
        // Page role: parameter — (page id, role code).
        let role = gio::SimpleAction::new("role", Some(&<(u64, u8)>::static_variant_type()));
        let this = self.clone();
        role.connect_activate(move |_, param| {
            let Some((id, code)) = param.and_then(|p| p.get::<(u64, u8)>()) else { return };
            let role = PageRole::try_from(code).unwrap_or(PageRole::Normal);
            if let Some(item) = this.0.items.borrow_mut().iter_mut().find(|i| i.id == id) {
                item.role = role;
            }
            // A role changes only one card — without rebuilding the grid.
            if let Some(index) = this.index_of(id) {
                this.update_card(index);
            }
        });
        self.0.window.add_action(&role);

        // Page attachments (sound, text, dark version): parameter — page id.
        let page_aux = gio::SimpleAction::new("page-aux", Some(&u64::static_variant_type()));
        let this = self.clone();
        page_aux.connect_activate(move |_, param| {
            if let Some(id) = param.and_then(|p| p.get::<u64>()) {
                this.edit_aux(id);
            }
        });
        self.0.window.add_action(&page_aux);

        // Page: remove / move by `delta` positions. Parameter — page id
        // (for keyboard shortcuts and automation; card buttons call the same methods directly).
        let remove_page = gio::SimpleAction::new("remove-page", Some(&u64::static_variant_type()));
        let this = self.clone();
        remove_page.connect_activate(move |_, param| {
            if let Some(i) = param.and_then(|p| p.get::<u64>()).and_then(|id| this.index_of(id)) {
                this.remove(i);
            }
        });
        self.0.window.add_action(&remove_page);
        let shift_page = gio::SimpleAction::new("shift-page", Some(&<(u64, i32)>::static_variant_type()));
        let this = self.clone();
        shift_page.connect_activate(move |_, param| {
            let Some((id, delta)) = param.and_then(|p| p.get::<(u64, i32)>()) else { return };
            if let Some(i) = this.index_of(id) {
                this.move_item(i, i.saturating_add_signed(delta as isize));
            }
        });
        self.0.window.add_action(&shift_page);

        // Structure: parameter — page id.
        let chapter_break = gio::SimpleAction::new("chapter-break", Some(&u64::static_variant_type()));
        let this = self.clone();
        chapter_break.connect_activate(move |_, param| {
            if let Some(id) = param.and_then(|p| p.get::<u64>()) {
                this.toggle_break(id, false);
            }
        });
        self.0.window.add_action(&chapter_break);
        let volume_break = gio::SimpleAction::new("volume-break", Some(&u64::static_variant_type()));
        let this = self.clone();
        volume_break.connect_activate(move |_, param| {
            if let Some(id) = param.and_then(|p| p.get::<u64>()) {
                this.toggle_break(id, true);
            }
        });
        self.0.window.add_action(&volume_break);

        // Moving a chapter or volume: (id of the first page, volume?, id of the page to put it
        // before — 0 means "to the end", is the target a volume header?). The same as dragging a header.
        let move_group = gio::SimpleAction::new("move-group", Some(&<(u64, bool, u64, bool)>::static_variant_type()));
        let this = self.clone();
        move_group.connect_activate(move |_, param| {
            if let Some((start, volume, before, header)) = param.and_then(|p| p.get::<(u64, bool, u64, bool)>()) {
                this.move_group(start, volume, (before != 0).then_some(before), header);
            }
        });
        self.0.window.add_action(&move_group);

        self.action("remove-selected", |this| {
            if let Some(i) = this.selected_index() {
                this.remove(i);
            }
        });
        self.action("move-left", |this| {
            if let Some(i) = this.selected_index() {
                this.move_item(i, i.saturating_sub(1));
            }
        });
        self.action("move-right", |this| {
            if let Some(i) = this.selected_index() {
                this.move_item(i, i + 1);
            }
        });

        app.set_accels_for_action("win.add-files", &["<Ctrl>o"]);
        app.set_accels_for_action("win.save", &["<Ctrl>s"]);
        app.set_accels_for_action("win.remove-selected", &["Delete"]);
        app.set_accels_for_action("win.move-left", &["<Ctrl>Left"]);
        app.set_accels_for_action("win.move-right", &["<Ctrl>Right"]);

        // External files — anywhere in the window.
        let this = self.clone();
        self.0.window.add_controller(widgets::file_drop_target(move |paths| this.add_paths(paths)));
    }

    // ------------------------------------------------------------------
    // Adding pages
    // ------------------------------------------------------------------

    fn add_paths(&self, paths: Vec<PathBuf>) {
        let this = self.clone();
        // A single `.vnmf` into an empty project — editing mode: take the file's parameters too.
        let single = paths.len() == 1;
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || -> anyhow::Result<_> {
                let mut pages = Vec::new();
                let mut meta: Option<Metadata> = None;
                let mut files = Vec::new();
                let mut skipped = 0;
                // Regular inputs (images, folders, CBZ) between `.vnmf` files — as one group.
                let mut run: Vec<PathBuf> = Vec::new();
                let flush = |run: &mut Vec<PathBuf>, pages: &mut Vec<Loaded>, meta: &mut Option<Metadata>, skipped: &mut usize| -> anyhow::Result<()> {
                    if run.is_empty() {
                        return Ok(());
                    }
                    let (loaded, m, s) = load_inputs(&std::mem::take(run))?;
                    pages.extend(loaded);
                    if meta.is_none() {
                        *meta = m;
                    }
                    *skipped += s;
                    Ok(())
                };
                for path in paths {
                    if path.is_file() && vnmf::edit::is_vnmf(&path) {
                        flush(&mut run, &mut pages, &mut meta, &mut skipped)?;
                        let (loaded, m, opts) = load_vnmf(&path)?;
                        pages.extend(loaded);
                        if meta.is_none() {
                            meta = m;
                        }
                        files.push(opts);
                    } else {
                        run.push(path);
                    }
                }
                flush(&mut run, &mut pages, &mut meta, &mut skipped)?;
                Ok((pages, meta, files, skipped))
            })
            .await;

            let (pages, meta, files, skipped) = match result {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return widgets::toast(&this.0.toasts, &trf!("Could not add: {0}", format!("{:#}", e))),
                Err(_) => return widgets::toast(&this.0.toasts, tr("Adding crashed")),
            };

            let added = pages.len();
            let was_empty = this.0.items.borrow().is_empty();
            let editing = was_empty && single && files.len() == 1;
            for (k, page) in pages.into_iter().enumerate() {
                // The first page of an empty project starts a chapter anyway: a marker is needed
                // only if there's a number or a title.
                let chapter = page.chapter.filter(|c| !(was_empty && k == 0) || *c != Section::default());
                let id = this.0.next_id.get();
                this.0.next_id.set(id + 1);
                let name = page.name.rsplit(['/', '\\']).next().unwrap_or(&page.name).to_string();
                this.0.items.borrow_mut().push(Item {
                    id,
                    name,
                    source: page.source,
                    format: page.format,
                    width: page.width,
                    height: page.height,
                    role: page.role,
                    thumb: None,
                    chapter,
                    volume: page.volume,
                    aux: page.aux,
                    flags: page.flags,
                    damaged: page.damaged,
                });
            }
            this.0.sources.borrow_mut().extend(files.iter().map(|f| f.path.clone()));
            // `.vnmf` metadata — only for an empty project (appending doesn't touch the form).
            let from_file = !files.is_empty();
            if let Some(meta) = meta.filter(|m| !m.is_empty() && (was_empty || !from_file)) {
                this.apply_metadata(meta);
                if !editing {
                    widgets::toast(&this.0.toasts, tr("Metadata imported"));
                }
            }
            if editing {
                this.start_editing(&files[0]);
            }
            for f in &files {
                let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if f.recovered {
                    widgets::toast(&this.0.toasts, &trf!("{0}: the primary metadata is damaged — the backup copy was read", name));
                }
                if f.damaged > 0 {
                    widgets::toast(&this.0.toasts, &trf!("{0}: damaged images — {1} (marked in red)", name, f.damaged));
                }
                if f.unreadable > 0 {
                    widgets::toast(&this.0.toasts, &trf!("{0}: pages that couldn't be read — {1}", name, f.unreadable));
                }
                if f.unknown_info {
                    widgets::toast(&this.0.toasts, &trf!("{0}: Info is neither VNMF nor ComicInfo — it won't make it into the new file", name));
                }
            }
            if skipped > 0 {
                widgets::toast(&this.0.toasts, &trf!("Skipped files that aren't images: {0}", skipped));
            }
            if added == 0 {
                widgets::toast(&this.0.toasts, tr("No images found"));
            }
            // Thumbnails are decoded only for visible cards (`schedule_thumbs`).
            this.refresh();
        });
    }

    /// Editing mode: save parameters as in the opened file, the title is its name.
    fn start_editing(&self, file: &FileOptions) {
        self.0.webtoon.set_active(file.webtoon);
        self.0.rtl.set_active(file.rtl);
        self.0.redundancy.set_value(f64::from(file.redundancy));
        self.0.tail.set_active(file.tail);
        self.0.covers.borrow_mut().clear();
        for (k, (source, volume)) in file.custom_previews.iter().enumerate() {
            self.push_cover(source.clone(), trf!("From file · {0}", k + 1), *volume);
        }
        self.0.cover.set_selected(match (file.cover, file.page_previews) {
            (false, _) => COVER_NONE,
            (true, true) => COVER_AUTO,
            (true, false) => COVER_CUSTOM,
        });
        self.rebuild_covers();
        let block = [BlockSize::Page4k, BlockSize::Page8k, BlockSize::Page16k, BlockSize::Ext32k];
        self.0.block.set_selected(block.iter().position(|b| *b == file.block).unwrap_or(0) as u32);
        let name = file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.0.title.set_title(&trf!("Editing: {0}", name));
        self.0.window.set_title(Some(&format!("{name} — VNMF Creator")));
        *self.0.editing.borrow_mut() = Some(file.path.clone());
        widgets::toast(&self.0.toasts, tr("Changes will be saved into a new file — the source stays unchanged"));
    }

    /// Adds a custom cover at the end of the list (without rebuilding the rows — see `rebuild_covers`).
    fn push_cover(&self, source: ChunkSource, name: String, volume: u16) {
        let id = self.0.next_id.get();
        self.0.next_id.set(id + 1);
        self.0.covers.borrow_mut().push(CoverItem { id, name, source, volume, thumb: None });
    }

    /// The first custom cover was added while covers are off — turn them on (otherwise it won't be in the file).
    fn covers_added(&self) {
        if self.0.cover.selected() == COVER_NONE {
            self.0.cover.set_selected(COVER_AUTO);
        }
        self.rebuild_covers();
    }

    /// Dialog for choosing custom covers: several images at once, they don't become pages.
    async fn choose_covers(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(tr("Images")));
        for pattern in ["*.png", "*.jpg", "*.jpeg", "*.webp", "*.avif", "*.jxl", "*.gif"] {
            filter.add_pattern(pattern);
            filter.add_pattern(&pattern.to_uppercase());
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title(tr("Custom covers")).filters(&filters).modal(true).build();
        let Ok(files) = dialog.open_multiple_future(Some(&self.0.window)).await else { return };
        let paths: Vec<PathBuf> = files.iter::<gio::File>().filter_map(|f| f.ok()?.path()).collect();
        let loaded = gio::spawn_blocking(move || {
            paths
                .into_iter()
                .map(|path| -> Result<(ChunkSource, String), String> {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let source = ChunkSource::from_file(&path).map_err(|e| format!("{name}: {e}"))?;
                    match source.probe_image() {
                        Ok((w, h, format)) if format.is_known() && w > 0 && h > 0 => Ok((source, name)),
                        _ => Err(trf!("{0}: this is not a supported image", name)),
                    }
                })
                .collect::<Vec<_>>()
        })
        .await;
        let Ok(loaded) = loaded else { return widgets::toast(&self.0.toasts, tr("Reading the images crashed")) };
        let mut added = 0;
        for result in loaded {
            match result {
                Ok((source, name)) => {
                    self.push_cover(source, name, 0);
                    added += 1;
                }
                Err(e) => widgets::toast(&self.0.toasts, &trf!("Could not add the cover — {0}", e)),
            }
        }
        if added > 0 {
            self.covers_added();
        }
    }

    /// Where a cover can be attached: the file and each volume of the project — (volume_id, title).
    fn cover_targets(&self) -> Vec<(u16, String)> {
        let (plans, has_volumes) = self.plan();
        let mut targets = vec![(0, tr("File").to_string())];
        if has_volumes {
            for plan in plans.iter().filter(|p| p.volume_start) {
                let title = plan.volume_title.as_deref().filter(|t| !t.is_empty());
                targets.push((plan.volume_id, title.map_or_else(|| trf!("Volume {0}", plan.volume_id), |t| trf!("Volume {0}: {1}", plan.volume_id, t))));
            }
        }
        targets
    }

    /// Custom cover rows in the parameters (after any change of the list or the volume structure).
    fn rebuild_covers(&self) {
        let group = &self.0.covers_group;
        for row in self.0.cover_rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        group.set_visible(self.0.cover.selected() != COVER_NONE);
        let mut targets = self.cover_targets();
        // A volume that's no longer in the project (pages were removed) stays selected, but marked.
        for cover in self.0.covers.borrow().iter() {
            if !targets.iter().any(|(v, _)| *v == cover.volume) {
                targets.push((cover.volume, trf!("Volume {0} (not in the project)", cover.volume)));
            }
        }
        let labels: Vec<&str> = targets.iter().map(|(_, l)| l.as_str()).collect();
        let covers = self.0.covers.borrow().clone();
        let mut rows = Vec::new();
        if covers.is_empty() {
            let row = adw::ActionRow::builder().title(tr("None")).subtitle(tr("Add images with the \"+\" button")).build();
            row.add_css_class("dim-label");
            rows.push(row.upcast::<gtk::Widget>());
        }
        for (k, cover) in covers.iter().enumerate() {
            let (w, h, format) = cover.source.probe_image().unwrap_or((0, 0, PageFormat::Unknown));
            let selected = targets.iter().position(|(v, _)| *v == cover.volume).unwrap_or(0);
            let main = !covers[..k].iter().any(|c| c.volume == cover.volume);
            let role = format!("{}{}", targets[selected].1, if main { "" } else { tr(" · additional") });
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&cover.name))
                .title_lines(1)
                .subtitle(glib::markup_escape_text(&format!("{role}\n{w}×{h} · {format} · {}", human_size(u64::from(cover.source.size)))))
                .subtitle_lines(2)
                .tooltip_text(&cover.name)
                .build();
            let picture = gtk::Picture::builder().can_shrink(true).content_fit(gtk::ContentFit::Contain).build();
            picture.set_size_request(36, 52);
            match &cover.thumb {
                Some(texture) => picture.set_paintable(Some(texture)),
                None => {
                    let (this, id, source, picture) = (self.clone(), cover.id, cover.source.clone(), picture.clone());
                    let width = 52 * self.0.window.scale_factor().clamp(1, 3);
                    glib::spawn_future_local(async move {
                        if let Ok(texture) = imaging::thumbnail(source, width).await {
                            picture.set_paintable(Some(&texture));
                            if let Some(c) = this.0.covers.borrow_mut().iter_mut().find(|c| c.id == id) {
                                c.thumb = Some(texture);
                            }
                        }
                    });
                }
            }
            row.add_prefix(&picture);

            // Row menu: what it's a cover of, order, remove (there's little room in the narrow panel).
            let target = gtk::DropDown::from_strings(&labels);
            target.set_selected(selected as u32);
            target.set_sensitive(targets.len() > 1);
            let (this, id, volumes) = (self.clone(), cover.id, targets.iter().map(|(v, _)| *v).collect::<Vec<_>>());
            target.connect_selected_notify(move |d| {
                if let Some(&volume) = volumes.get(d.selected() as usize)
                    && let Some(c) = this.0.covers.borrow_mut().iter_mut().find(|c| c.id == id)
                {
                    c.volume = volume;
                }
                // After the menu closes: the row (and the menu with it) is rebuilt.
                let this = this.clone();
                glib::idle_add_local_once(move || this.rebuild_covers());
            });
            let up = gtk::Button::builder().label(tr("Up")).tooltip_text(tr("The first cover of a file or volume is the main one")).sensitive(k > 0).build();
            up.add_css_class("flat");
            let remove = gtk::Button::builder().label(tr("Remove")).build();
            remove.add_css_class("flat");
            remove.add_css_class("destructive-action");
            let content =
                gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
            content.append(&gtk::Label::builder().label(tr("Cover for")).xalign(0.0).css_classes(["dim-label", "caption"]).build());
            content.append(&target);
            content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            content.append(&up);
            content.append(&remove);
            let popover = gtk::Popover::builder().child(&content).build();
            let menu = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .tooltip_text(tr("Cover for…, order, remove"))
                .valign(gtk::Align::Center)
                .popover(&popover)
                .build();
            menu.add_css_class("flat");
            let (this, id, pop) = (self.clone(), cover.id, popover.clone());
            up.connect_clicked(move |_| {
                pop.popdown();
                {
                    let mut covers = this.0.covers.borrow_mut();
                    if let Some(i) = covers.iter().position(|c| c.id == id).filter(|&i| i > 0) {
                        covers.swap(i, i - 1);
                    }
                }
                let this = this.clone();
                glib::idle_add_local_once(move || this.rebuild_covers());
            });
            let (this, id, pop) = (self.clone(), cover.id, popover.clone());
            remove.connect_clicked(move |_| {
                pop.popdown();
                this.0.covers.borrow_mut().retain(|c| c.id != id);
                let this = this.clone();
                glib::idle_add_local_once(move || this.rebuild_covers());
            });
            row.add_suffix(&menu);
            row.set_activatable_widget(Some(&menu));
            rows.push(row.upcast());
        }
        for row in &rows {
            group.add(row);
        }
        *self.0.cover_rows.borrow_mut() = rows;
    }

    /// An empty project: no pages, metadata or editing mode.
    fn clear(&self) {
        self.0.items.borrow_mut().clear();
        self.0.sources.borrow_mut().clear();
        self.0.covers.borrow_mut().clear();
        self.rebuild_covers();
        *self.0.editing.borrow_mut() = None;
        *self.0.base.borrow_mut() = Metadata::new();
        self.0.form.fill(&Metadata::new());
        self.0.title.set_title("VNMF Creator");
        self.0.window.set_title(Some("VNMF Creator"));
        self.refresh();
    }

    /// Importing metadata from a file: vnmf.json, ComicInfo.xml or format 1.1 Info.
    fn import_info(&self, path: &std::path::Path) {
        let parsed = std::fs::read(path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Metadata::from_info(Some(&bytes)).ok_or_else(|| anyhow::anyhow!(tr("this is neither vnmf.json nor ComicInfo.xml"))));
        match parsed {
            Ok(meta) => {
                self.apply_metadata(meta);
                widgets::toast(&self.0.toasts, tr("Metadata imported"));
            }
            Err(e) => widgets::toast(&self.0.toasts, &trf!("Could not import: {0}", format!("{:#}", e))),
        }
    }

    /// Fills the form; fields outside the form are kept for writing.
    fn apply_metadata(&self, meta: Metadata) {
        self.0.form.fill(&meta);
        if meta.is_rtl() {
            self.0.rtl.set_active(true);
        }
        *self.0.base.borrow_mut() = meta;
    }

    // ------------------------------------------------------------------
    // Grid
    // ------------------------------------------------------------------

    fn selected_index(&self) -> Option<usize> {
        self.0.cards.borrow().iter().position(gtk::FlowBoxChild::is_selected)
    }

    fn remove(&self, index: usize) {
        // One of several cards of a chapter — remove only it (without rebuilding the grid).
        let child = self.0.cards.borrow().get(index).cloned();
        let flow = child.as_ref().and_then(|c| c.parent()).and_downcast::<gtk::FlowBox>();
        let incremental = flow.as_ref().is_some_and(|f| f.first_child() != f.last_child());
        {
            let mut items = self.0.items.borrow_mut();
            if index < items.len() {
                let removed = items.remove(index);
                // The chapter/volume start moves to the next page — the chapter doesn't disappear.
                if let Some(next) = items.get_mut(index) {
                    if next.chapter.is_none() {
                        next.chapter = removed.chapter;
                    }
                    if next.volume.is_none() {
                        next.volume = removed.volume;
                    }
                }
            }
        }
        match (incremental, child, flow) {
            (true, Some(child), Some(flow)) => {
                flow.remove(&child);
                self.0.cards.borrow_mut().remove(index);
                self.0.badges.borrow_mut().remove(index);
                self.renumber(index..self.0.badges.borrow().len());
                if !self.sync_counts() {
                    self.refresh();
                }
                self.schedule_thumbs();
            }
            _ => self.refresh(),
        }
        let n = self.0.items.borrow().len();
        if n > 0 {
            self.select(index.min(n - 1));
        }
    }

    /// Card numbers in the range — by their place in the project.
    fn renumber(&self, range: std::ops::Range<usize>) {
        for (i, badge) in self.0.badges.borrow().iter().enumerate().skip(range.start).take(range.len()) {
            badge.set_label(&(i + 1).to_string());
        }
    }

    /// Updates the counters in the headers and the window subtitle if the split into chapters is the same
    /// as in the grid. `false` — the structure changed, a full rebuild is needed.
    fn sync_counts(&self) -> bool {
        let (plans, has_volumes) = self.plan();
        {
            let counts = self.0.counts.borrow();
            let flows = self.0.flows.borrow();
            if counts.len() != plans.len() || flows.len() != plans.len() {
                return false;
            }
            for (plan, flow) in plans.iter().zip(flows.iter()) {
                let mut n = 0;
                let mut child = flow.first_child();
                while let Some(c) = child {
                    n += 1;
                    child = c.next_sibling();
                }
                if n != plan.pages.len() {
                    return false;
                }
            }
            for (plan, (volume, chapter)) in plans.iter().zip(counts.iter()) {
                if let Some(label) = chapter {
                    label.set_label(&ntrf!("{0} page", "{0} pages", plan.pages.len(), plan.pages.len()));
                }
                if let Some(label) = volume {
                    let pages: usize = plans.iter().filter(|p| p.volume_id == plan.volume_id).map(|p| p.pages.len()).sum();
                    label.set_label(&ntrf!("{0} page", "{0} pages", pages, pages));
                }
            }
        }
        self.update_subtitle(&plans, has_volumes);
        true
    }

    /// Rebuilds a single card (role, markers) in the same place.
    fn update_card(&self, index: usize) {
        let Some(child) = self.0.cards.borrow().get(index).cloned() else { return };
        let webtoon = self.0.webtoon.is_active();
        let (card, badge) = {
            let items = self.0.items.borrow();
            self.card(index, &items[index], webtoon)
        };
        let selected = child.is_selected();
        child.set_child(Some(&card));
        if let Some(slot) = self.0.badges.borrow_mut().get_mut(index) {
            *slot = badge;
        }
        if selected {
            child.grab_focus();
        }
    }

    fn move_item(&self, from: usize, to: usize) {
        let (plans, _) = self.plan();
        let target = {
            let len = self.0.items.borrow().len();
            if from >= len || from == to {
                return;
            }
            to.min(len - 1)
        };
        let to = {
            let mut items = self.0.items.borrow_mut();
            // Moving within a chapter without markers doesn't change the structure — only one card
            // moves within the same grid. Otherwise (at a chapter boundary, a page with a marker) —
            // a full rebuild.
            let to = target;
            let plan = plans.iter().find(|p| p.pages.contains(&from));
            let local = plan.is_some_and(|p| {
                from > p.pages.start && to > p.pages.start && to < p.pages.end && items[from].chapter.is_none() && items[from].volume.is_none()
            });
            let item = items.remove(from);
            items.insert(to, item);
            local.then_some(to)
        };
        let child = self.0.cards.borrow().get(from).cloned();
        match to.zip(child) {
            Some((to, child)) => {
                let flow = child.parent().and_downcast::<gtk::FlowBox>().expect("card in a grid");
                let start = plans.iter().find(|p| p.pages.contains(&from)).map_or(0, |p| p.pages.start);
                flow.remove(&child);
                flow.insert(&child, (to - start) as i32);
                let mut cards = self.0.cards.borrow_mut();
                let card = cards.remove(from);
                cards.insert(to, card);
                drop(cards);
                let mut badges = self.0.badges.borrow_mut();
                let badge = badges.remove(from);
                badges.insert(to, badge);
                drop(badges);
                self.renumber(from.min(to)..from.max(to) + 1);
                self.select(to);
            }
            None => {
                self.refresh();
                self.select(target);
            }
        }
    }

    fn select(&self, index: usize) {
        let Some(child) = self.0.cards.borrow().get(index).cloned() else { return };
        if let Some(flow) = child.parent().and_downcast::<gtk::FlowBox>() {
            flow.select_child(&child);
            child.grab_focus();
        }
        // The selected card must be visible — but only after layout (before it the coordinates are zero).
        let this = self.clone();
        glib::idle_add_local_once(move || {
            if this.0.pending_scroll.get().is_some() {
                return; // the position is still being restored — don't interrupt
            }
            let Some(bounds) = child.compute_bounds(&this.0.sections) else { return };
            let adj = this.0.grid.vadjustment();
            let (top, bottom) = (f64::from(bounds.y()), f64::from(bounds.y() + bounds.height()));
            if top < adj.value() {
                adj.set_value(top);
            } else if bottom > adj.value() + adj.page_size() {
                adj.set_value(bottom - adj.page_size());
            }
        });
    }

    /// The current project structure.
    fn plan(&self) -> (Vec<ChapterPlan>, bool) {
        let items = self.0.items.borrow();
        let markers: Vec<_> = items.iter().map(|i| (i.chapter.as_ref(), i.volume.as_ref())).collect();
        plan_structure(&markers, None)
    }

    /// Rebuilds the grid from the current list of pages.
    fn refresh(&self) {
        // FIX: the grid is rebuilt entirely, and the scroll used to reset to the start — any action
        // on a page (role, spread, move) "threw" it to the top. The position is remembered
        // and restored as soon as the new grid is laid out (in the scroller's `changed` — in the same frame).
        // Focus in the old card: when it's removed, GTK moves focus to the first card of the grid and
        // scrolls to it (sometimes a frame later). Drop the focus in advance.
        let focus_in_grid = gtk::prelude::GtkWindowExt::focus(&self.0.window).is_some_and(|f| f.is_ancestor(&self.0.sections));
        if focus_in_grid {
            gtk::prelude::GtkWindowExt::set_focus(&self.0.window, None::<&gtk::Widget>);
        }
        let value = self.0.grid.vadjustment().value();
        if value > 0.0 && self.0.pending_scroll.get().is_none() {
            self.0.pending_scroll.set(Some(value));
            // A safeguard: if the content got shorter and the exact position won't exist anymore.
            let this = self.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                if let Some(target) = this.0.pending_scroll.take() {
                    let adj = this.0.grid.vadjustment();
                    adj.set_value(target.min(adj.upper() - adj.page_size()).max(0.0));
                }
            });
        }
        // FIX (performance): the new grid is built off-window and inserted in one go.
        // Adding thousands of widgets to an already shown container realizes and styles each one separately —
        // a repeated rebuild was several times slower than the first.
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        let webtoon = self.0.webtoon.is_active();
        let (plans, has_volumes) = self.plan();
        let structured = has_volumes || plans.len() > 1;
        let mut flows = Vec::new();
        let mut cards = Vec::new();
        let mut badges = Vec::new();
        let mut counts = Vec::new();
        {
            let items = self.0.items.borrow();
            for plan in &plans {
                let start_id = items[plan.pages.start].id;
                let removable = plan.pages.start > 0;
                let mut volume_count = None;
                let mut chapter_count = None;
                if has_volumes && plan.volume_start {
                    let pages = plans.iter().filter(|p| p.volume_id == plan.volume_id).map(|p| p.pages.len()).sum::<usize>();
                    let (header, count) = self.section_header(&plan.volume_label(), pages, start_id, true, removable);
                    content.append(&header);
                    volume_count = Some(count);
                }
                if structured {
                    let (header, count) = self.section_header(&plan.label(), plan.pages.len(), start_id, false, removable);
                    content.append(&header);
                    chapter_count = Some(count);
                }
                counts.push((volume_count, chapter_count));
                let flow = new_flow();
                for i in plan.pages.clone() {
                    let (card, badge) = self.card(i, &items[i], webtoon);
                    flow.append(&card);
                    cards.push(flow.last_child().and_downcast::<gtk::FlowBoxChild>().expect("card in a FlowBoxChild"));
                    badges.push(badge);
                }
                // One selection for the whole project: selected in one grid — clear it in the others.
                let this = self.clone();
                flow.connect_selected_children_changed(move |flow| {
                    if flow.selected_children().is_empty() {
                        return;
                    }
                    for other in this.0.flows.borrow().iter().filter(|f| *f != flow) {
                        other.unselect_all();
                    }
                });
                content.append(&flow);
                flows.push(flow);
            }
        }
        // The "to the end" zone for chapters and volumes — appears only while they're being dragged.
        let end_zone = structured.then(|| {
            let zone = gtk::Label::builder()
                .label(if has_volumes { tr("Drop here to move a chapter or volume to the end") } else { tr("Drop here to move a chapter to the end") })
                .visible(false)
                .margin_top(12)
                .build();
            zone.add_css_class("group-drop-zone");
            zone.add_css_class("dim-label");
            self.attach_group_target(zone.upcast_ref(), None, true);
            content.append(&zone);
            zone.upcast::<gtk::Widget>()
        });
        *self.0.end_zone.borrow_mut() = end_zone;
        while let Some(child) = self.0.sections.first_child() {
            self.0.sections.remove(&child);
        }
        self.0.sections.append(&content);
        *self.0.flows.borrow_mut() = flows;
        *self.0.cards.borrow_mut() = cards;
        *self.0.badges.borrow_mut() = badges;
        *self.0.counts.borrow_mut() = counts;
        self.update_subtitle(&plans, has_volumes);
        self.schedule_thumbs();
        // Volumes may have changed — so has the list of what a cover can be attached to.
        self.rebuild_covers();
    }

    /// Window subtitle: pages, formats, volumes and chapters; and the state of the "Save" button.
    fn update_subtitle(&self, plans: &[ChapterPlan], has_volumes: bool) {
        let structured = has_volumes || plans.len() > 1;
        let items = self.0.items.borrow();
        let n = items.len();
        self.0.stack.set_visible_child_name(if n == 0 { "empty" } else { "pages" });
        self.0.save_button.set_sensitive(n > 0 && !self.0.busy.get());
        let mut formats: Vec<String> = Vec::new();
        for item in items.iter() {
            let f = item.format.to_string();
            if !formats.contains(&f) {
                formats.push(f);
            }
        }
        let mut subtitle = ntrf!("{0} page · {1}", "{0} pages · {1}", n, n, formats.join(", "));
        if structured {
            let volumes = plans.iter().filter(|p| p.volume_start).count();
            subtitle = if has_volumes {
                trf!("{0} · volumes {1}, chapters {2}", subtitle, volumes, plans.len())
            } else {
                trf!("{0} · chapters {1}", subtitle, plans.len())
            };
        }
        self.0.title.set_subtitle(&if n == 0 { tr("New volume").to_string() } else { subtitle });
    }

    fn update_thumb(&self, index: usize, paintable: &gdk::Paintable) {
        let Some(child) = self.0.cards.borrow().get(index).cloned() else { return };
        if let Some(picture) = find_child::<gtk::Picture>(&child.upcast()) {
            picture.set_paintable(Some(paintable));
        }
    }

    /// Check the visible thumbnails on the next iteration (several events — one check).
    fn schedule_thumbs(&self) {
        if self.0.thumbs_pending.replace(true) {
            return;
        }
        let this = self.clone();
        glib::idle_add_local_once(move || {
            this.0.thumbs_pending.set(false);
            this.update_thumbs();
        });
    }

    /// FIX (performance): thumbnails used to be decoded for all pages at once (2000 parallel
    /// decodes, ~1 GB of textures). Now — only for cards near the visible area, no more than
    /// `THUMB_WORKERS` at a time; distant cards release their texture (it comes back on scroll).
    fn update_thumbs(&self) {
        let adj = self.0.grid.vadjustment();
        let (value, page) = (adj.value(), adj.page_size().max(1.0));
        let (near_top, near_bottom) = (value - page, value + 2.0 * page);
        let (far_top, far_bottom) = (value - 4.0 * page, value + 5.0 * page);
        let cards = self.0.cards.borrow().clone();
        let width = CARD_WIDTH * self.0.window.scale_factor().clamp(1, 3);
        for (i, child) in cards.iter().enumerate() {
            let Some(bounds) = child.compute_bounds(&self.0.sections) else { continue };
            if bounds.height() <= 0.0 {
                continue; // not laid out yet — will be checked after `changed`
            }
            let (top, bottom) = (f64::from(bounds.y()), f64::from(bounds.y() + bounds.height()));
            let near = bottom >= near_top && top <= near_bottom;
            let far = bottom < far_top || top > far_bottom;
            let (id, has_thumb, source, w, h) = {
                let items = self.0.items.borrow();
                let Some(item) = items.get(i) else { break };
                (item.id, item.thumb.is_some(), item.source.clone(), item.width, item.height)
            };
            if near && !has_thumb {
                let mut loading = self.0.thumbs_loading.borrow_mut();
                if loading.len() >= THUMB_WORKERS || loading.contains(&id) {
                    continue;
                }
                loading.insert(id);
                drop(loading);
                let this = self.clone();
                glib::spawn_future_local(async move {
                    let result = imaging::thumbnail(source, width).await;
                    this.0.thumbs_loading.borrow_mut().remove(&id);
                    if let Ok(texture) = result {
                        let index = this.index_of(id);
                        if let Some(index) = index {
                            this.0.items.borrow_mut()[index].thumb = Some(texture.clone());
                            this.update_thumb(index, texture.upcast_ref());
                        }
                    }
                    this.schedule_thumbs();
                });
            } else if far && has_thumb {
                self.0.items.borrow_mut()[i].thumb = None;
                self.update_thumb(i, &imaging::placeholder(w.max(2), h.max(3)));
            }
        }
    }

    /// A volume or chapter header: title, page count, editing, removing the split.
    fn section_header(&self, label: &str, pages: usize, start_id: u64, volume: bool, removable: bool) -> (gtk::Widget, gtk::Label) {
        let row = gtk::Box::builder().spacing(6).margin_top(if volume { 18 } else { 10 }).build();
        row.add_css_class("section-header");
        row.set_tooltip_text(Some(if volume { tr("Drag the header to move the whole volume") } else { tr("Drag the header to move the whole chapter") }));
        let title = gtk::Label::builder().label(label).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        title.add_css_class(if volume { "title-2" } else { "title-4" });
        let count = gtk::Label::new(Some(&ntrf!("{0} page", "{0} pages", pages, pages)));
        count.add_css_class("dim-label");
        let spacer = gtk::Box::builder().hexpand(true).build();
        let edit = small_button("document-edit-symbolic", if volume { tr("Volume number and title") } else { tr("Chapter number and title") });
        let this = self.clone();
        edit.connect_clicked(move |_| this.edit_section(start_id, volume));
        row.append(&title);
        row.append(&count);
        row.append(&spacer);
        row.append(&edit);
        if removable {
            let tip = if volume { tr("Remove the split: the volume's pages will join the previous one") } else { tr("Merge with the previous chapter") };
            let remove = small_button("list-remove-symbolic", tip);
            let this = self.clone();
            remove.connect_clicked(move |_| this.toggle_break(start_id, volume));
            row.append(&remove);
        }
        self.attach_group_dnd(&row, start_id, volume);
        (row.upcast(), count)
    }

    /// Sets or removes the start of a chapter (volume) on a page.
    fn toggle_break(&self, id: u64, volume: bool) {
        let first = self.0.items.borrow().first().map(|i| i.id) == Some(id);
        {
            let mut items = self.0.items.borrow_mut();
            let Some(item) = items.iter_mut().find(|i| i.id == id) else { return };
            let slot = if volume { &mut item.volume } else { &mut item.chapter };
            if slot.is_some() {
                *slot = None;
            } else if first && !volume {
                drop(items);
                return widgets::toast(&self.0.toasts, tr("The first page always starts a chapter"));
            } else {
                *slot = Some(Section::default());
            }
        }
        self.refresh();
    }

    /// Dialog for a chapter or volume number and title.
    fn edit_section(&self, id: u64, volume: bool) {
        let (plans, _) = self.plan();
        let Some(index) = self.index_of(id) else { return };
        let Some(plan) = plans.iter().find(|p| p.pages.start == index).cloned() else { return };
        let current = {
            let items = self.0.items.borrow();
            let item = &items[index];
            if volume { item.volume.clone() } else { item.chapter.clone() }.unwrap_or_default()
        };
        let auto = if volume { plan.volume_id.to_string() } else { plan.chapter_id.to_string() };
        let number = adw::EntryRow::builder().title(trf!("Number (empty — {0})", auto)).text(&current.number).build();
        let title = adw::EntryRow::builder().title(tr("Title")).text(&current.title).build();
        if volume {
            number.set_input_purpose(gtk::InputPurpose::Digits);
        }
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        list.append(&number);
        list.append(&title);

        let dialog = adw::AlertDialog::new(Some(if volume { tr("Volume") } else { tr("Chapter") }), None);
        if !volume {
            dialog.set_body(tr("The number can be fractional (\"10.5\") — in the file it's stored in the table of contents."));
        }
        dialog.set_extra_child(Some(&list));
        dialog.add_response("cancel", tr("Cancel"));
        dialog.add_response("save", tr("Save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let this = self.clone();
        dialog.connect_response(Some("save"), move |_, _| {
            let section = Section { number: number.text().trim().to_string(), title: title.text().trim().to_string(), ..current.clone() };
            if volume && !section.number.is_empty() && section.number.parse::<u16>().is_err() {
                return widgets::toast(&this.0.toasts, tr("A volume number must be an integer"));
            }
            if let Some(item) = this.0.items.borrow_mut().iter_mut().find(|i| i.id == id) {
                if volume {
                    item.volume = Some(section);
                } else {
                    item.chapter = Some(section);
                }
            }
            this.refresh();
        });
        dialog.present(Some(&self.0.window));
    }

    // ------------------------------------------------------------------
    // Page attachments
    // ------------------------------------------------------------------

    /// Changes attachment `k` of page `id`.
    fn with_aux(&self, id: u64, k: usize, f: impl FnOnce(&mut AuxInput)) {
        if let Some(a) = self.0.items.borrow_mut().iter_mut().find(|i| i.id == id).and_then(|i| i.aux.get_mut(k)) {
            f(a);
        }
    }

    /// The "Page attachments" dialog: a list with parameters, adding and previewing.
    fn edit_aux(&self, id: u64) {
        let Some(index) = self.index_of(id) else { return };
        let webtoon = self.0.webtoon.is_active();
        let title = if webtoon { trf!("Fragment {0} attachments", index + 1) } else { trf!("Page {0} attachments", index + 1) };
        let header = adw::HeaderBar::new();
        let add = gtk::MenuButton::builder().icon_name("list-add-symbolic").tooltip_text(tr("Add")).build();
        let menu = gio::Menu::new();
        for (label, kind) in [
            (tr("Music…"), AuxKind::Music),
            (tr("Sound effect…"), AuxKind::Sfx),
            (tr("Text from a file…"), AuxKind::Text),
            (tr("Dark version…"), AuxKind::DarkImage),
            (tr("Other file…"), AuxKind::Attachment),
        ] {
            let entry = gio::MenuItem::new(Some(label), None);
            entry.set_action_and_target_value(Some("aux.add"), Some(&(kind as u8).to_variant()));
            menu.append_item(&entry);
        }
        menu.append(Some(tr("Write text…")), Some("aux.write"));
        add.set_menu_model(Some(&menu));
        header.pack_start(&add);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        let dialog = adw::Dialog::builder().title(title).content_width(480).content_height(560).child(&toolbar).build();
        let player: Preview = Rc::default();

        let fill: Refill = {
            let (this, toolbar, player) = (self.clone(), toolbar.clone(), player.clone());
            let fill_ref: Rc<RefCell<Option<Refill>>> = Rc::default();
            let me = fill_ref.clone();
            let f: Refill = Rc::new(move || {
                let refill = me.borrow().clone();
                toolbar.set_content(Some(&this.aux_page(id, webtoon, &player, refill)));
            });
            *fill_ref.borrow_mut() = Some(f.clone());
            f
        };

        let actions = gio::SimpleActionGroup::new();
        let add_action = gio::SimpleAction::new("add", Some(&u8::static_variant_type()));
        let (this, refill) = (self.clone(), fill.clone());
        add_action.connect_activate(move |_, param| {
            let kind = AuxKind::from(param.and_then(|p| p.get::<u8>()).unwrap_or(0));
            let (this, refill) = (this.clone(), refill.clone());
            glib::spawn_future_local(async move {
                if this.add_aux_file(id, kind).await {
                    refill();
                }
            });
        });
        actions.add_action(&add_action);
        let write = gio::SimpleAction::new("write", None);
        let (this, refill) = (self.clone(), fill.clone());
        write.connect_activate(move |_, _| this.edit_text(id, None, refill.clone()));
        actions.add_action(&write);
        dialog.insert_action_group("aux", Some(&actions));
        dialog.connect_closed(move |_| {
            player.borrow_mut().take();
        });
        fill();
        dialog.present(Some(&self.0.window));
    }

    /// Contents of the attachments dialog for the current state of the page.
    fn aux_page(&self, id: u64, webtoon: bool, player: &Preview, refill: Option<Refill>) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        page.add(&group);
        let (aux, height) = {
            let items = self.0.items.borrow();
            let Some(item) = items.iter().find(|i| i.id == id) else { return page };
            (item.aux.clone(), item.height)
        };
        if aux.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("audio-x-generic-symbolic")
                .title(tr("No attachments"))
                .description(tr("The \"+\" button: music, sound effects, page text or its dark version.\nThe reader plays the sound and shows the text, and the dark version — in a dark theme."))
                .build();
            empty.add_css_class("compact");
            group.add(&empty);
            return page;
        }
        group.set_description(Some(if webtoon {
            tr("Music plays from this point until the next music or the end of the chapter; an effect — when the reader reaches the trigger line.")
        } else {
            tr("Music plays from this page until the next music or the end of the chapter; an effect — when the page is opened.")
        }));
        let refill = refill.unwrap_or_else(|| Rc::new(|| {}));
        for (k, a) in aux.iter().enumerate() {
            let format = aux_format(&a.chunk);
            let subtitle = match &a.chunk.label {
                Some(name) => format!("{name} · {}", human_size(u64::from(a.chunk.size))),
                None => format!("{format} · {}", human_size(u64::from(a.chunk.size))),
            };
            let row = adw::ExpanderRow::builder().title(a.kind.label()).subtitle(subtitle).build();
            // Suffixes are added right to left: the trash can is the outermost.
            let remove = small_button("user-trash-symbolic", tr("Remove attachment"));
            let (this, refill_c, player_c) = (self.clone(), refill.clone(), player.clone());
            remove.connect_clicked(move |_| {
                player_c.borrow_mut().take();
                if let Some(item) = this.0.items.borrow_mut().iter_mut().find(|i| i.id == id)
                    && k < item.aux.len()
                {
                    item.aux.remove(k);
                }
                if let Some(i) = this.index_of(id) {
                    this.update_card(i);
                }
                refill_c();
            });
            row.add_suffix(&remove);
            row.add_prefix(&gtk::Image::from_icon_name(audio::kind_icon(a.kind)));
            if a.kind.is_audio() || (a.kind == AuxKind::Attachment && format.is_audio()) {
                let play = small_button("media-playback-start-symbolic", tr("Preview"));
                let playing = player.borrow().as_ref().is_some_and(|(n, s)| *n == k && s.is_playing());
                if playing {
                    play.set_icon_name("media-playback-stop-symbolic");
                }
                let (player, chunk, volume) = (player.clone(), a.chunk.clone(), a.volume);
                play.connect_clicked(move |button| {
                    let mut slot = player.borrow_mut();
                    if slot.as_ref().is_some_and(|(n, s)| *n == k && s.is_playing()) {
                        slot.take();
                        button.set_icon_name("media-playback-start-symbolic");
                        return;
                    }
                    let Some(bytes) = chunk_bytes(&chunk) else { return };
                    match audio::Sound::new(&bytes) {
                        Ok(sound) => {
                            sound.set_volume(if volume == 0 { 1.0 } else { f64::from(volume) / 100.0 });
                            let shown = button.downgrade();
                            sound.media().connect_ended_notify(move |m| {
                                if m.is_ended()
                                    && let Some(b) = shown.upgrade()
                                {
                                    b.set_icon_name("media-playback-start-symbolic");
                                }
                            });
                            sound.play();
                            *slot = Some((k, sound));
                            drop(slot);
                            button.set_icon_name("media-playback-stop-symbolic");
                        }
                        Err(e) => eprintln!("sound: {e}"),
                    }
                });
                row.add_suffix(&play);
            }
            if a.kind == AuxKind::Text {
                let edit = small_button("document-edit-symbolic", tr("Edit text"));
                let (this, refill) = (self.clone(), refill.clone());
                edit.connect_clicked(move |_| this.edit_text(id, Some(k), refill.clone()));
                row.add_suffix(&edit);
            }

            match a.kind {
                AuxKind::Music | AuxKind::Sfx => {
                    let looped = adw::SwitchRow::builder()
                        .title(if a.kind == AuxKind::Music { tr("Loop") } else { tr("Loop while the page is visible") })
                        .active(a.flags & AuxFlags::LOOP != 0)
                        .build();
                    let this = self.clone();
                    looped.connect_active_notify(move |r| {
                        let on = r.is_active();
                        this.with_aux(id, k, |a| a.flags = if on { a.flags | AuxFlags::LOOP } else { a.flags & !AuxFlags::LOOP });
                    });
                    row.add_row(&looped);
                    if a.kind == AuxKind::Music {
                        let pages = adw::SpinRow::with_range(0.0, 9999.0, 1.0);
                        pages.set_title(tr("Number of pages"));
                        pages.set_subtitle(if webtoon {
                            tr("How many episode pages it plays for, starting from this one; 0 — until the next music or the end of the chapter")
                        } else {
                            tr("How many pages it plays for, starting from this one; 0 — until the next music or the end of the chapter")
                        });
                        pages.set_value(f64::from(a.pages));
                        let this = self.clone();
                        pages.connect_value_notify(move |r| {
                            let v = r.value() as u16;
                            this.with_aux(id, k, |a| a.pages = v);
                        });
                        row.add_row(&pages);
                    }
                    let duration = adw::SpinRow::with_range(0.0, 36_000.0, 0.5);
                    duration.set_digits(1);
                    duration.set_title(tr("Duration, s"));
                    duration.set_subtitle(if a.kind == AuxKind::Music {
                        tr("How long to play from the start (the same with looping), then fade out; 0 — unlimited")
                    } else {
                        tr("Cut the effect off after this many seconds (handy with looping); 0 — unlimited")
                    });
                    duration.set_value(f64::from(a.duration_ms) / 1000.0);
                    let this = self.clone();
                    duration.connect_value_notify(move |r| {
                        let v = (r.value() * 1000.0).round() as u32;
                        this.with_aux(id, k, |a| a.duration_ms = v);
                    });
                    row.add_row(&duration);
                    let delay = adw::SpinRow::with_range(0.0, 600_000.0, 100.0);
                    delay.set_title(tr("Delay, ms"));
                    delay.set_value(f64::from(a.delay_ms));
                    let this = self.clone();
                    delay.connect_value_notify(move |r| {
                        let v = r.value() as u32;
                        this.with_aux(id, k, |a| a.delay_ms = v);
                    });
                    row.add_row(&delay);
                    let volume = adw::SpinRow::with_range(1.0, 100.0, 5.0);
                    volume.set_title(tr("Volume, %"));
                    volume.set_value(if a.volume == 0 { 100.0 } else { f64::from(a.volume) });
                    let this = self.clone();
                    volume.connect_value_notify(move |r| {
                        let v = r.value() as u8;
                        this.with_aux(id, k, |a| a.volume = if v >= 100 { 0 } else { v });
                    });
                    row.add_row(&volume);
                    if a.kind == AuxKind::Sfx && webtoon {
                        let trigger = adw::SpinRow::with_range(0.0, f64::from(height.max(1)), 50.0);
                        trigger.set_title(tr("Trigger line, px"));
                        trigger.set_subtitle(tr("From the top of this fragment: the effect plays when the reader reaches it"));
                        trigger.set_value(f64::from(a.trigger_y));
                        let this = self.clone();
                        trigger.connect_value_notify(move |r| {
                            let v = r.value() as u32;
                            this.with_aux(id, k, |a| a.trigger_y = v);
                        });
                        row.add_row(&trigger);
                    }
                }
                AuxKind::Text => {
                    let text = chunk_bytes(&a.chunk).map(|b| vnmf::playback::text_layer(&b)).unwrap_or_default();
                    let label = gtk::Label::builder()
                        .label(if text.is_empty() { tr("(empty)").to_string() } else { text })
                        .wrap(true)
                        .xalign(0.0)
                        .selectable(true)
                        .lines(8)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .build();
                    label.add_css_class("page-text");
                    row.add_row(&label);
                }
                AuxKind::DarkImage => {
                    let hint = adw::ActionRow::builder()
                        .title(if webtoon { tr("Replaces this fragment in a dark theme") } else { tr("Replaces the page in a dark theme") })
                        .subtitle(tr("The dimensions must match the original"))
                        .build();
                    row.add_row(&hint);
                }
                AuxKind::Attachment => {}
            }
            group.add(&row);
        }
        page
    }

    /// Adds an attachment from a file. `true` — added.
    async fn add_aux_file(&self, id: u64, kind: AuxKind) -> bool {
        let filter = gtk::FileFilter::new();
        let patterns: &[&str] = match kind {
            AuxKind::Music | AuxKind::Sfx => &["*.ogg", "*.oga", "*.opus", "*.mp3", "*.flac", "*.wav", "*.m4a", "*.aac"],
            AuxKind::Text => &["*.txt", "*.json"],
            AuxKind::DarkImage => &["*.png", "*.jpg", "*.jpeg", "*.webp", "*.avif", "*.jxl", "*.gif"],
            AuxKind::Attachment => &["*"],
        };
        filter.set_name(Some(kind.label()));
        for p in patterns {
            filter.add_pattern(p);
            filter.add_pattern(&p.to_uppercase());
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title(kind.label()).filters(&filters).modal(true).build();
        let Ok(file) = dialog.open_future(Some(&self.0.window)).await else { return false };
        let Some(path) = file.path() else { return false };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let chunk = match ChunkSource::from_file(&path) {
            Ok(c) => c.with_label(name.clone()),
            Err(e) => {
                widgets::toast(&self.0.toasts, &format!("{name}: {e}"));
                return false;
            }
        };
        let format = aux_format(&chunk);
        if !kind.accepts(format) {
            let what = match kind {
                AuxKind::Music | AuxKind::Sfx => tr("a sound (Ogg, MP3, FLAC, WAV, M4A)"),
                AuxKind::Text => tr("UTF-8 text or JSON"),
                _ => tr("an image"),
            };
            widgets::toast(&self.0.toasts, &trf!("{0}: this is not {1}", name, what));
            return false;
        }
        let mut aux = AuxInput::new(kind, chunk);
        if kind == AuxKind::Music {
            aux.flags = AuxFlags::LOOP;
        }
        let Some(index) = self.index_of(id) else { return false };
        if kind == AuxKind::DarkImage {
            let page = self.0.items.borrow()[index].width as f64 / self.0.items.borrow()[index].height.max(1) as f64;
            if let Ok((w, h, _)) = aux.chunk.probe_image()
                && (w as f64 / h.max(1) as f64 - page).abs() > 0.02 * page
            {
                widgets::toast(&self.0.toasts, &trf!("{0}: the aspect ratio differs from the page", name));
            }
        }
        self.0.items.borrow_mut()[index].aux.push(aux);
        self.update_card(index);
        true
    }

    /// Page text editor: `k` — an existing text attachment, `None` — a new one.
    fn edit_text(&self, id: u64, k: Option<usize>, refill: Refill) {
        let current = k
            .and_then(|k| self.0.items.borrow().iter().find(|i| i.id == id).and_then(|i| i.aux.get(k)).and_then(|a| chunk_bytes(&a.chunk)))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let view = gtk::TextView::builder().wrap_mode(gtk::WrapMode::WordChar).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).build();
        view.buffer().set_text(&current);
        let scroll = gtk::ScrolledWindow::builder().child(&view).min_content_height(220).min_content_width(380).build();
        scroll.add_css_class("card");
        let dialog = adw::AlertDialog::new(
            Some(tr("Page text")),
            Some(tr("The reader shows it in the \"Text\" panel (T). You can paste vnmf-text/1 JSON with regions.")),
        );
        dialog.set_extra_child(Some(&scroll));
        dialog.add_response("cancel", tr("Cancel"));
        dialog.add_response("save", tr("Save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        let this = self.clone();
        dialog.connect_response(Some("save"), move |_, _| {
            let buffer = view.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let Ok(chunk) = ChunkSource::from_bytes(text.into_bytes()) else { return };
            let chunk = chunk.with_label(tr("page text"));
            {
                let mut items = this.0.items.borrow_mut();
                let Some(item) = items.iter_mut().find(|i| i.id == id) else { return };
                match k.and_then(|k| item.aux.get_mut(k)) {
                    Some(a) => a.chunk = chunk,
                    None => item.aux.push(AuxInput::new(AuxKind::Text, chunk)),
                }
            }
            if let Some(i) = this.index_of(id) {
                this.update_card(i);
            }
            refill();
        });
        dialog.present(Some(&self.0.window));
    }

    fn card(&self, index: usize, item: &Item, webtoon: bool) -> (gtk::Widget, gtk::Label) {
        let picture = match &item.thumb {
            Some(t) => gtk::Picture::for_paintable(t),
            None => gtk::Picture::for_paintable(&imaging::placeholder(item.width.max(2), item.height.max(3))),
        };
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_size_request(CARD_WIDTH, CARD_HEIGHT);

        let number = gtk::Label::new(Some(&(index + 1).to_string()));
        number.add_css_class("page-badge");
        number.add_css_class("osd");
        number.set_halign(gtk::Align::Start);
        number.set_valign(gtk::Align::Start);
        number.set_margin_start(6);
        number.set_margin_top(6);

        // The thumbnail is decoded for the screen scale (HiDPI), but the card has a fixed width.
        let frame = adw::Clamp::builder().maximum_size(CARD_WIDTH).tightening_threshold(CARD_WIDTH).child(&picture).build();
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&frame));
        overlay.add_overlay(&number);
        if item.damaged {
            let damage = gtk::Label::new(Some(tr("Damaged")));
            damage.add_css_class("damage-badge");
            damage.set_tooltip_text(Some(tr("The image CRC in the source file doesn't match — the page should be checked or replaced")));
            damage.set_halign(gtk::Align::End);
            damage.set_valign(gtk::Align::Start);
            damage.set_margin_end(6);
            damage.set_margin_top(6);
            overlay.add_overlay(&damage);
        }
        if !item.aux.is_empty() {
            let badge = gtk::Box::builder()
                .spacing(3)
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .margin_end(6)
                .margin_top(if item.damaged { 32 } else { 6 })
                .build();
            badge.add_css_class("aux-badge");
            badge.add_css_class("osd");
            let mut kinds: Vec<AuxKind> = item.aux.iter().map(|a| a.kind).collect();
            kinds.sort();
            kinds.dedup();
            for kind in &kinds {
                let icon = gtk::Image::from_icon_name(audio::kind_icon(*kind));
                icon.set_pixel_size(12);
                badge.append(&icon);
            }
            let labels: Vec<&str> = item.aux.iter().map(|a| a.kind.label()).collect();
            badge.set_tooltip_text(Some(&trf!("Attachments: {0}", labels.join(", "))));
            overlay.add_overlay(&badge);
        }
        if !webtoon && item.role != PageRole::Normal {
            let role = gtk::Label::new(Some(tr(ROLE_LABELS[item.role as usize])));
            role.add_css_class("role-badge");
            role.add_css_class("osd");
            role.set_halign(gtk::Align::Center);
            role.set_valign(gtk::Align::End);
            role.set_margin_bottom(6);
            overlay.add_overlay(&role);
        }

        let name = gtk::Label::builder()
            .label(&item.name)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(18)
            .tooltip_text(format!("{} · {}×{} · {}", item.name, item.width, item.height, item.format))
            .build();
        name.add_css_class("caption");

        // Buttons: ◀ ▶ role 🗑
        let left = small_button("go-previous-symbolic", tr("Move left (Ctrl+←)"));
        let right = small_button("go-next-symbolic", tr("Move right (Ctrl+→)"));
        let remove = small_button("user-trash-symbolic", tr("Remove (Delete)"));
        // Closures, not `action-name`: every button with a window action registers an observer in
        // GtkActionMuxer, and with thousands of cards that makes rebuilding the grid quadratically slower.
        let id = item.id;
        for (button, delta) in [(&left, -1isize), (&right, 1)] {
            let this = self.clone();
            button.connect_clicked(move |_| {
                if let Some(i) = this.index_of(id) {
                    this.move_item(i, i.saturating_add_signed(delta));
                }
            });
        }
        let this = self.clone();
        remove.connect_clicked(move |_| {
            if let Some(i) = this.index_of(id) {
                this.remove(i);
            }
        });
        let actions = gtk::Box::builder().spacing(2).halign(gtk::Align::Center).build();
        actions.append(&left);
        actions.append(&self.page_menu(item, webtoon));
        actions.append(&right);
        actions.append(&remove);

        let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).width_request(CARD_WIDTH + 12).build();
        card.add_css_class("page-card");
        card.append(&overlay);
        card.append(&name);
        card.append(&actions);
        self.attach_dnd(&card, id);
        (card.upcast(), number)
    }

    /// Page menu: chapter/volume start and (not in a webtoon) the role.
    ///
    /// FIX (performance): the menu is built only when opened. A `MenuButton` with a model immediately
    /// creates the whole `PopoverMenu` — 14 items per card, so 2000 pages produced
    /// tens of thousands of widgets, seconds for each grid rebuild and over a gigabyte of memory.
    fn page_menu(&self, item: &Item, webtoon: bool) -> gtk::MenuButton {
        let tooltip = if webtoon { tr("Chapter and volume").to_string() } else { trf!("Role: {0} · chapter and volume", tr(ROLE_LABELS[item.role as usize])) };
        let button = gtk::MenuButton::builder().icon_name("view-more-symbolic").tooltip_text(tooltip).build();
        button.add_css_class("flat");
        button.add_css_class("circular");
        let (this, id) = (self.clone(), item.id);
        button.set_create_popup_func(move |button| {
            let items = this.0.items.borrow();
            let Some(index) = items.iter().position(|i| i.id == id) else { return };
            button.set_menu_model(Some(&page_menu_model(&items[index], index == 0, this.0.webtoon.is_active())));
            // After closing — remove it: next time the menu is built from the current state.
            if let Some(popover) = button.popover() {
                let button = button.downgrade();
                popover.connect_closed(move |_| {
                    let button = button.clone();
                    glib::idle_add_local_once(move || {
                        if let Some(button) = button.upgrade() {
                            button.set_menu_model(None::<&gio::MenuModel>);
                        }
                    });
                });
            }
        });
        button
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.0.items.borrow().iter().position(|i| i.id == id)
    }

    /// Dragging cards: the source is a page id, the target is another card (left/right half).
    fn attach_dnd(&self, card: &gtk::Box, id: u64) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&format!("vnmf-page:{id}").to_value())));
        let card_weak = card.downgrade();
        source.connect_drag_begin(move |source, _| {
            if let Some(card) = card_weak.upgrade() {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&card))), CARD_WIDTH / 2, CARD_HEIGHT / 2);
            }
        });
        card.add_controller(source);

        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        target.set_preload(true);
        let card_weak = card.downgrade();
        target.connect_motion(move |target, x, _| {
            // Chapters and volumes are dropped on headers, not on cards.
            if dragged_text(target).is_some_and(|s| s.starts_with(GROUP_PREFIX)) {
                return gdk::DragAction::empty();
            }
            if let Some(card) = card_weak.upgrade() {
                let after = x > f64::from(card.width()) / 2.0;
                card.remove_css_class(if after { "drop-before" } else { "drop-after" });
                card.add_css_class(if after { "drop-after" } else { "drop-before" });
            }
            gdk::DragAction::MOVE
        });
        let card_weak = card.downgrade();
        target.connect_leave(move |_| {
            if let Some(card) = card_weak.upgrade() {
                card.remove_css_class("drop-before");
                card.remove_css_class("drop-after");
            }
        });
        let (this, card_weak) = (self.clone(), card.downgrade());
        target.connect_drop(move |_, value, x, _| {
            let Some(dragged) = value.get::<String>().ok().and_then(|s| s.strip_prefix("vnmf-page:")?.parse::<u64>().ok()) else {
                return false;
            };
            let (Some(from), Some(to)) = (this.index_of(dragged), this.index_of(id)) else { return false };
            let after = card_weak.upgrade().is_some_and(|c| x > f64::from(c.width()) / 2.0);
            let mut target_index = if after { to + 1 } else { to };
            if from < target_index {
                target_index -= 1; // the element is first taken out of the list
            }
            let this = this.clone();
            // Rebuild the grid — after the drag handling finishes.
            glib::idle_add_local_once(move || this.move_item(from, target_index));
            true
        });
        card.add_controller(target);
    }

    /// Dragging a whole chapter or volume by its header. The target is another header: the group
    /// goes before that chapter (volume). A volume can only be dropped on a volume header.
    fn attach_group_dnd(&self, row: &gtk::Box, start_id: u64, volume: bool) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&group_token(volume, start_id).to_value())));
        let (this, row_weak) = (self.clone(), row.downgrade());
        source.connect_drag_begin(move |source, _| {
            if let Some(row) = row_weak.upgrade() {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 24, 16);
            }
            this.show_end_zone(true);
        });
        let this = self.clone();
        source.connect_drag_end(move |_, _, _| this.show_end_zone(false));
        row.add_controller(source);
        self.attach_group_target(row.upcast_ref(), Some(start_id), volume);
    }

    /// A target for chapters and volumes: a header (`before` — its first page) or the "to the end" zone.
    fn attach_group_target(&self, widget: &gtk::Widget, before: Option<u64>, volume_header: bool) {
        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        target.set_preload(true);
        let accepts = move |target: &gtk::DropTarget| {
            let group = dragged_text(target).as_deref().and_then(parse_group);
            group.filter(|(volume, _)| volume_header || !volume)
        };
        let weak = widget.downgrade();
        target.connect_motion(move |target, _, _| {
            let ok = accepts(target).is_some();
            if let Some(w) = weak.upgrade() {
                if ok { w.add_css_class("drop-before") } else { w.remove_css_class("drop-before") }
            }
            if ok { gdk::DragAction::MOVE } else { gdk::DragAction::empty() }
        });
        let weak = widget.downgrade();
        target.connect_leave(move |_| {
            if let Some(w) = weak.upgrade() {
                w.remove_css_class("drop-before");
            }
        });
        let this = self.clone();
        target.connect_drop(move |target, value, _, _| {
            let group = value.get::<String>().ok().as_deref().and_then(parse_group);
            let Some((volume, id)) = group.filter(|(volume, _)| volume_header || !volume) else { return false };
            if let Some(w) = target.widget() {
                w.remove_css_class("drop-before");
            }
            let this = this.clone();
            // Rebuild the grid — after the drag handling finishes.
            glib::idle_add_local_once(move || this.move_group(id, volume, before, volume_header));
            true
        });
        widget.add_controller(target);
    }

    fn show_end_zone(&self, visible: bool) {
        if let Some(zone) = self.0.end_zone.borrow().as_ref() {
            zone.set_visible(visible);
        }
    }

    /// Moves the chapter (or volume) starting at page `start` before page `before`
    /// (`None` — to the end). `volume_header`: the target is a volume header, i.e. "before the volume"; otherwise
    /// the chapter becomes the first chapter of the volume on whose first chapter's header it was dropped.
    fn move_group(&self, start: u64, volume: bool, before: Option<u64>, volume_header: bool) {
        let (Some(a), t) = (self.index_of(start), before.map(|id| self.index_of(id))) else { return };
        let t = match t {
            Some(Some(t)) => t,
            Some(None) => return,
            None => self.0.items.borrow().len(),
        };
        let moved = move_group_in(&mut self.0.items.borrow_mut(), a, volume, t, volume_header);
        if let Some(i) = moved {
            self.refresh();
            self.select(i);
        }
    }

    // ------------------------------------------------------------------
    // Saving
    // ------------------------------------------------------------------

    /// Metadata from the form (on top of the imported ones), reading direction and mode, and the contents.
    fn build_metadata(&self, chapters: &[ChapterPlan]) -> Metadata {
        let mut meta = self.0.form.read(&self.0.base.borrow());
        let direction = meta.reading_direction.as_deref();
        if self.0.rtl.is_active() {
            meta.reading_direction = Some("rtl".into());
        } else if direction == Some("rtl") {
            meta.reading_direction = None;
        }
        if self.0.webtoon.is_active() {
            meta.layout = Some("strip".into());
        } else if meta.layout.as_deref() == Some("strip") {
            meta.layout = None;
        }
        let mut contents = meta.contents.clone();
        contents.merge(&plan_contents(chapters));
        // Only the chapters and volumes that are in the file: after deletion or renumbering old
        // contents entries would describe other chapters.
        let exists = |v: u16, c: Option<u16>| chapters.iter().any(|p| p.volume_id == v && c.is_none_or(|c| p.chapter_id == c));
        for volume in &mut contents.volumes {
            let v = volume.volume;
            volume.chapters.retain(|c| exists(v, Some(c.chapter)));
        }
        contents.volumes.retain(|v| exists(v.volume, None));
        contents.prune();
        meta.contents = contents;
        meta
    }

    fn suggested_name(&self) -> String {
        if let Some(path) = self.0.editing.borrow().as_ref() {
            return trf!("{0} (modified).vnmf", vnmf::input::file_stem(path));
        }
        let title = self.0.form.read(&self.0.base.borrow()).title;
        let base = title.unwrap_or_else(|| tr("volume").into());
        let clean: String = base.chars().map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
        format!("{clean}.vnmf")
    }

    async fn save(&self) {
        if self.0.items.borrow().is_empty() || self.0.busy.get() {
            return;
        }
        let editing = self.0.editing.borrow().clone();
        let dialog = gtk::FileDialog::builder()
            .title(if editing.is_some() { tr("Save as a new file") } else { tr("Save volume") })
            .initial_name(self.suggested_name())
            .modal(true)
            .build();
        if let Some(folder) = editing.as_ref().and_then(|p| p.parent()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let Some(path) = dialog.save_future(Some(&self.0.window)).await.ok().and_then(|f| f.path()) else { return };
        // Pages of opened `.vnmf` files are read directly from them — and editing never
        // overwrites the source file.
        let same = |a: &std::path::Path| std::fs::canonicalize(a).ok().is_some_and(|a| std::fs::canonicalize(&path).ok() == Some(a));
        if self.0.sources.borrow().iter().any(|s| same(s)) {
            return widgets::toast(&self.0.toasts, tr("The source file is never overwritten — choose a different name"));
        }

        // A snapshot of the project for the background thread.
        let pages: Vec<ProjectPage> = self
            .0
            .items
            .borrow()
            .iter()
            .map(|i| ProjectPage { source: i.source.clone(), role: i.role, flags: i.flags, aux: i.aux.clone(), height: i.height })
            .collect();
        let webtoon = self.0.webtoon.is_active();
        let rtl = self.0.rtl.is_active();
        let redundancy = self.0.redundancy.value() as u8;
        let tail = self.0.tail.is_active();
        let cover = match self.0.cover.selected() {
            COVER_NONE => Cover::None,
            COVER_CUSTOM => Cover::Custom,
            _ => Cover::Auto,
        };
        let covers: Vec<(ChunkSource, u16)> = self.0.covers.borrow().iter().map(|c| (c.source.clone(), c.volume)).collect();
        // The first preview in the file is the main file cover: in "custom only" mode it's required.
        if matches!(cover, Cover::Custom) && !covers.iter().any(|(_, v)| *v == 0) {
            return widgets::toast(&self.0.toasts, tr("No custom cover for the file: add an image or attach one of the covers to \"File\""));
        }
        let block = [BlockSize::Page4k, BlockSize::Page8k, BlockSize::Page16k, BlockSize::Ext32k][self.0.block.selected().min(3) as usize];
        let (chapters, _) = self.plan();
        let meta = self.build_metadata(&chapters);
        let info = (!meta.is_empty()).then(|| meta.to_bytes());

        self.set_busy(true);
        let project = Project { pages, info, webtoon, rtl, redundancy, tail, cover, covers, block, chapters };
        let job_path = path.clone();
        let result = gio::spawn_blocking(move || build_volume(project, &job_path)).await;
        self.set_busy(false);

        match result {
            Ok(Ok(size)) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let toast = adw::Toast::builder()
                    .title(glib::markup_escape_text(&trf!("Saved {0} · {1}", name, human_size(size))))
                    .button_label(tr("Open in Reader"))
                    .timeout(8)
                    .build();
                let toasts = self.0.toasts.clone();
                toast.connect_button_clicked(move |_| {
                    if let Err(e) = vnmf_gui::launch_sibling("vnmf-reader", &path) {
                        widgets::toast(&toasts, &trf!("Could not launch Reader: {0}", e));
                    }
                });
                self.0.toasts.add_toast(toast);
            }
            Ok(Err(e)) => widgets::toast(&self.0.toasts, &trf!("Could not save: {0}", format!("{:#}", e))),
            Err(_) => widgets::toast(&self.0.toasts, tr("Saving crashed")),
        }
    }

    fn set_busy(&self, busy: bool) {
        self.0.busy.set(busy);
        if busy {
            let spinner = adw::Spinner::new();
            self.0.save_button.set_child(Some(&spinner));
        } else {
            self.0.save_button.set_label(tr("Save…"));
        }
        self.0.save_button.set_sensitive(!busy && !self.0.items.borrow().is_empty());
    }
}

impl AppWindow for CreatorWindow {
    fn present(&self) {
        self.0.window.present();
    }

    fn gtk_window(&self) -> gtk::Window {
        self.0.window.clone().upcast()
    }

    fn open(&self, path: PathBuf) {
        self.add_paths(vec![path]);
    }

    // Several images / CBZs from a file manager — one project.
    const ALL_IN_ONE: bool = true;
    fn open_many(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.add_paths(paths);
        }
    }
}

/// Images, folders and CBZs → pages with chapter and volume markers, metadata, the number of skipped files.
fn load_inputs(paths: &[PathBuf]) -> anyhow::Result<(Vec<Loaded>, Option<Metadata>, usize)> {
    let chapters = vnmf::input::load_inputs(paths)?;
    let structured: Vec<_> = chapters.iter().filter(|c| !c.loose && !c.pages.is_empty()).collect();
    let multi = structured.len() > 1;
    let one_volume = structured.windows(2).all(|w| w[0].volume == w[1].volume);
    // Saved contents (contents.json after vnmfunpack) — titles by (volume, number).
    let saved: Option<Contents> = chapters.iter().find_map(|c| c.contents.clone());
    let mut pages = Vec::new();
    let mut comic = None;
    let mut meta: Option<Metadata> = None;
    let mut skipped = 0;
    let mut prev_volume = None;
    for ch in chapters {
        skipped += ch.skipped.len();
        if comic.is_none() {
            comic = ch.comic_info.clone();
        }
        if meta.is_none() {
            meta = ch.metadata.clone();
        }
        let mut number = ch.number.clone();
        let mut title = ch.title.clone();
        let mut volume_title = ch.volume_title.clone();
        if let Some(saved) = &saved {
            let vol = ch.volume.unwrap_or(0);
            if let Some(meta) = number.as_deref().and_then(|n| n.parse().ok()).and_then(|id| saved.chapter(vol, id)) {
                number = meta.number.clone().or(number);
                title = meta.title.clone().or(title);
            }
            volume_title = saved.volume(vol).and_then(|v| v.title.clone()).or(volume_title);
        }
        // A chapter — each CBZ and folder (individual images continue the current chapter).
        let chapter = (!ch.loose).then(|| Section::new(number, title));
        let volume = match ch.volume {
            Some(v) if prev_volume != Some(v) => {
                prev_volume = Some(v);
                Some(Section::new(Some(v.to_string()), volume_title))
            }
            _ => None,
        };
        let start = pages.len();
        let mut aux = ch.aux;
        for (k, p) in ch.pages.into_iter().enumerate() {
            let (width, height, _) = p.source.probe_image().unwrap_or((0, 0, p.format));
            let first = k == 0;
            pages.push(Loaded {
                name: p.name,
                source: p.source,
                format: p.format,
                width,
                height,
                role: PageRole::Normal,
                flags: 0,
                aux: Vec::new(),
                damaged: false,
                chapter: chapter.clone().filter(|_| first),
                volume: volume.clone().filter(|_| first),
            });
        }
        // Attachments from `aux/` (after vnmfunpack): the number is the page's first image; in a webtoon
        // the dark version and the effect line point to a specific fragment.
        let heights: Vec<u32> = pages[start..].iter().map(|p| p.height).collect();
        for (idx, a) in aux.drain(..) {
            let (k, a) = place_aux(idx, a, &heights);
            if let Some(page) = pages.get_mut(start + k) {
                page.aux.push(a);
            }
        }
    }
    // A chapter's ComicInfo for a multi-chapter file describes the whole series (without Title/Number…).
    if multi && let Some(c) = comic.as_mut() {
        comicinfo::strip_chapter_fields(c, one_volume);
    }
    // vnmf.json (after vnmfunpack) takes precedence over ComicInfo.
    let meta = meta.or_else(|| comic.as_ref().map(Metadata::from_comicinfo));
    Ok((pages, meta, skipped))
}

/// `.vnmf` → pages (slices of the file, without copying), metadata and file parameters.
///
/// Webtoon: each fragment of an episode becomes a separate card, the episode — a chapter (when saving
/// the chapter's fragments are assembled into one long page again).
fn load_vnmf(path: &std::path::Path) -> anyhow::Result<(Vec<Loaded>, Option<Metadata>, FileOptions)> {
    let opened = vnmf::edit::open(path)?;
    let stem = vnmf::input::file_stem(path);
    let damaged = opened.damaged();
    let mut pages = Vec::new();
    for chapter in opened.chapters {
        let mut chapter_mark = Some(Section { number: chapter.number, title: chapter.title.unwrap_or_default(), date: chapter.date, isbn: None });
        let mut volume_mark = chapter.volume_start.then(|| Section {
            number: chapter.volume.to_string(),
            title: chapter.volume_title.unwrap_or_default(),
            date: chapter.volume_date,
            isbn: chapter.volume_isbn,
        });
        for page in chapter.pages {
            let (role, flags) = (page.role, page.flags);
            // Page attachments — by fragment (a regular page has one fragment).
            let heights: Vec<u32> = page.images.iter().map(|i| i.height).collect();
            let mut per_image: Vec<Vec<AuxInput>> = vec![Vec::new(); heights.len()];
            for a in page.aux {
                let (k, a) = place_aux(0, a, &heights);
                if let Some(slot) = per_image.get_mut(k) {
                    slot.push(a);
                }
            }
            for ((k, image), aux) in page.images.into_iter().enumerate().zip(per_image) {
                let n = pages.len() + 1;
                pages.push(Loaded {
                    name: format!("{stem} · {n:04}.{}", image.format.extension()),
                    source: image.source,
                    format: image.format,
                    width: image.width,
                    height: image.height,
                    role: if k == 0 { role } else { PageRole::Normal },
                    flags: if k == 0 { flags } else { 0 },
                    aux,
                    damaged: image.damaged,
                    chapter: chapter_mark.take(),
                    volume: volume_mark.take(),
                });
            }
        }
    }
    let options = FileOptions {
        path: path.to_path_buf(),
        webtoon: opened.webtoon,
        rtl: opened.rtl,
        redundancy: opened.redundancy,
        block: opened.block_size,
        tail: opened.tail_duplicate,
        cover: opened.has_previews,
        custom_previews: opened.custom_previews,
        page_previews: opened.page_previews,
        recovered: opened.recovered,
        unknown_info: opened.unknown_info,
        damaged,
        unreadable: opened.unreadable,
    };
    Ok((pages, opened.metadata, options))
}

/// A page in the project snapshot.
#[derive(Clone)]
struct ProjectPage {
    source: ChunkSource,
    role: PageRole,
    /// Image flags (alpha, lossless, animation) from the opened `.vnmf`.
    flags: u16,
    /// Attachments; in a webtoon — of this fragment (see [`place_aux`]).
    aux: Vec<AuxInput>,
    height: u32,
}

impl ProjectPage {
    #[cfg(test)]
    fn new(source: ChunkSource, role: PageRole) -> Self {
        let height = source.probe_image().map_or(0, |(_, h, _)| h);
        Self { source, role, flags: 0, aux: Vec::new(), height }
    }
}

/// Where in the project to put a page attachment from a file: `start` — the index of the page's first image,
/// `heights` — the heights of the images from it. Dark version `part` → fragment `part`;
/// an effect with a trigger line → the fragment that line falls into (the line — from its top).
/// Returns the offset from `start` and the attachment with parameters relative to the fragment.
fn place_aux(start: usize, mut aux: AuxInput, heights: &[u32]) -> (usize, AuxInput) {
    let local = start.min(heights.len().saturating_sub(1));
    match aux.kind {
        AuxKind::DarkImage => {
            let k = (local + aux.part as usize).min(heights.len().saturating_sub(1));
            aux.part = 0;
            (k, aux)
        }
        AuxKind::Sfx if aux.trigger_y > 0 => {
            let mut k = local;
            while k + 1 < heights.len() && aux.trigger_y >= heights[k] && heights[k] > 0 {
                aux.trigger_y -= heights[k];
                k += 1;
            }
            (k, aux)
        }
        _ => (local, aux),
    }
}

/// The inverse of [`place_aux`]: attachments of webtoon fragments → attachments of a single page.
fn merge_fragment_aux(fragments: &[ProjectPage]) -> Vec<AuxInput> {
    let mut out = Vec::new();
    let mut top = 0u32;
    for (k, f) in fragments.iter().enumerate() {
        for a in &f.aux {
            let mut a = a.clone();
            match a.kind {
                AuxKind::DarkImage => a.part = k as u32,
                AuxKind::Sfx if k > 0 => a.trigger_y = a.trigger_y.saturating_add(top),
                _ => {}
            }
            out.push(a);
        }
        top = top.saturating_add(f.height);
    }
    out
}

/// File previews.
#[derive(Clone, Copy)]
enum Cover {
    None,
    /// Custom covers, and for the file and volumes without custom ones — from pages.
    Auto,
    /// Custom covers only.
    Custom,
}

/// A project snapshot for building on a background thread.
struct Project {
    pages: Vec<ProjectPage>,
    info: Option<Vec<u8>>,
    webtoon: bool,
    rtl: bool,
    redundancy: u8,
    tail: bool,
    cover: Cover,
    /// Custom covers in list order: the image and the volume (0 — the file).
    covers: Vec<(ChunkSource, u16)>,
    block: BlockSize,
    /// Volumes and chapters (ranges of `pages` indices).
    chapters: Vec<ChapterPlan>,
}

/// Builds the `.vnmf`; returns the file size.
fn build_volume(p: Project, path: &std::path::Path) -> anyhow::Result<u64> {
    anyhow::ensure!(!p.pages.is_empty(), tr("no pages"));
    let mut builder = Builder::new()
        .with_uuid(uuid::Uuid::new_v4().into_bytes())
        .with_block_size(p.block)
        .with_tail_duplicate(p.tail)
        .with_redundancy(p.redundancy)
        .with_mixed_formats(true);
    if let Some(info) = p.info {
        builder.set_info(info);
    }
    if !matches!(p.cover, Cover::None) {
        // Covers of the file and, if there are volumes, of each volume (previews with volume_id), the file first:
        // the first preview in the file is the main one. Custom ones first (in list order, any number of them),
        // then ("From pages") — all pages with the "Cover" role; if there are none — the first page.
        // A file with volumes takes only one from pages — the main cover of the first volume (otherwise
        // the covers of all volumes would be repeated in it).
        let role_covers = |range: std::ops::Range<usize>| -> Vec<ChunkSource> {
            p.pages[range].iter().filter(|p| p.role == PageRole::FrontCover).map(|p| p.source.clone()).collect()
        };
        let mut targets = vec![(0u16, 0..p.pages.len())];
        for (i, plan) in p.chapters.iter().enumerate().filter(|(_, c)| c.volume_start) {
            let end = p.chapters[i + 1..].iter().find(|c| c.volume_start).map_or(p.pages.len(), |c| c.pages.start);
            targets.push((plan.volume_id, plan.pages.start..end));
        }
        let has_volumes = targets.len() > 1;
        for (volume, range) in &targets {
            let own: Vec<ChunkSource> = p.covers.iter().filter(|(_, v)| v == volume).map(|(s, _)| s.clone()).collect();
            let mut from_pages = Vec::new();
            if matches!(p.cover, Cover::Auto) {
                from_pages = role_covers(range.clone());
                if *volume == 0 && has_volumes {
                    // A file with volumes: only the main cover (of the first volume) and only without custom ones.
                    from_pages.truncate(usize::from(own.is_empty()));
                }
                if from_pages.is_empty() && own.is_empty() {
                    from_pages.push(p.pages[range.start].source.clone());
                }
            }
            let mut written: Vec<u32> = Vec::new();
            for source in own.into_iter().chain(from_pages) {
                if !written.contains(&source.crc) {
                    written.push(source.crc);
                    builder.add_preview(source, *volume, 0)?;
                }
            }
        }
        // Those attached to a volume that's no longer in the project are saved too (at the end).
        for (source, volume) in p.covers.iter().filter(|(_, v)| !targets.iter().any(|(t, _)| t == v)) {
            builder.add_preview(source.clone(), *volume, 0)?;
        }
    }
    let base = if p.rtl { PageBitFlags::READING_RTL } else { 0 };
    let mut pages = p.pages.into_iter();
    for plan in &p.chapters {
        builder.set_volume(plan.volume_id);
        let chapter: Vec<ProjectPage> = pages.by_ref().take(plan.pages.len()).collect();
        if p.webtoon {
            // Webtoon: a chapter (episode) is one long page; flags — of the first fragment,
            // attachments — of all fragments (with the fragment number and the line from the top of the page).
            let flags = chapter.first().map_or(0, |c| c.flags);
            let aux = merge_fragment_aux(&chapter);
            let sources = chapter.into_iter().map(|c| c.source).collect();
            builder.add_webtoon(sources, plan.chapter_id, 1, base | flags)?;
            for a in aux {
                builder.attach_aux(a)?;
            }
        } else {
            for (k, page) in chapter.into_iter().enumerate() {
                let number = u16::try_from(k + 1).unwrap_or(u16::MAX);
                builder.add_page(page.source, page.role, plan.chapter_id, number, base | page.flags)?;
                for mut a in page.aux {
                    a.part = 0;
                    builder.attach_aux(a)?;
                }
            }
        }
    }
    builder.build(path)?;
    Ok(std::fs::metadata(path)?.len())
}

struct Options {
    webtoon: adw::SwitchRow,
    rtl: adw::SwitchRow,
    redundancy: adw::SpinRow,
    block: adw::ComboRow,
    cover: adw::ComboRow,
    covers_group: adw::PreferencesGroup,
    tail: adw::SwitchRow,
}

/// A metadata form field: how it's read from `Metadata` and written back.
#[derive(Clone, Copy)]
enum Field {
    /// A string (`title`, `publisher`, …).
    Text(&'static str),
    /// A comma-separated list (`genres`, `tags`, …).
    List(&'static str),
    /// A number (`volume_count`, `chapter_count`).
    Count(&'static str),
    /// People with a role (comma-separated).
    Credit(&'static str),
    /// A catalog identifier.
    Id(&'static str),
}

/// A metadata string field by key.
fn text_slot<'a>(m: &'a mut Metadata, key: &str) -> &'a mut Option<String> {
    match key {
        "title" => &mut m.title,
        "original_title" => &mut m.original_title,
        "romanized_title" => &mut m.romanized_title,
        "original_language" => &mut m.original_language,
        "origin_country" => &mut m.origin_country,
        "serialization" => &mut m.serialization,
        "original_publisher" => &mut m.original_publisher,
        "start_date" => &mut m.start_date,
        "end_date" => &mut m.end_date,
        "language" => &mut m.language,
        "publisher" => &mut m.publisher,
        "imprint" => &mut m.imprint,
        "edition" => &mut m.edition,
        "release_date" => &mut m.release_date,
        "isbn" => &mut m.isbn,
        "translation.notes" => &mut m.translation.notes,
        // Dictionary fields (`ComboRow`).
        "type" => &mut m.kind,
        "publication" => &mut m.publication,
        "status" => &mut m.status,
        "demographic" => &mut m.demographic,
        "age_rating" => &mut m.age_rating,
        "color" => &mut m.color,
        "translation.type" => &mut m.translation.kind,
        "translation.status" => &mut m.translation.status,
        other => unreachable!("unknown form field {other}"),
    }
}

fn list_get(m: &Metadata, key: &str) -> Vec<String> {
    match key {
        "alternative_titles" => m.alternative_titles.iter().map(|a| a.title.clone()).collect(),
        "genres" => m.genres.clone(),
        "tags" => m.tags.clone(),
        "characters" => m.characters.clone(),
        "content_warnings" => m.content_warnings.clone(),
        "translation.groups" => m.translation.groups.clone(),
        "links" => m.links.iter().map(|l| l.url.clone()).collect(),
        other => unreachable!("unknown form field {other}"),
    }
}

fn list_set(m: &mut Metadata, key: &str, values: Vec<String>) {
    match key {
        // The language of an alternative title and a link's label are kept if the entry remains.
        "alternative_titles" => {
            let old = std::mem::take(&mut m.alternative_titles);
            m.alternative_titles =
                values.into_iter().map(|t| old.iter().find(|a| a.title == t).cloned().unwrap_or(metadata::AltTitle { title: t, language: None })).collect();
        }
        "links" => {
            let old = std::mem::take(&mut m.links);
            m.links = values.into_iter().map(|u| old.iter().find(|l| l.url == u).cloned().unwrap_or(metadata::Link { url: u, title: None })).collect();
        }
        "genres" => m.genres = values,
        "tags" => m.tags = values,
        "characters" => m.characters = values,
        "content_warnings" => m.content_warnings = values,
        "translation.groups" => m.translation.groups = values,
        other => unreachable!("unknown form field {other}"),
    }
}

/// Dictionary of a field's values (see `vnmf::metadata::TYPES`, etc.).
type Dict = &'static [(&'static str, &'static str)];

/// The metadata form in the sidebar.
struct MetaForm {
    entries: Vec<(Field, adw::EntryRow)>,
    /// (key, dictionary, row); the first item of the row is "not specified".
    combos: Vec<(&'static str, Dict, adw::ComboRow)>,
    summary: gtk::TextView,
}

impl MetaForm {
    fn fill(&self, m: &Metadata) {
        let mut probe = m.clone();
        for (field, row) in &self.entries {
            let text = match *field {
                Field::Text(key) => text_slot(&mut probe, key).clone().unwrap_or_default(),
                Field::List(key) => list_get(m, key).join(", "),
                Field::Count(key) => match key {
                    "volume_count" => m.volume_count,
                    _ => m.chapter_count,
                }
                .map(|n| n.to_string())
                .unwrap_or_default(),
                Field::Credit(role) => m.credited(role).join(", "),
                Field::Id(key) => m.identifiers.get(key).cloned().unwrap_or_default(),
            };
            row.set_text(&text);
        }
        for (key, dict, row) in &self.combos {
            let value = text_slot(&mut probe, key).clone();
            let index = value.and_then(|v| dict.iter().position(|(k, _)| *k == v)).map_or(0, |i| i + 1);
            row.set_selected(index as u32);
        }
        self.summary.buffer().set_text(m.summary.as_deref().unwrap_or(""));
    }

    /// Metadata: `base` with the form fields on top.
    fn read(&self, base: &Metadata) -> Metadata {
        let mut m = base.clone();
        let clean = |s: glib::GString| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        for (field, row) in &self.entries {
            match *field {
                Field::Text(key) => *text_slot(&mut m, key) = clean(row.text()),
                Field::List(key) => list_set(&mut m, key, metadata::split_list(&row.text())),
                Field::Count(key) => {
                    let n = row.text().trim().parse().ok();
                    if key == "volume_count" { m.volume_count = n } else { m.chapter_count = n }
                }
                Field::Credit(role) => m.set_credited(role, &metadata::split_list(&row.text())),
                Field::Id(key) => match clean(row.text()) {
                    Some(id) => {
                        m.identifiers.insert(key.to_string(), id);
                    }
                    None => {
                        m.identifiers.remove(key);
                    }
                },
            }
        }
        for (key, dict, row) in &self.combos {
            let selected = row.selected() as usize;
            *text_slot(&mut m, key) = selected.checked_sub(1).and_then(|i| dict.get(i)).map(|(k, _)| (*k).to_string());
        }
        let buffer = self.summary.buffer();
        m.summary = clean(buffer.text(&buffer.start_iter(), &buffer.end_iter(), false));
        m
    }
}

fn build_sidebar() -> (adw::PreferencesPage, MetaForm, Options) {
    let page = adw::PreferencesPage::new();
    let mut entries: Vec<(Field, adw::EntryRow)> = Vec::new();
    let mut combos = Vec::new();

    let entry = |entries: &mut Vec<(Field, adw::EntryRow)>, field: Field, title: &str| {
        let row = adw::EntryRow::builder().title(title).build();
        if matches!(field, Field::Count(_)) {
            row.set_input_purpose(gtk::InputPurpose::Digits);
        }
        entries.push((field, row.clone()));
        row
    };
    let combo = |combos: &mut Vec<_>, key: &'static str, dict: Dict, title: &str| {
        let mut labels = vec![tr("not specified")];
        labels.extend(dict.iter().map(|(_, l)| tr(l)));
        let row = adw::ComboRow::builder().title(title).model(&gtk::StringList::new(&labels)).build();
        combos.push((key, dict, row.clone()));
        row
    };
    let more = |title: &str, subtitle: &str| adw::ExpanderRow::builder().title(title).subtitle(subtitle).build();

    // The work
    let work = adw::PreferencesGroup::builder()
        .title(tr("Work"))
        .description(tr("VNMF metadata: all fields are optional, lists are comma-separated. A CBZ gets them as ComicInfo.xml"))
        .build();
    let import = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text(tr("Import vnmf.json or ComicInfo.xml"))
        .valign(gtk::Align::Center)
        .action_name("win.import-info")
        .build();
    import.add_css_class("flat");
    work.set_header_suffix(Some(&import));
    work.add(&entry(&mut entries, Field::Text("title"), tr("Title")));
    work.add(&entry(&mut entries, Field::Text("original_title"), tr("Original title")));
    work.add(&combo(&mut combos, "type", metadata::TYPES, tr("Type")));
    work.add(&combo(&mut combos, "status", metadata::STATUSES, tr("Status")));
    let titles = more(tr("Other titles and origin"), tr("romanized, alternative, language and country"));
    titles.add_row(&entry(&mut entries, Field::Text("romanized_title"), tr("Romanized")));
    titles.add_row(&entry(&mut entries, Field::List("alternative_titles"), tr("Alternative titles")));
    titles.add_row(&entry(&mut entries, Field::Text("original_language"), tr("Original language (ja, ko, zh)")));
    titles.add_row(&entry(&mut entries, Field::Text("origin_country"), tr("Country (JP, KR, CN)")));
    titles.add_row(&combo(&mut combos, "publication", metadata::PUBLICATIONS, tr("Form of publication")));
    titles.add_row(&combo(&mut combos, "demographic", metadata::DEMOGRAPHICS, tr("Audience")));
    work.add(&titles);
    page.add(&work);

    // Authors
    let people = adw::PreferencesGroup::builder().title(tr("Authors")).build();
    people.add(&entry(&mut entries, Field::Credit("author"), tr("Author (story and art)")));
    people.add(&entry(&mut entries, Field::Credit("story"), tr("Writer")));
    people.add(&entry(&mut entries, Field::Credit("art"), tr("Artist")));
    let roles = more(tr("Other roles"), tr("original creator, colorist, editor…"));
    for role in ["original_creator", "colorist", "letterer", "cover_artist", "assistant", "editor"] {
        roles.add_row(&entry(&mut entries, Field::Credit(role), &metadata::label(metadata::ROLES, role)));
    }
    people.add(&roles);
    page.add(&people);

    // Translation and edition
    let edition = adw::PreferencesGroup::builder().title(tr("Translation and edition")).build();
    edition.add(&entry(&mut entries, Field::Text("language"), tr("Text language (en, uk)")));
    edition.add(&combo(&mut combos, "translation.type", metadata::TRANSLATION_TYPES, tr("Translation")));
    edition.add(&combo(&mut combos, "translation.status", metadata::STATUSES, tr("Translation status")));
    edition.add(&entry(&mut entries, Field::List("translation.groups"), tr("Translation team")));
    let team = more(tr("Team members"), tr("translator, proofreader, typesetter…"));
    for role in ["translator", "proofreader", "typesetter", "cleaner", "redrawer", "quality_checker", "raw_provider"] {
        team.add_row(&entry(&mut entries, Field::Credit(role), &metadata::label(metadata::ROLES, role)));
    }
    team.add_row(&entry(&mut entries, Field::Text("translation.notes"), tr("Translation notes")));
    edition.add(&team);
    let print = more(tr("Edition"), tr("publisher, date, ISBN"));
    print.add_row(&entry(&mut entries, Field::Text("publisher"), tr("Publisher")));
    print.add_row(&entry(&mut entries, Field::Text("imprint"), tr("Imprint")));
    print.add_row(&entry(&mut entries, Field::Text("edition"), tr("Edition (Deluxe, digital…)")));
    print.add_row(&entry(&mut entries, Field::Text("release_date"), tr("Release date (YYYY-MM-DD)")));
    print.add_row(&entry(&mut entries, Field::Text("isbn"), "ISBN"));
    edition.add(&print);
    page.add(&edition);

    // Description
    let about = adw::PreferencesGroup::builder().title(tr("Description")).build();
    about.add(&entry(&mut entries, Field::List("genres"), tr("Genres")));
    about.add(&entry(&mut entries, Field::List("tags"), tr("Tags")));
    about.add(&combo(&mut combos, "age_rating", metadata::AGE_RATINGS, tr("Age rating")));
    let details = more(tr("More"), tr("characters, warnings, color"));
    details.add_row(&entry(&mut entries, Field::List("characters"), tr("Characters")));
    details.add_row(&entry(&mut entries, Field::List("content_warnings"), tr("Content warnings")));
    details.add_row(&combo(&mut combos, "color", metadata::COLORS, tr("Color")));
    about.add(&details);
    let summary = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .height_request(120)
        .margin_top(12)
        .build();
    summary.add_css_class("card");
    summary.update_property(&[gtk::accessible::Property::Label(tr("Description"))]);
    about.add(&summary);
    page.add(&about);

    // Series, catalogs
    let series = adw::PreferencesGroup::builder().title(tr("Series and catalogs")).build();
    let publication = more(tr("Original publication"), tr("magazine or platform, years, number of volumes"));
    publication.add_row(&entry(&mut entries, Field::Text("serialization"), tr("Magazine / platform")));
    publication.add_row(&entry(&mut entries, Field::Text("original_publisher"), tr("Original publisher")));
    publication.add_row(&entry(&mut entries, Field::Text("start_date"), tr("Start (YYYY-MM-DD)")));
    publication.add_row(&entry(&mut entries, Field::Text("end_date"), tr("End")));
    publication.add_row(&entry(&mut entries, Field::Count("volume_count"), tr("Total volumes")));
    publication.add_row(&entry(&mut entries, Field::Count("chapter_count"), tr("Total chapters")));
    series.add(&publication);
    let catalogs = more(tr("Catalogs and links"), "AniList, MyAnimeList, MangaDex…");
    for id in ["anilist", "mal", "mangadex", "mangaupdates", "shikimori", "webtoons"] {
        catalogs.add_row(&entry(&mut entries, Field::Id(id), &metadata::label(metadata::IDENTIFIERS, id)));
    }
    catalogs.add_row(&entry(&mut entries, Field::List("links"), tr("Links (comma-separated URLs)")));
    series.add(&catalogs);
    page.add(&series);

    let form = MetaForm { entries, combos, summary };

    // Parameters
    let opts = adw::PreferencesGroup::builder().title(tr("Parameters")).build();
    let webtoon = adw::SwitchRow::builder().title(tr("Webtoon")).subtitle(tr("All images form one long page; formats can be mixed")).build();
    let rtl = adw::SwitchRow::builder().title(tr("Right to left")).subtitle(tr("Manga reading direction")).build();
    let redundancy = adw::SpinRow::builder()
        .title(tr("Redundancy, %"))
        .subtitle(tr("Protection against media damage: a contiguous region of the same size can be recovered"))
        .adjustment(&gtk::Adjustment::new(10.0, 0.0, f64::from(spec::MAX_REDUNDANCY), 1.0, 5.0, 0.0))
        .build();
    let tail = adw::SwitchRow::builder().title(tr("Metadata backup copy")).subtitle(tr("A copy of the tables at the end of the file")).active(true).build();
    let cover = adw::ComboRow::builder()
        .title(tr("Covers"))
        .model(&gtk::StringList::new(&[tr("None"), tr("From pages"), tr("Custom only")]))
        .selected(COVER_AUTO)
        .subtitle(tr(COVER_HINTS[COVER_AUTO as usize]))
        .build();
    // Custom covers: any number of images from files or pages; they don't become pages.
    let add =
        gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text(tr("Add images…")).valign(gtk::Align::Center).action_name("win.add-covers").build();
    add.add_css_class("flat");
    let covers_group = adw::PreferencesGroup::builder()
        .title(tr("Custom covers"))
        .description(tr("Images from files — they don't become pages. A cover from pages is the \"Cover\" page role. For the file and each volume the first one is the main one, the rest are additional."))
        .header_suffix(&add)
        .build();
    let group = covers_group.clone();
    cover.connect_selected_notify(move |combo| {
        let choice = combo.selected().min(COVER_CUSTOM);
        combo.set_subtitle(tr(COVER_HINTS[choice as usize]));
        group.set_visible(choice != COVER_NONE);
    });
    let block = adw::ComboRow::builder().title(tr("Block size")).model(&gtk::StringList::new(&["4 KiB", "8 KiB", "16 KiB", "32 KiB"])).build();
    for row in [webtoon.upcast_ref::<gtk::Widget>(), rtl.upcast_ref(), redundancy.upcast_ref(), tail.upcast_ref(), cover.upcast_ref(), block.upcast_ref()] {
        opts.add(row);
    }
    page.add(&opts);
    page.add(&covers_group);

    (page, form, Options { webtoon, rtl, redundancy, block, cover, covers_group, tail })
}

/// Page menu: chapter/volume start and (not in a webtoon) the role. Compact — the card is narrow.
fn page_menu_model(item: &Item, first: bool, webtoon: bool) -> gio::Menu {
    let menu = gio::Menu::new();
    let structure = gio::Menu::new();
    if !first {
        let label = if item.chapter.is_some() { tr("Don't start a chapter here") } else { tr("Start a new chapter here") };
        let entry = gio::MenuItem::new(Some(label), None);
        entry.set_action_and_target_value(Some("win.chapter-break"), Some(&item.id.to_variant()));
        structure.append_item(&entry);
    }
    let label = if item.volume.is_some() { tr("Don't start a volume here") } else { tr("Start a new volume here") };
    let entry = gio::MenuItem::new(Some(label), None);
    entry.set_action_and_target_value(Some("win.volume-break"), Some(&item.id.to_variant()));
    structure.append_item(&entry);
    menu.append_section(Some(tr("Structure")), &structure);
    let extras = gio::Menu::new();
    let label = if item.aux.is_empty() { tr("Sound, text, dark version…").to_string() } else { trf!("Attachments ({0})…", item.aux.len()) };
    let entry = gio::MenuItem::new(Some(&label), None);
    entry.set_action_and_target_value(Some("win.page-aux"), Some(&item.id.to_variant()));
    extras.append_item(&entry);
    menu.append_section(Some(tr("Attachments")), &extras);
    if !webtoon {
        let roles = gio::Menu::new();
        for (code, label) in ROLE_LABELS.iter().enumerate() {
            let entry = gio::MenuItem::new(Some(tr(label)), None);
            entry.set_action_and_target_value(Some("win.role"), Some(&(item.id, code as u8).to_variant()));
            roles.append_item(&entry);
        }
        menu.append_section(Some(tr("Page role")), &roles);
    }
    menu
}

/// Moves the chapter (`volume = false`) or volume starting at index `a` before index `t`
/// (`len` — to the end). Returns the new index of the first moved page; `None` — nothing
/// changed. `volume_header`: the target is a volume header ("before the volume"); otherwise a chapter dropped on
/// the header of a volume's first chapter becomes the first chapter of that volume.
fn move_group_in(items: &mut Vec<Item>, a: usize, volume: bool, t: usize, volume_header: bool) -> Option<usize> {
    let (plans, has_volumes) = {
        let markers: Vec<_> = items.iter().map(|i| (i.chapter.as_ref(), i.volume.as_ref())).collect();
        plan_structure(&markers, None)
    };
    let k = plans.iter().position(|p| p.pages.start == a)?;
    let len = items.len();
    let volume_end = plans[k + 1..].iter().find(|p| p.volume_start).map_or(len, |p| p.pages.start);
    let b = if volume { volume_end } else { plans[k].pages.end };
    if t > len || (a..=b).contains(&t) {
        return None; // onto its own place
    }
    // The first page of the project starts a chapter (and a volume) implicitly — after a move it may
    // end up inside, so the markers are made explicit.
    if let Some(first) = items.first_mut() {
        first.chapter.get_or_insert_with(Section::default);
        if has_volumes {
            first.volume.get_or_insert_with(Section::default);
        }
    }
    items[a].chapter.get_or_insert_with(Section::default);
    if !volume {
        // The chapter goes without the volume: the volume marker stays on the next chapter of that volume
        // (if the chapter was the only one in the volume — the volume disappears).
        let mark = items[a].volume.take();
        if b < volume_end {
            items[b].volume = mark;
        }
    }
    let mut block: Vec<Item> = items.drain(a..b).collect();
    let t = if t > a { t - (b - a) } else { t };
    // Onto the header of a volume's first chapter — the chapter becomes the first in that volume.
    if !volume
        && !volume_header
        && let Some(next) = items.get_mut(t)
        && next.volume.is_some()
    {
        block[0].volume = next.volume.take();
    }
    items.splice(t..t, block);
    Some(t)
}

/// Prefix of a dragged chapter or volume (pages — `vnmf-page:`).
const GROUP_PREFIX: &str = "vnmf-group:";

fn group_token(volume: bool, start_id: u64) -> String {
    format!("{GROUP_PREFIX}{}:{start_id}", if volume { "volume" } else { "chapter" })
}

/// `(volume?, id of the first page)`.
fn parse_group(s: &str) -> Option<(bool, u64)> {
    let (kind, id) = s.strip_prefix(GROUP_PREFIX)?.split_once(':')?;
    Some((kind == "volume", id.parse().ok()?))
}

/// The text currently being dragged (from `preload`) — so the target knows right away whether it'll accept it.
fn dragged_text(target: &gtk::DropTarget) -> Option<String> {
    target.value()?.get::<String>().ok()
}

/// A grid of cards for a single chapter.
fn new_flow() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .homogeneous(true)
        .valign(gtk::Align::Start)
        .row_spacing(12)
        .column_spacing(12)
        .max_children_per_line(30)
        .activate_on_single_click(false)
        .build()
}

fn small_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).build();
    button.add_css_class("flat");
    button.add_css_class("circular");
    button
}

fn find_child<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Option<T> {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if let Ok(found) = w.clone().downcast::<T>() {
            return Some(found);
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            stack.push(c);
        }
    }
    None
}

/// Source bytes (a file is read from disk).
fn chunk_bytes(chunk: &ChunkSource) -> Option<Vec<u8>> {
    match &chunk.data {
        vnmf_core::ChunkData::File(path) => std::fs::read(path).ok(),
        data => data.bytes().map(<[u8]>::to_vec),
    }
}

/// Attachment data format by the first bytes.
fn aux_format(chunk: &ChunkSource) -> AuxFormat {
    match &chunk.data {
        vnmf_core::ChunkData::File(path) => {
            use std::io::Read;
            let mut head = Vec::new();
            std::fs::File::open(path).and_then(|f| f.take(64 * 1024).read_to_end(&mut head)).map_or(AuxFormat::Unknown, |_| AuxFormat::detect(&head))
        }
        data => data.bytes().map_or(AuxFormat::Unknown, |b| AuxFormat::detect(&b[..b.len().min(64 * 1024)])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnmf_core::VnmfReader;

    fn png(seed: u8) -> ChunkSource {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        v.extend_from_slice(&800u32.to_be_bytes());
        v.extend_from_slice(&1200u32.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        v.extend((0..4000).map(|i| (i as u8).wrapping_add(seed)));
        ChunkSource::from_bytes(v).unwrap()
    }

    #[test]
    fn metadata_from_project_is_vnmf_schema() {
        let (chapters, _) = plan_structure(&[(Some(&Section { title: "Prologue".into(), ..Default::default() }), None), (None, None)], None);
        let mut meta = Metadata::new();
        meta.title = Some("T".into());
        meta.set_credited("translator", &["Vera".into()]);
        meta.contents = plan_contents(&chapters);
        let v: serde_json::Value = serde_json::from_slice(&meta.to_bytes()).unwrap();
        assert_eq!(v["schema"], metadata::SCHEMA);
        assert_eq!(v["credits"][0]["roles"][0], "translator");
        assert_eq!(v["contents"]["volumes"][0]["chapters"][0]["title"], "Prologue");
        assert!(v.get("ComicInfo").is_none());
    }

    #[test]
    fn builds_paged_and_webtoon_volumes() {
        let dir = std::env::temp_dir().join(format!("vnmf-creator-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for webtoon in [false, true] {
            let path = dir.join(format!("t{webtoon}.vnmf"));
            let project = Project {
                pages: vec![
                    ProjectPage::new(png(1), PageRole::Normal),
                    ProjectPage::new(png(2), PageRole::FrontCover),
                    ProjectPage::new(png(3), PageRole::Normal),
                ],
                info: Some(Metadata { title: Some("T".into()), ..Metadata::new() }.to_bytes()),
                webtoon,
                rtl: true,
                redundancy: 10,
                tail: true,
                cover: Cover::Auto,
                covers: Vec::new(),
                block: BlockSize::Page4k,
                chapters: plan_structure(&[(None, None); 3], Some(7)).0,
            };
            assert!(build_volume(project, &path).unwrap() > 0);
            let r = VnmfReader::open(&path).unwrap();
            assert_eq!(r.redundancy(), 10);
            assert!(r.verify_parity().unwrap().is_clean());
            assert_eq!(r.previews_count(), 1);
            if webtoon {
                assert_eq!(r.pages_count(), 1);
                assert_eq!(r.pages()[0].chunk_count.get(), 3);
            } else {
                assert_eq!(r.pages_count(), 3);
                assert_eq!(r.pages()[1].role, PageRole::FrontCover as u8);
                assert_eq!(r.pages()[0].chapter_id.get(), 7);
                assert_ne!(r.pages()[0].flags.get() & PageBitFlags::READING_RTL, 0);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Spread roles chosen in Creator go into the file unchanged (Reader reads them).
    /// `VNMF_KEEP_TEST_FILE=<path>` — keep the file for manual checking in Reader.
    #[test]
    fn spread_roles_are_written_to_file() {
        let path = std::env::var("VNMF_KEEP_TEST_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join(format!("vnmf-creator-roles-{}.vnmf", std::process::id())));
        let roles = [
            PageRole::FrontCover,
            PageRole::Normal,
            PageRole::SpreadRight, // manga: the right half comes first
            PageRole::SpreadLeft,
            PageRole::SpreadSingle,
            PageRole::Normal,
            PageRole::BackCover,
        ];
        // `VNMF_TEST_IMAGES=<folder>` — real images instead of synthetic ones (for viewing in Reader).
        let real: Vec<PathBuf> = std::env::var("VNMF_TEST_IMAGES")
            .ok()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|it| {
                let mut v: Vec<PathBuf> = it.filter_map(|e| Some(e.ok()?.path())).collect();
                v.sort();
                v
            })
            .unwrap_or_default();
        let source = |i: usize| match real.get(i) {
            Some(path) => ChunkSource::from_file(path).unwrap(),
            None => png(i as u8),
        };
        let project = Project {
            pages: roles.iter().enumerate().map(|(i, r)| ProjectPage::new(source(i), *r)).collect(),
            info: None,
            webtoon: false,
            rtl: true,
            redundancy: 0,
            tail: true,
            cover: Cover::None,
            covers: Vec::new(),
            block: BlockSize::Page4k,
            chapters: plan_structure(&[(None, None); 7], None).0,
        };
        build_volume(project, &path).unwrap();
        let r = VnmfReader::open(&path).unwrap();
        let written: Vec<u8> = r.pages().iter().map(|p| p.role).collect();
        assert_eq!(written, roles.iter().map(|r| *r as u8).collect::<Vec<_>>());
        // Role menu labels match the role codes.
        for (code, label) in ROLE_LABELS.iter().enumerate() {
            let role = PageRole::try_from(code as u8).unwrap();
            assert_eq!(ROLE_LABELS[role as usize], *label);
        }
        if std::env::var("VNMF_KEEP_TEST_FILE").is_err() {
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn plan_follows_markers() {
        let ch = |n: &str, t: &str| Section { number: n.into(), title: t.into(), ..Default::default() };
        let (c1, c2, v2) = (ch("", "Prologue"), ch("10.5", ""), ch("", "Storm"));
        // 0..2 — volume 1 / chapter "Prologue"; 2 — chapter 10.5; 3.. — volume 2.
        let markers = [(Some(&c1), None), (None, None), (Some(&c2), None), (None, None), (None, None)];
        let (plans, volumes) = plan_structure(&markers, None);
        assert!(!volumes, "without volume markers there are no volumes");
        assert_eq!(plans.iter().map(|p| (p.pages.clone(), p.volume_id)).collect::<Vec<_>>(), vec![(0..2, 0), (2..5, 0)]);

        let markers = [(Some(&c1), Some(&Section::default())), (None, None), (Some(&c2), None), (None, Some(&v2)), (None, None)];
        let (plans, volumes) = plan_structure(&markers, None);
        assert!(volumes);
        let got: Vec<_> = plans.iter().map(|p| (p.pages.clone(), p.volume_id, p.chapter_id, p.volume_start)).collect();
        // The number "10.5" isn't an integer — the numbers are ordinals, "10.5" goes into the contents.
        assert_eq!(got, vec![(0..2, 1, 1, true), (2..3, 1, 2, false), (3..5, 2, 3, true)]);
        assert_eq!(plans[1].label(), "Chapter 10.5");
        assert_eq!(plans[2].volume_label(), "Volume 2 — Storm");
        let contents = plan_contents(&plans);
        assert_eq!(contents.chapter(1, 1).unwrap().title.as_deref(), Some("Prologue"));
        assert_eq!(contents.chapter(1, 2).unwrap().number.as_deref(), Some("10.5"));
        assert_eq!(contents.volume(2).unwrap().title.as_deref(), Some("Storm"));
    }

    #[test]
    fn builds_volumes_with_covers() {
        let path = std::env::temp_dir().join(format!("vnmf-creator-volumes-{}.vnmf", std::process::id()));
        let v = Section::default();
        let markers = [(None, Some(&v)), (None, None), (Some(&Section::default()), None), (None, Some(&v)), (None, None)];
        let (chapters, _) = plan_structure(&markers, None);
        let info = Some(Metadata { contents: plan_contents(&chapters), ..Metadata::new() }.to_bytes());
        // Volume 1 — two pages with the "Cover" role (1 and 2), volume 2 — none.
        let roles = [PageRole::Normal, PageRole::FrontCover, PageRole::FrontCover, PageRole::Normal, PageRole::Normal];
        let project = Project {
            pages: (0..5).map(|i| ProjectPage::new(png(i), roles[i as usize])).collect(),
            info,
            webtoon: false,
            rtl: false,
            redundancy: 0,
            tail: true,
            cover: Cover::Auto,
            covers: Vec::new(),
            block: BlockSize::Page4k,
            chapters,
        };
        build_volume(project, &path).unwrap();
        let r = VnmfReader::open(&path).unwrap();
        let s = r.structure();
        assert_eq!(s.iter().map(|v| (v.volume_id, v.chapters.len())).collect::<Vec<_>>(), vec![(1, 2), (2, 1)]);
        // The file — the main cover of the first volume; volume 1 — both "Covers"; volume 2 — the first page.
        let previews: Vec<(u16, u32)> = r.previews().iter().map(|p| (p.volume_id.get(), p.crc.get())).collect();
        assert_eq!(previews, vec![(0, png(1).crc), (1, png(1).crc), (1, png(2).crc), (2, png(3).crc)]);
        let _ = std::fs::remove_file(&path);
    }

    fn item(id: u64, chapter: Option<&str>, volume: Option<&str>) -> Item {
        let section = |t: &str| Section { title: t.into(), ..Default::default() };
        Item {
            id,
            name: String::new(),
            source: png(id as u8),
            format: PageFormat::Png,
            width: 1,
            height: 1,
            role: PageRole::Normal,
            thumb: None,
            chapter: chapter.map(section),
            volume: volume.map(section),
            aux: Vec::new(),
            flags: 0,
            damaged: false,
        }
    }

    /// (volume, chapter) of each page by id.
    fn layout(items: &[Item]) -> Vec<(u64, String, String)> {
        let markers: Vec<_> = items.iter().map(|i| (i.chapter.as_ref(), i.volume.as_ref())).collect();
        let (plans, _) = plan_structure(&markers, None);
        let mut out = Vec::new();
        for p in &plans {
            let vol = plans.iter().rev().find(|q| q.volume_start && q.pages.start <= p.pages.start);
            let v = vol.map(|q| items[q.pages.start].volume.as_ref().map(|s| s.title.clone()).unwrap_or_default()).unwrap_or_default();
            let c = items[p.pages.start].chapter.as_ref().map(|s| s.title.clone()).unwrap_or_default();
            out.extend(p.pages.clone().map(|i| (items[i].id, v.clone(), c.clone())));
        }
        out
    }

    #[test]
    fn moves_chapters_and_volumes() {
        // Volume A: chapters a1 (1,2), a2 (3); volume B: b1 (4), b2 (5,6).
        let project = || {
            vec![
                item(1, Some("a1"), Some("A")),
                item(2, None, None),
                item(3, Some("a2"), None),
                item(4, Some("b1"), Some("B")),
                item(5, Some("b2"), None),
                item(6, None, None),
            ]
        };
        let ids = |items: &[Item]| items.iter().map(|i| i.id).collect::<Vec<_>>();

        // Volume B — before volume A.
        let mut items = project();
        assert_eq!(move_group_in(&mut items, 3, true, 0, true), Some(0));
        assert_eq!(ids(&items), [4, 5, 6, 1, 2, 3]);
        assert_eq!(layout(&items)[3], (1, "A".into(), "a1".into()));
        assert_eq!(layout(&items)[0], (4, "B".into(), "b1".into()));

        // The first chapter of volume A — to the end: the volume A marker moves to a2.
        let mut items = project();
        assert_eq!(move_group_in(&mut items, 0, false, 6, true), Some(4));
        assert_eq!(ids(&items), [3, 4, 5, 6, 1, 2]);
        let l = layout(&items);
        assert_eq!(l[0], (3, "A".into(), "a2".into()));
        assert_eq!(l[4], (1, "B".into(), "a1".into()));

        // Chapter b2 — onto the header of chapter a1 (the first in volume A): becomes the first in volume A.
        let mut items = project();
        assert_eq!(move_group_in(&mut items, 4, false, 0, false), Some(0));
        assert_eq!(ids(&items), [5, 6, 1, 2, 3, 4]);
        let l = layout(&items);
        assert_eq!(l[0], (5, "A".into(), "b2".into()));
        assert_eq!(l[2], (1, "A".into(), "a1".into()));
        assert_eq!(l[5], (4, "B".into(), "b1".into()));

        // The same chapter — onto the header of volume A: before the volume (becomes a separate first volume).
        let mut items = project();
        move_group_in(&mut items, 4, false, 0, true);
        assert_eq!(ids(&items), [5, 6, 1, 2, 3, 4]);
        assert_eq!(layout(&items)[2], (1, "A".into(), "a1".into()));

        // Onto its own place — no change.
        let mut items = project();
        assert_eq!(move_group_in(&mut items, 2, false, 3, false), None);
        assert_eq!(move_group_in(&mut items, 2, false, 2, false), None);
    }
    /// An opened `.vnmf` is rebuilt into a new file with different parameters without loss.
    #[test]
    fn reopened_file_rebuilds_with_new_parameters() {
        let dir = std::env::temp_dir().join(format!("vnmf-creator-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, out) = (dir.join("src.vnmf"), dir.join("out.vnmf"));
        let v = Section { title: "First".into(), date: Some("2024".into()), ..Default::default() };
        let c = Section { number: "10.5".into(), title: "Extra".into(), ..Default::default() };
        let markers = [(None, Some(&v)), (None, None), (Some(&c), None)];
        let (chapters, _) = plan_structure(&markers, None);
        let meta = Metadata { title: Some("T".into()), contents: plan_contents(&chapters), ..Metadata::new() };
        let mut pages: Vec<_> = (0..3).map(|i| ProjectPage::new(png(i), PageRole::Normal)).collect();
        pages[0].role = PageRole::FrontCover;
        let mut music = AuxInput::new(AuxKind::Music, ChunkSource::from_bytes(b"OggS\0music".to_vec()).unwrap());
        music.flags = AuxFlags::LOOP;
        music.pages = 1;
        music.duration_ms = 45_000;
        music.volume = 70;
        pages[1].aux = vec![music, AuxInput::new(AuxKind::Text, ChunkSource::from_bytes("Text".as_bytes().to_vec()).unwrap())];
        let project = |pages, chapters, info, redundancy| Project {
            pages,
            info,
            webtoon: false,
            rtl: true,
            redundancy,
            tail: true,
            cover: Cover::None,
            covers: Vec::new(),
            block: BlockSize::Page4k,
            chapters,
        };
        build_volume(project(pages, chapters, Some(meta.to_bytes()), 10), &src).unwrap();

        let (loaded, meta, opts) = load_vnmf(&src).unwrap();
        assert_eq!((opts.redundancy, opts.rtl, opts.damaged), (10, true, 0));
        assert_eq!(loaded[0].role, PageRole::FrontCover);
        assert_eq!(loaded[2].chapter.as_ref().unwrap().number, "10.5");
        assert_eq!(loaded[0].volume.as_ref().unwrap().date.as_deref(), Some("2024"));
        let markers: Vec<_> = loaded.iter().map(|l| (l.chapter.as_ref(), l.volume.as_ref())).collect();
        let (chapters, _) = plan_structure(&markers, None);
        let mut meta = meta.unwrap();
        meta.contents = plan_contents(&chapters);
        assert_eq!(loaded[1].aux.len(), 2);
        let pages =
            loaded.iter().map(|l| ProjectPage { source: l.source.clone(), role: l.role, flags: l.flags, aux: l.aux.clone(), height: l.height }).collect();
        build_volume(project(pages, chapters, Some(meta.to_bytes()), 0), &out).unwrap();

        let (a, b) = (VnmfReader::open(&src).unwrap(), VnmfReader::open(&out).unwrap());
        assert_eq!(b.redundancy(), 0);
        assert_eq!(a.pages_count(), b.pages_count());
        for i in 0..a.pages_count() {
            assert_eq!(a.get_page_data(i).unwrap(), b.get_page_data(i).unwrap());
            assert_eq!(a.page_aux(i).len(), b.page_aux(i).len());
            for (ka, kb) in a.page_aux_range(i).zip(b.page_aux_range(i)) {
                assert_eq!(a.get_aux_data(ka).unwrap(), b.get_aux_data(kb).unwrap());
                let (ra, rb) = (a.aux()[ka], b.aux()[kb]);
                assert_eq!((ra.kind, ra.flags.get(), ra.volume, ra.delay_ms.get()), (rb.kind, rb.flags.get(), rb.volume, rb.delay_ms.get()));
                assert_eq!((ra.duration_ms.get(), ra.pages.get()), (rb.duration_ms.get(), rb.pages.get()));
            }
            let (pa, pb) = (&a.pages()[i], &b.pages()[i]);
            assert_eq!((pa.role, pa.flags.get(), pa.chapter_id.get(), pa.volume_id.get()), (pb.role, pb.flags.get(), pb.chapter_id.get(), pb.volume_id.get()));
        }
        assert_eq!(Metadata::from_info(a.info_bytes()), Metadata::from_info(b.info_bytes()));
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// Webtoon: attachments are tied to fragments — in the file the dark version gets the fragment number,
    /// and the effect line is counted from the top of the whole page; when opening — back to fragments.
    #[test]
    fn webtoon_aux_follow_fragments() {
        let path = std::env::temp_dir().join(format!("vnmf-creator-webtoon-aux-{}.vnmf", std::process::id()));
        let (chapters, _) = plan_structure(&[(None, None), (None, None)], None);
        let mut pages: Vec<_> = (0..2).map(|i| ProjectPage::new(png(i), PageRole::Normal)).collect();
        let mut sfx = AuxInput::new(AuxKind::Sfx, ChunkSource::from_bytes(b"ID3\x04sfx".to_vec()).unwrap());
        sfx.trigger_y = 5;
        pages[1].aux = vec![AuxInput::new(AuxKind::DarkImage, png(50)), sfx];
        let h0 = pages[0].height;
        let project = Project {
            pages,
            info: None,
            webtoon: true,
            rtl: false,
            redundancy: 0,
            tail: false,
            cover: Cover::None,
            covers: Vec::new(),
            block: BlockSize::Page4k,
            chapters,
        };
        build_volume(project, &path).unwrap();
        let r = VnmfReader::open(&path).unwrap();
        let aux = r.page_aux(0);
        assert_eq!((aux[0].kind(), aux[0].part.get()), (AuxKind::DarkImage, 1));
        assert_eq!((aux[1].kind(), aux[1].trigger_y.get()), (AuxKind::Sfx, h0 + 5));
        let (loaded, _, _) = load_vnmf(&path).unwrap();
        assert!(loaded[0].aux.is_empty());
        assert_eq!(loaded[1].aux.iter().map(|a| (a.kind, a.part, a.trigger_y)).collect::<Vec<_>>(), [(AuxKind::DarkImage, 0, 0), (AuxKind::Sfx, 0, 5)]);
        let _ = std::fs::remove_file(&path);
    }
    /// Custom covers are separate images, any number of them: they don't become pages, they go first
    /// for their file or volume, and volumes without custom ones take the cover from pages. When the file is reopened
    /// they're recognized as custom — with volumes and in the same order.
    #[test]
    fn custom_covers_are_not_pages() {
        let path = std::env::temp_dir().join(format!("vnmf-creator-preview-{}.vnmf", std::process::id()));
        let v = Section::default();
        let (chapters, _) = plan_structure(&[(None, Some(&v)), (None, None), (None, Some(&v))], None);
        let (main, extra, vol2) = (png(200), png(201), png(202));
        let covers = vec![(vol2.clone(), 2), (main.clone(), 0), (extra.clone(), 0), (png(203), 9)];
        let project = |cover, covers| Project {
            pages: (0..3).map(|i| ProjectPage::new(png(i), PageRole::Normal)).collect(),
            info: None,
            webtoon: false,
            rtl: false,
            redundancy: 0,
            tail: true,
            cover,
            covers,
            block: BlockSize::Page4k,
            chapters: chapters.clone(),
        };
        build_volume(project(Cover::Auto, covers.clone()), &path).unwrap();
        let r = VnmfReader::open(&path).unwrap();
        assert_eq!(r.pages_count(), 3);
        let previews: Vec<(u16, u32)> = r.previews().iter().map(|p| (p.volume_id.get(), p.crc.get())).collect();
        // The file — two custom ones (main first), volume 1 — from a page, volume 2 — custom, volume 9 (absent) — at the end.
        assert_eq!(previews, vec![(0, main.crc), (0, extra.crc), (1, png(0).crc), (2, vol2.crc), (9, png(203).crc)]);
        let (_, _, opts) = load_vnmf(&path).unwrap();
        let custom: Vec<(u32, u16)> = opts.custom_previews.iter().map(|(p, v)| (p.crc, *v)).collect();
        assert_eq!(custom, vec![(main.crc, 0), (extra.crc, 0), (vol2.crc, 2), (png(203).crc, 9)]);
        assert!(opts.page_previews);

        // "Custom only": volume 1 without a cover.
        build_volume(project(Cover::Custom, covers), &path).unwrap();
        let r = VnmfReader::open(&path).unwrap();
        assert_eq!(r.previews().iter().map(|p| p.volume_id.get()).collect::<Vec<_>>(), vec![0, 0, 2, 9]);
        let (_, _, opts) = load_vnmf(&path).unwrap();
        assert!(!opts.page_previews);
        let _ = std::fs::remove_file(&path);
    }
}
