// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! VNMF Reader — a manga and webtoon reader with short file information (like `vnmfinfo --quick`).
//!
//! - **Pages** (manga): a page or a spread on screen, ←/→, clicking the edges, right to left;
//!   a floating navigation bar that hides while the mouse is still.
//! - **Strip** (webtoon): continuous vertical scrolling of all fragments without a bottom bar;
//!   images are decoded only near the visible area, placeholders with the right proportions keep
//!   the strip from "jumping".
//! - **Zoom**: Ctrl+scroll, touchpad pinch, Ctrl+= / Ctrl+− / Ctrl+0 — in both modes;
//!   the point under the cursor (or the center) stays in place.
//! - **Page attachments**: music and sound effects (M — mute), the "Page
//!   text" panel (T), dark versions of pages in a dark theme (see `media`).

mod layout;
mod prefs;
mod settings;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use vnmf::contents::Toc;
use vnmf::health::{Health, assess, role_name};
use vnmf::metadata::Metadata;
use vnmf::playback;
use vnmf_core::{AuxFlags, AuxKind, HeaderBitEnum, PageBitFlags, PageFormat, PageRole, VnmfReader};
use vnmf_gui::aspect_picture::AspectPicture;
use vnmf_gui::audio::{self, Sound};
use vnmf_gui::imaging::{self, TextureCache};
use vnmf_gui::{AppWindow, health_ui, widgets};

use layout::PageInfo;
use settings::{Fit, Position, STRIP_WIDTH_MAX, STRIP_WIDTH_MIN, Settings, ZOOM_MAX, ZOOM_MIN};
use vnmf_i18n::{n_, tr, trf};

const APP_ID: &str = "org.vnmf.Reader";
/// Decoded fragments kept in memory: at most this many…
const CACHE_SIZE: usize = 256;
/// …and at most this much (RGBA). Preloading takes `Settings::preload` pages on each side of the
/// current one, but no more than the budget minus [`SCREEN_RESERVE`] — so the preloaded pages
/// don't push each other (and the one on screen) out of the cache.
const CACHE_BYTES: usize = 768 << 20;
/// The part of [`CACHE_BYTES`] left for what's on screen (a spread, the visible strip rows).
const SCREEN_RESERVE: usize = 192 << 20;
/// After how much inactivity the floating bar hides (and the header in fullscreen).
const AUTOHIDE: Duration = Duration::from_millis(2500);
/// How long the "Volume · Chapter" card is shown at the start of a chapter.
const CARD_TIME: Duration = Duration::from_millis(3000);
/// Height of a chapter header in the strip, px (exact: position calculations depend on it).
const STRIP_HEADER: i32 = 112;

fn main() -> glib::ExitCode {
    vnmf_gui::run_app(APP_ID, ReaderWindow::new)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Paged,
    Strip,
}

/// Strip layout: the top of each row (and the total height as the last element) and the height of the image in it.
struct StripGeometry {
    tops: Vec<f64>,
    heights: Vec<f64>,
}

/// A single image fragment (for regular manga — one page).
#[derive(Clone, Copy)]
struct ChunkMeta {
    page: usize,
    chunk: usize,
    width: u32,
    height: u32,
}

struct State {
    reader: Option<Rc<VnmfReader>>,
    path: Option<PathBuf>,
    uuid: String,
    chunks: Vec<ChunkMeta>,
    /// Page groups for display (a single page or a spread), in reading order.
    groups: Vec<Vec<usize>>,
    current: usize,
    mode: Mode,
    rtl: bool,
    bad_pages: BTreeSet<usize>,
    /// Volumes and chapters of the file with titles.
    toc: Toc,
    /// Aspect ratio of an ordinary page — the zoom unit (see `layout::page_unit`).
    page_unit: Option<f64>,
    /// Which attachments the file has.
    has_audio: bool,
    has_text: bool,
    has_dark: bool,
}

/// Page sound that is playing now.
#[derive(Default)]
struct MediaState {
    /// Music: (index in the aux table, sound) and the one waiting for its delay.
    music: Option<(usize, Sound)>,
    music_pending: Option<usize>,
    /// Music whose time (`duration_ms`) has run out: it doesn't start again on its pages
    /// until the reader goes somewhere with different music or silence.
    music_expired: Option<usize>,
    /// Effects that are playing (by index in the aux table).
    sfx: HashMap<usize, Sound>,
    /// Generation of deferred starts: a change cancels the ones still waiting.
    music_gen: u64,
    sfx_gen: u64,
    /// Strip: effects whose trigger line is currently on screen.
    inside: HashSet<usize>,
    /// For which group (page) the sound and text have already been updated.
    shown: Option<(bool, usize)>,
}

#[derive(Clone)]
struct ReaderWindow(Rc<Inner>);

struct Inner {
    window: adw::ApplicationWindow,
    toolbar: adw::ToolbarView,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    split: adw::OverlaySplitView,
    sidebar: adw::Bin,
    reading_area: gtk::Overlay,
    views: gtk::Stack,
    paged_scroller: gtk::ScrolledWindow,
    strip_scroller: gtk::ScrolledWindow,
    /// Strip: a plain column of rows (not a ListView — it doesn't rebind rows on a zoom
    /// change, because the model objects are the same). The exact layout makes it possible to hold the position precisely.
    strip_box: gtk::Box,
    strip_rows: RefCell<Vec<(adw::Clamp, AspectPicture)>>,
    // Floating bar.
    bar: gtk::Revealer,
    scale: gtk::Scale,
    position: gtk::Label,
    /// Chapter navigation buttons on the bar (only for files with several chapters).
    chapter_buttons: [gtk::Button; 2],
    zoom_button: gtk::MenuButton,
    /// The "Volume · Chapter" card that appears at the start of a chapter.
    card: gtk::Revealer,
    card_volume: gtk::Label,
    card_chapter: gtk::Label,
    card_timer: RefCell<Option<glib::SourceId>>,
    /// Height of the chapter header above each strip row (0 — none).
    strip_headers: RefCell<Vec<f64>>,
    state: RefCell<State>,
    settings: RefCell<Settings>,
    cache: RefCell<TextureCache>,
    /// Around which visible rows (with which `preload` and `generation`) the strip was last
    /// preloaded: scrolling within the same rows doesn't repeat the work.
    strip_prefetched: Cell<Option<(usize, usize, u32, u64)>>,
    /// Size of the reading area (for explicitly computing page sizes).
    viewport: Cell<(i32, i32)>,
    hide_timer: RefCell<Option<glib::SourceId>>,
    pointer_in_bar: Cell<bool>,
    render_pending: Cell<bool>,
    save_pending: Cell<bool>,
    strip_update_pending: Cell<bool>,
    /// Last pointer position over the reading area (the point to zoom around).
    pointer: Cell<Option<(f64, f64)>>,
    /// Scroll position to restore after the layout is recomputed:
    /// (fraction of the content, offset of the point from the edge of the visible area).
    pending_h: Cell<Option<(f64, f64)>>,
    pending_v: Cell<Option<(f64, f64)>>,
    /// Size of the paged mode content computed during display: the position is restored only
    /// once the scroller already knows the new size (and not an intermediate one — while the content is being replaced).
    expected_size: Cell<(f64, f64)>,
    /// Strip: (row, fraction within the row, offset from the top of the visible area).
    pending_strip: Cell<Option<(usize, f64, f64)>>,
    /// Which group (page/spread) is shown: scrolling is reset only when moving to another one.
    rendered_group: Cell<Option<usize>>,
    /// Images of the shown group: (widget, aspect ratio). Zoom only changes their sizes —
    /// without rebuilding widgets, so an "intermediate" state never appears in a frame.
    /// `true` — a column (webtoon page), aspect = height/width; `false` — a row, width/height.
    paged_items: RefCell<(bool, Vec<(AspectPicture, f64)>)>,
    /// Actual zoom of the shown group relative to the window width (for the "NN%" label).
    effective_zoom: Cell<f64>,
    /// Chunks currently being decoded, and widgets waiting for the result. All of them wait:
    /// the same chunk may be requested by a new widget while it's being decoded for an old one
    /// (the page is rebuilt during opening), and it must get the image too.
    loading: RefCell<HashMap<usize, Vec<glib::WeakRef<gtk::Picture>>>>,
    /// Pages the decoder couldn't display (reported once).
    undecodable: RefCell<HashSet<usize>>,
    /// Don't react to a slider change that we made ourselves.
    syncing: Cell<bool>,
    generation: Cell<u64>,
    media: RefCell<MediaState>,
    sound_button: gtk::ToggleButton,
    text_button: gtk::ToggleButton,
    /// The "Page text" panel to the right of the page.
    text_panel: gtk::Revealer,
    text_title: gtk::Label,
    text_label: gtk::Label,
}

impl ReaderWindow {
    fn new(app: &adw::Application) -> Self {
        let title = adw::WindowTitle::new("VNMF Reader", "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));

        let open_button = gtk::Button::builder().icon_name("document-open-symbolic").tooltip_text(tr("Open (Ctrl+O)")).build();
        open_button.set_action_name(Some("win.open"));
        let sidebar_button = gtk::ToggleButton::builder().icon_name("sidebar-show-symbolic").tooltip_text(tr("Information and pages (F9)")).build();
        header.pack_start(&open_button);
        header.pack_start(&sidebar_button);
        header.pack_end(&reader_menu());
        let zoom_button = zoom_menu_button();
        header.pack_end(&zoom_button);
        // Visible only for files with page sound / text.
        let text_button =
            gtk::ToggleButton::builder().icon_name("text-x-generic-symbolic").tooltip_text(tr("Page text (T)")).action_name("win.text").visible(false).build();
        let sound_button = gtk::ToggleButton::builder()
            .icon_name("audio-volume-high-symbolic")
            .tooltip_text(tr("Page sound (M)"))
            .action_name("win.sound")
            .visible(false)
            .build();
        header.pack_end(&text_button);
        header.pack_end(&sound_button);

        let banner = health_ui::banner();

        let (empty, empty_button) = widgets::open_status_page("x-office-document-symbolic", "VNMF Reader", tr("Open a .vnmf file or drag it into the window"));
        empty_button.set_action_name(Some("win.open"));

        // Paged mode: page sizes are computed explicitly, so scrolling works in both directions.
        let paged_scroller = gtk::ScrolledWindow::new();
        paged_scroller.add_css_class("reader-surface");

        // Strip.
        let strip_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        strip_box.add_css_class("reader-surface");
        let strip_scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&strip_box).build();
        strip_scroller.add_css_class("reader-surface");

        let views = gtk::Stack::new();
        views.add_named(&paged_scroller, Some("paged"));
        views.add_named(&strip_scroller, Some("strip"));

        // Floating navigation bar (only in paged mode).
        let prev = gtk::Button::builder().icon_name("go-previous-symbolic").tooltip_text(tr("Left page (←)")).build();
        prev.set_action_name(Some("win.left"));
        prev.add_css_class("flat");
        let next = gtk::Button::builder().icon_name("go-next-symbolic").tooltip_text(tr("Right page (→)")).build();
        next.set_action_name(Some("win.right"));
        next.add_css_class("flat");
        let chapter_left = gtk::Button::builder().icon_name("media-skip-backward-symbolic").tooltip_text(tr("Previous chapter (Ctrl+←)")).build();
        chapter_left.set_action_name(Some("win.chapter-left"));
        chapter_left.add_css_class("flat");
        let chapter_right = gtk::Button::builder().icon_name("media-skip-forward-symbolic").tooltip_text(tr("Next chapter (Ctrl+→)")).build();
        chapter_right.set_action_name(Some("win.chapter-right"));
        chapter_right.add_css_class("flat");
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 2.0, 1.0);
        scale.set_draw_value(false);
        scale.set_width_request(360);
        let position = gtk::Label::new(None);
        position.add_css_class("numeric");
        position.set_width_chars(9);
        let bar_box = gtk::Box::builder().spacing(4).build();
        bar_box.add_css_class("osd");
        bar_box.add_css_class("floating-bar");
        bar_box.append(&chapter_left);
        bar_box.append(&prev);
        bar_box.append(&scale);
        bar_box.append(&next);
        bar_box.append(&chapter_right);
        bar_box.append(&position);
        let bar = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(250)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(18)
            .child(&bar_box)
            .build();

        // Chapter start card: top center, disappears by itself.
        let card_volume = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        card_volume.add_css_class("caption-heading");
        card_volume.add_css_class("dim-label");
        let card_chapter = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).max_width_chars(40).build();
        card_chapter.add_css_class("title-3");
        let card_box =
            gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).margin_top(10).margin_bottom(10).margin_start(18).margin_end(18).build();
        card_box.append(&card_volume);
        card_box.append(&card_chapter);
        let card_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card_frame.add_css_class("osd");
        card_frame.add_css_class("floating-bar");
        card_frame.append(&card_box);
        let card = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(300)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .margin_top(18)
            .can_target(false)
            .child(&card_frame)
            .build();

        // The "Page text" panel: on the right, on top of the reading area.
        let text_title = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        text_title.add_css_class("heading");
        let text_label = gtk::Label::builder().xalign(0.0).yalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).selectable(true).build();
        let text_scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&text_label).build();
        let text_close = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text(tr("Hide (T)")).action_name("win.text").build();
        text_close.set_action_target_value(None);
        text_close.add_css_class("flat");
        text_close.add_css_class("circular");
        let text_head = gtk::Box::builder().spacing(6).build();
        text_title.set_hexpand(true);
        text_head.append(&text_title);
        text_head.append(&text_close);
        let text_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).width_request(320).build();
        text_box.add_css_class("page-text-panel");
        text_box.append(&text_head);
        text_box.append(&text_scroll);
        let text_panel = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Fill)
            .margin_top(12)
            .margin_bottom(12)
            .margin_end(12)
            .child(&text_box)
            .build();

        // An invisible widget over the whole reading area — only to know its size.
        let size_probe = gtk::DrawingArea::builder().can_target(false).can_focus(false).build();

        let reading_area = gtk::Overlay::new();
        reading_area.set_child(Some(&views));
        reading_area.add_overlay(&size_probe);
        reading_area.set_measure_overlay(&size_probe, false);
        reading_area.add_overlay(&bar);
        reading_area.add_overlay(&card);
        reading_area.add_overlay(&text_panel);

        let sidebar = adw::Bin::new();
        let split = adw::OverlaySplitView::builder()
            .sidebar(&sidebar)
            .content(&reading_area)
            .show_sidebar(false)
            .min_sidebar_width(280.0)
            .max_sidebar_width(380.0)
            .build();
        sidebar_button.bind_property("active", &split, "show-sidebar").bidirectional().sync_create().build();

        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&split, Some("content"));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&toasts));
        // The header on top of the page when it's shown in fullscreen.
        toolbar.set_top_bar_style(adw::ToolbarStyle::Flat);

        let window = adw::ApplicationWindow::builder().application(app).title("VNMF Reader").default_width(1100).default_height(900).content(&toolbar).build();

        let this = Self(Rc::new(Inner {
            window,
            toolbar,
            title,
            stack,
            banner,
            toasts,
            split,
            sidebar,
            reading_area,
            views,
            paged_scroller,
            strip_scroller,
            strip_box,
            strip_rows: RefCell::new(Vec::new()),
            bar,
            scale,
            position,
            chapter_buttons: [chapter_left, chapter_right],
            zoom_button,
            card,
            card_volume,
            card_chapter,
            card_timer: RefCell::new(None),
            strip_headers: RefCell::new(Vec::new()),
            state: RefCell::new(State {
                reader: None,
                path: None,
                uuid: String::new(),
                chunks: Vec::new(),
                groups: Vec::new(),
                current: 0,
                mode: Mode::Paged,
                rtl: false,
                bad_pages: BTreeSet::new(),
                toc: Toc::default(),
                page_unit: None,
                has_audio: false,
                has_text: false,
                has_dark: false,
            }),
            settings: RefCell::new(Settings::load()),
            cache: RefCell::new(TextureCache::with_budget(CACHE_SIZE, CACHE_BYTES)),
            strip_prefetched: Cell::new(None),
            viewport: Cell::new((0, 0)),
            hide_timer: RefCell::new(None),
            pointer_in_bar: Cell::new(false),
            render_pending: Cell::new(false),
            save_pending: Cell::new(false),
            strip_update_pending: Cell::new(false),
            pointer: Cell::new(None),
            pending_h: Cell::new(None),
            pending_v: Cell::new(None),
            expected_size: Cell::new((0.0, 0.0)),
            pending_strip: Cell::new(None),
            rendered_group: Cell::new(None),
            paged_items: RefCell::new((false, Vec::new())),
            effective_zoom: Cell::new(1.0),
            loading: RefCell::new(HashMap::new()),
            undecodable: RefCell::new(HashSet::new()),
            syncing: Cell::new(false),
            generation: Cell::new(0),
            media: RefCell::new(MediaState::default()),
            sound_button,
            text_button,
            text_panel,
            text_title,
            text_label,
        }));

        let me = this.clone();
        size_probe.connect_resize(move |_, w, h| {
            let (old_w, old_h) = me.0.viewport.replace((w, h));
            if (old_w - w).abs() > 1 || (old_h - h).abs() > 1 {
                me.schedule_layout();
            }
        });
        let me = this.clone();
        bar_box.add_controller({
            let motion = gtk::EventControllerMotion::new();
            let leave = me.clone();
            motion.connect_enter(move |_, _, _| me.0.pointer_in_bar.set(true));
            motion.connect_leave(move |_| {
                leave.0.pointer_in_bar.set(false);
                leave.poke_ui();
            });
            motion
        });

        this.install_actions(app);
        this.install_input();
        this.apply_background();
        widgets::install_about(&this.0.window, "VNMF Reader", tr("A manga and webtoon reader for the VNMF format."));

        let me = this.clone();
        this.0.window.connect_fullscreened_notify(move |w| {
            let hide = w.is_fullscreen() && me.0.settings.borrow().hide_header_fullscreen;
            me.0.toolbar.set_reveal_top_bars(!hide);
            me.poke_ui();
        });
        // The window — at the size and state (maximized or not) it was closed with.
        {
            let s = this.0.settings.borrow();
            this.0.window.set_default_size(s.window_width, s.window_height);
            if s.maximized {
                this.0.window.maximize();
            }
        }
        let me = this.clone();
        this.0.window.connect_close_request(move |w| {
            {
                let mut s = me.0.settings.borrow_mut();
                // Size without maximizing and fullscreen: GTK updates it when the window is resized manually.
                let (width, height) = w.default_size();
                if width > 0 && height > 0 {
                    (s.window_width, s.window_height) = (width, height);
                }
                s.maximized = w.is_maximized();
            }
            me.flush_settings();
            me.stop_media();
            audio::cleanup();
            glib::Propagation::Proceed
        });
        // The dark theme changed — so do the dark page versions.
        let me = this.clone();
        adw::StyleManager::default().connect_dark_notify(move |_| me.dark_changed());
        this
    }

    // ------------------------------------------------------------------
    // Actions and input
    // ------------------------------------------------------------------

    fn action(&self, name: &str, f: impl Fn(&Self) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let this = self.clone();
        action.connect_activate(move |_, _| f(&this));
        self.0.window.add_action(&action);
    }

    fn stateful(&self, name: &str, initial: glib::Variant, f: impl Fn(&Self, &glib::Variant) + 'static) {
        // Boolean actions are flags without a parameter; string ones are radio buttons with a parameter.
        let param = (initial.type_() != glib::VariantTy::BOOLEAN).then(|| initial.type_());
        let action = gio::SimpleAction::new_stateful(name, param, &initial);
        let this = self.clone();
        action.connect_change_state(move |action, value| {
            if let Some(value) = value {
                action.set_state(value);
                f(&this, value);
            }
        });
        self.0.window.add_action(&action);
    }

    fn install_actions(&self, app: &adw::Application) {
        self.action("open", |this| {
            let this = this.clone();
            glib::spawn_future_local(async move {
                if let Some(path) = widgets::choose_vnmf(&this.0.window).await {
                    this.open(path);
                }
            });
        });
        self.action("left", |this| this.step_visual(-1));
        self.action("right", |this| this.step_visual(1));
        // Chapters: left/right on screen (taking direction into account) and in reading order.
        self.action("chapter-left", |this| this.chapter_step_visual(-1));
        self.action("chapter-right", |this| this.chapter_step_visual(1));
        self.action("prev-chapter", |this| this.chapter_step(-1));
        self.action("next-chapter", |this| this.chapter_step(1));
        self.action("sidebar", |this| this.0.split.set_show_sidebar(!this.0.split.shows_sidebar()));
        self.action("fullscreen", |this| this.0.window.set_fullscreened(!this.0.window.is_fullscreen()));
        self.action("zoom-in", |this| this.zoom_by(1.15));
        self.action("zoom-out", |this| this.zoom_by(1.0 / 1.15));
        self.action("zoom-reset", |this| this.zoom_reset());
        self.action("preferences", prefs::show);
        self.action("info", |this| {
            if let Some(path) = this.0.state.borrow().path.clone()
                && let Err(e) = vnmf_gui::launch_sibling("vnmf-info", &path)
            {
                widgets::toast(&this.0.toasts, &trf!("Could not launch VNMF Info: {0}", e));
            }
        });

        let s = self.0.settings.borrow().clone();
        self.stateful("mode", "paged".to_variant(), |this, v| {
            this.set_mode(if v.str() == Some("strip") { Mode::Strip } else { Mode::Paged });
        });
        let fit_name = match s.fit {
            Fit::Page => "page",
            Fit::Width => "width",
            Fit::Custom => "custom",
        };
        self.stateful("fit", fit_name.to_variant(), |this, v| {
            let fit = match v.str() {
                Some("width") => Fit::Width,
                Some("custom") => Fit::Custom,
                _ => Fit::Page,
            };
            if this.0.state.borrow().mode == Mode::Paged {
                this.capture_anchor();
            }
            this.0.settings.borrow_mut().fit = fit;
            this.layout_paged();
            this.schedule_save();
        });
        self.stateful("rtl", false.to_variant(), |this, v| this.set_rtl(v.get::<bool>().unwrap_or(false)));
        self.stateful("sound", s.sound.to_variant(), |this, v| {
            this.0.settings.borrow_mut().sound = v.get::<bool>().unwrap_or(true);
            this.sound_changed();
            this.schedule_save();
        });
        self.stateful("text", s.show_text.to_variant(), |this, v| {
            this.0.settings.borrow_mut().show_text = v.get::<bool>().unwrap_or(false);
            this.update_text_panel();
            this.schedule_save();
        });
        self.stateful("dark-pages", s.dark_pages.to_variant(), |this, v| {
            this.0.settings.borrow_mut().dark_pages = v.get::<bool>().unwrap_or(true);
            this.dark_changed();
            this.schedule_save();
        });
        self.stateful("double", s.double_page.to_variant(), |this, v| {
            let on = v.get::<bool>().unwrap_or(false);
            this.0.settings.borrow_mut().double_page = on;
            this.settings_changed();
            if on {
                this.explain_single_page();
            }
        });

        // Page slider.
        let this = self.clone();
        self.0.scale.connect_value_changed(move |scale| {
            if !this.0.syncing.get() {
                this.go_to(scale.value().round() as usize - 1);
            }
        });

        // Opening by drag and drop.
        let this = self.clone();
        self.0.window.add_controller(widgets::file_drop_target(move |paths| {
            if let Some(path) = paths.into_iter().next() {
                this.open(path);
            }
        }));

        app.set_accels_for_action("win.fullscreen", &["F11"]);
        app.set_accels_for_action("win.sidebar", &["F9"]);
        app.set_accels_for_action("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]);
        app.set_accels_for_action("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]);
        app.set_accels_for_action("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]);
        app.set_accels_for_action("win.preferences", &["<Ctrl>comma"]);
        app.set_accels_for_action("win.info", &["<Ctrl>i"]);
        app.set_accels_for_action("win.chapter-left", &["<Ctrl>Left"]);
        app.set_accels_for_action("win.chapter-right", &["<Ctrl>Right"]);
        app.set_accels_for_action("win.prev-chapter", &["<Ctrl>Page_Up"]);
        app.set_accels_for_action("win.next-chapter", &["<Ctrl>Page_Down"]);
    }

    fn install_input(&self) {
        // Keyboard: ←/→ taking direction into account, PgUp/PgDn/space — "forward/back" in reading order.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = self.clone();
        keys.connect_key_pressed(move |_, key, keycode, modifiers| {
            if this.0.state.borrow().reader.is_none() || modifiers.contains(gdk::ModifierType::CONTROL_MASK) || this.focus_in_dialog_or_entry() {
                return glib::Propagation::Proceed;
            }
            this.poke_ui();
            let paged = this.0.state.borrow().mode == Mode::Paged;
            match key {
                gdk::Key::Left if paged => this.step_visual(-1),
                gdk::Key::Right if paged => this.step_visual(1),
                gdk::Key::Page_Down | gdk::Key::space if paged => this.step(1),
                gdk::Key::Page_Up | gdk::Key::BackSpace if paged => this.step(-1),
                gdk::Key::Home => this.go_to(0),
                gdk::Key::End => this.go_to(this.pages_count().saturating_sub(1)),
                gdk::Key::Escape if this.0.window.is_fullscreen() => this.0.window.unfullscreen(),
                // T — page text, M — sound (by the physical key, like D).
                _ if is_key(key, keycode, gdk::Key::t) && this.0.state.borrow().has_text => {
                    let on = !this.0.settings.borrow().show_text;
                    if let Some(action) = this.0.window.lookup_action("text") {
                        action.change_state(&on.to_variant());
                    }
                }
                _ if is_key(key, keycode, gdk::Key::m) && this.0.state.borrow().has_audio => {
                    let on = !this.0.settings.borrow().sound;
                    if let Some(action) = this.0.window.lookup_action("sound") {
                        action.change_state(&on.to_variant());
                    }
                }
                // D — spread; by the physical key, so it also works in other layouts (e.g. "в" in Ukrainian).
                _ if is_key(key, keycode, gdk::Key::d) => {
                    let double = !this.0.settings.borrow().double_page;
                    if let Some(action) = this.0.window.lookup_action("double") {
                        action.change_state(&double.to_variant());
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.0.window.add_controller(keys);

        // Page turning by click: the left half of the reading area — left, the right half — right.
        // FIX: the gesture used to hang on the scroller and had a "dead" middle third, and quick clicks
        // in a row (n = 2, 3 — a double click) were discarded — so page turning sometimes "didn't work".
        // Now it's an invisible layer over the whole reading area: the Capture phase sees the click before
        // any widget under it (the page, the scroller), but doesn't claim the event — scrolling
        // and dragging work as before. The floating bar buttons and scrollbars are not affected.
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = self.clone();
        click.connect_released(move |gesture, _, x, y| {
            let paged = this.0.state.borrow().mode == Mode::Paged && this.0.state.borrow().reader.is_some();
            if !paged || !this.0.settings.borrow().click_zones {
                return;
            }
            let Some(area) = gesture.widget() else { return };
            // Bar buttons and scrollbars have their own actions.
            let target = area.pick(x, y, gtk::PickFlags::DEFAULT);
            let on_control = target.is_some_and(|w| {
                w == this.0.bar
                    || w.is_ancestor(&this.0.bar)
                    || w.is_ancestor(&this.0.text_panel)
                    || w.is::<gtk::Scrollbar>()
                    || w.ancestor(gtk::Scrollbar::static_type()).is_some()
            });
            if on_control {
                return;
            }
            this.step_visual(if x < f64::from(area.width()) / 2.0 { -1 } else { 1 });
        });
        self.0.reading_area.add_controller(click);

        // Ctrl+scroll — zoom. Capture phase: otherwise the ScrolledWindow takes the scroll
        // before the handler sees the event (that's exactly why zoom used to not work).
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = self.clone();
        wheel.connect_scroll(move |controller, _, dy| {
            if !controller.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) || dy == 0.0 {
                return glib::Propagation::Proceed;
            }
            // Mouse wheel — dy = ±1 (step ×1.1); touchpad — small dy, so zooming is smooth.
            this.zoom_by(1.1f64.powf(-dy.clamp(-5.0, 5.0)));
            glib::Propagation::Stop
        });
        self.0.reading_area.add_controller(wheel);

        // Pinch on a touchpad / touchscreen.
        let pinch = gtk::GestureZoom::new();
        let start = Rc::new(Cell::new(1.0));
        let (this, start_c) = (self.clone(), start.clone());
        pinch.connect_begin(move |_, _| start_c.set(this.current_zoom()));
        let this = self.clone();
        pinch.connect_scale_changed(move |gesture, scale| {
            if let Some(center) = gesture.bounding_box_center() {
                this.0.pointer.set(Some(center));
            }
            this.set_zoom(start.get() * scale);
        });
        self.0.reading_area.add_controller(pinch);

        // Mouse movement shows the bar (and the header near the top edge in fullscreen).
        let motion = gtk::EventControllerMotion::new();
        let this = self.clone();
        motion.connect_motion(move |_, x, y| {
            this.0.pointer.set(Some((x, y)));
            if this.0.window.is_fullscreen() && this.0.settings.borrow().hide_header_fullscreen && y < 48.0 {
                this.0.toolbar.set_reveal_top_bars(true);
            }
            this.poke_ui();
        });
        let this = self.clone();
        motion.connect_leave(move |_| this.0.pointer.set(None));
        self.0.reading_area.add_controller(motion);

        // After the layout is recomputed (a new page, window size) — restore the scroll position.
        // `changed` arrives in the middle of layout, where an offset won't apply in this frame —
        // so right after it. (Zoom sets the position in advance — see `settle_anchor`.)
        let this = self.clone();
        self.0.paged_scroller.hadjustment().connect_changed(move |adj| {
            if this.0.pending_h.get().is_some() {
                let (this, adj) = (this.clone(), adj.clone());
                glib::idle_add_local_once(move || apply_pending(&adj, &this.0.pending_h, this.0.expected_size.get().0, false));
            }
        });
        let this = self.clone();
        self.0.paged_scroller.vadjustment().connect_changed(move |adj| {
            if this.0.pending_v.get().is_some() {
                let (this, adj) = (this.clone(), adj.clone());
                glib::idle_add_local_once(move || apply_pending(&adj, &this.0.pending_v, this.0.expected_size.get().1, false));
            }
        });

        // Strip: the current page, lazy loading, restoring the position after zoom.
        let this = self.clone();
        self.0.strip_scroller.vadjustment().connect_value_changed(move |adj| {
            this.track_strip_position(adj);
            this.schedule_strip_update();
            this.update_strip_sfx();
        });
        // A point that couldn't be set right away (the strip isn't shown yet — opening a file,
        // switching modes): `changed` arrives in the middle of layout, where an offset won't apply anymore,
        // so — right after it.
        let this = self.clone();
        self.0.strip_scroller.vadjustment().connect_changed(move |adj| {
            if this.0.pending_strip.get().is_some() {
                let (this, adj) = (this.clone(), adj.clone());
                glib::idle_add_local_once(move || this.apply_strip_pending(&adj));
            }
            this.schedule_strip_update();
        });
    }

    /// Focus is in a dialog (settings, etc.) or a text field — reading keys are not intercepted,
    /// otherwise the arrows and space would turn pages instead of operating the dialog controls.
    fn focus_in_dialog_or_entry(&self) -> bool {
        let Some(focus) = gtk::prelude::GtkWindowExt::focus(&self.0.window) else { return false };
        focus.is::<gtk::Text>()
            || focus.is::<gtk::Editable>()
            || focus.ancestor(adw::Dialog::static_type()).is_some()
            || focus.ancestor(gtk::Popover::static_type()).is_some()
    }

    /// Show the floating bar and restart the auto-hide timer.
    fn poke_ui(&self) {
        let paged = self.0.state.borrow().mode == Mode::Paged && self.0.state.borrow().reader.is_some();
        self.0.bar.set_reveal_child(paged);
        if let Some(id) = self.0.hide_timer.take() {
            id.remove();
        }
        let this = self.clone();
        let id = glib::timeout_add_local_once(AUTOHIDE, move || {
            this.0.hide_timer.take();
            let s = this.0.settings.borrow();
            if s.autohide_bar && !this.0.pointer_in_bar.get() {
                this.0.bar.set_reveal_child(false);
            }
            if this.0.window.is_fullscreen() && s.hide_header_fullscreen && !this.0.pointer_in_bar.get() {
                this.0.toolbar.set_reveal_top_bars(false);
            }
        });
        *self.0.hide_timer.borrow_mut() = Some(id);
    }

    // ------------------------------------------------------------------
    // Settings
    // ------------------------------------------------------------------

    /// Apply changed settings and save them.
    fn settings_changed(&self) {
        self.apply_background();
        self.rebuild_groups();
        self.apply_strip_layout();
        self.schedule_build();
        self.update_zoom_label();
        self.schedule_save();
    }

    fn schedule_save(&self) {
        if self.0.save_pending.replace(true) {
            return;
        }
        let this = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(800), move || this.flush_settings());
    }

    fn flush_settings(&self) {
        self.0.save_pending.set(false);
        self.0.settings.borrow().save();
    }

    /// Restart in a new process on the same file (e.g. after changing the interface language):
    /// the position is saved by closing the window, the new process opens the file where it stopped.
    fn restart(&self) {
        let path = self.0.state.borrow().path.clone();
        self.remember_position();
        vnmf_gui::restart_app(&self.0.window, path);
    }

    fn apply_background(&self) {
        let bg = self.0.settings.borrow().background;
        for class in ["reader-bg-black", "reader-bg-gray", "reader-bg-white"] {
            self.0.views.remove_css_class(class);
        }
        if let Some(class) = bg.css_class() {
            self.0.views.add_css_class(class);
        }
    }

    // ------------------------------------------------------------------
    // Zoom
    // ------------------------------------------------------------------

    /// Width of the strip area (a strip row = min(strip width, this width)).
    fn strip_area_width(&self) -> f64 {
        let w = self.0.strip_box.width().max(self.0.strip_scroller.width());
        f64::from(if w > 0 { w } else { self.0.viewport.get().0.max(1) })
    }

    /// Zoom relative to the window width: 100% — the page (or the strip) at full width.
    fn current_zoom(&self) -> f64 {
        match self.0.state.borrow().mode {
            Mode::Paged => self.0.effective_zoom.get(),
            Mode::Strip => (f64::from(self.0.settings.borrow().strip_width) / self.strip_area_width()).min(1.0),
        }
    }

    /// Remember the point under the cursor (or the center) so it stays in place after the zoom changes.
    /// The content fraction under the point doesn't depend on zoom: if the previous one hasn't been applied yet
    /// (a quick series of Ctrl+= or a pinch), it's the same — a new one isn't taken from a stale scroll.
    fn capture_anchor(&self) {
        let (vw, vh) = self.0.viewport.get();
        let (ax, ay) = self.0.pointer.get().unwrap_or((f64::from(vw) / 2.0, f64::from(vh) / 2.0));
        match self.0.state.borrow().mode {
            Mode::Paged => {
                let s = &self.0.paged_scroller;
                if self.0.pending_h.get().is_none() {
                    self.0.pending_h.set(anchor(&s.hadjustment(), ax));
                }
                if self.0.pending_v.get().is_none() {
                    self.0.pending_v.set(anchor(&s.vadjustment(), ay));
                }
            }
            Mode::Strip => {
                if self.0.pending_strip.get().is_none() {
                    self.0.pending_strip.set(self.strip_anchor(ay));
                }
            }
        }
    }

    fn set_zoom(&self, zoom: f64) {
        self.capture_anchor();
        match self.0.state.borrow().mode {
            Mode::Paged => {
                {
                    let mut s = self.0.settings.borrow_mut();
                    s.fit = Fit::Custom;
                    s.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
                }
                self.set_action_state("fit", "custom".to_variant());
            }
            Mode::Strip => {
                // The strip is never wider than the window (there's no horizontal scrolling).
                let width = (self.strip_area_width() * zoom.min(1.0)).round() as i32;
                self.0.settings.borrow_mut().strip_width = width.clamp(STRIP_WIDTH_MIN, STRIP_WIDTH_MAX);
            }
        }
        // Synchronously, without rebuilding widgets: new sizes and position — in a single frame.
        if self.0.state.borrow().mode == Mode::Strip {
            self.apply_strip_layout();
        } else {
            self.layout_paged();
        }
        self.settle_anchor();
        self.update_zoom_label();
        self.schedule_save();
    }

    /// If the zoom changed nothing (already 100% or the width limit), the scroller won't send `changed`,
    /// and the remembered point would stay hanging: the next zoom step would take it instead of
    /// a new one and throw the strip back, and while it hangs, the reading position isn't updated.
    /// Here it's applied right away if the size is already known; otherwise it waits for `changed`, as before.
    fn settle_anchor(&self) {
        match self.0.state.borrow().mode {
            Mode::Strip => self.apply_strip_pending(&self.0.strip_scroller.vadjustment()),
            Mode::Paged => {
                let s = &self.0.paged_scroller;
                let (w, h) = self.0.expected_size.get();
                apply_pending(&s.hadjustment(), &self.0.pending_h, w, true);
                apply_pending(&s.vadjustment(), &self.0.pending_v, h, true);
            }
        }
    }

    fn zoom_by(&self, factor: f64) {
        self.set_zoom(self.current_zoom() * factor);
    }

    /// 100% — full width (in paged mode that's "Fit width").
    fn zoom_reset(&self) {
        if self.0.state.borrow().mode == Mode::Paged {
            self.capture_anchor();
            self.0.settings.borrow_mut().fit = Fit::Width;
            self.set_action_state("fit", "width".to_variant());
            self.layout_paged();
            self.settle_anchor();
            self.schedule_save();
        } else {
            self.set_zoom(1.0);
        }
    }

    fn update_zoom_label(&self) {
        let percent = (self.current_zoom() * 100.0).round();
        self.0.zoom_button.set_label(&format!("{percent}%"));
    }

    /// A spread is enabled, but the current page is shown alone — explain why, so it doesn't
    /// seem that the spread "doesn't work".
    fn explain_single_page(&self) {
        let (mode, current) = {
            let s = self.0.state.borrow();
            (s.mode, s.current)
        };
        if self.0.state.borrow().reader.is_none() {
            return;
        }
        let message = if mode == Mode::Strip {
            Some(tr("Spreads work in \"Pages\" mode").to_string())
        } else {
            let groups = self.0.state.borrow().groups.clone();
            let single = groups.get(self.group_index(current)).is_some_and(|g| g.len() == 1);
            match () {
                _ if !single => None,
                _ if current == 0 && self.0.settings.borrow().cover_alone => {
                    Some(tr("Spread enabled: the cover is shown alone, spreads start from the next page").into())
                }
                _ if self.page_chunks(current).len() > 1 => Some(tr("This page is a webtoon strip, it's shown alone").into()),
                _ if self.page_role(current).is_some_and(|r| matches!(r, PageRole::FrontCover | PageRole::BackCover)) => {
                    Some(tr("The file marked this page as a cover — it's shown alone").into())
                }
                _ if self.page_role(current) == Some(PageRole::SpreadSingle) => {
                    Some(tr("The file marked this page as a whole spread — it's shown alone").into())
                }
                _ => Some(tr("This page is wide (a spread by itself) — it's shown alone").into()),
            }
        };
        if let Some(message) = message {
            widgets::toast(&self.0.toasts, &message);
        }
    }

    // ------------------------------------------------------------------
    // Opening a file
    // ------------------------------------------------------------------

    fn pages_count(&self) -> usize {
        self.0.state.borrow().reader.as_ref().map_or(0, |r| r.pages_count())
    }

    fn set_action_state(&self, name: &str, value: glib::Variant) {
        if let Some(action) = self.0.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
            action.set_state(&value);
        }
    }

    /// A damaged page was detected while displaying — add it to the banner.
    fn report_bad_page(&self, page: usize) {
        let (path, has_parity, bad) = {
            let mut state = self.0.state.borrow_mut();
            if !state.bad_pages.insert(page) {
                return;
            }
            let has_parity = state.reader.as_ref().is_some_and(|r| r.parity_layout().is_some());
            (state.path.clone(), has_parity, state.bad_pages.iter().map(|p| (p + 1).to_string()).collect::<Vec<_>>())
        };
        let Some(path) = path else { return };
        let health = Health {
            issues: vec![trf!("damaged page data: {0}", bad.join(", "))],
            prognosis: if has_parity {
                tr("recoverable thanks to redundancy")
            } else {
                tr("no redundancy: damaged pages can't be recovered, the surviving ones can be extracted")
            },
            data_checked: false,
        };
        health_ui::update_banner(&self.0.banner, &health, &path);
        widgets::toast(&self.0.toasts, &trf!("Page {0} is damaged — the data doesn't match the checksum", page + 1));
    }

    fn build_sidebar(&self, reader: &VnmfReader, health: &Health) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();

        let about = adw::PreferencesGroup::builder().title(tr("About the file")).build();
        // Briefly: title, type, authors, translation, genres (in full — in VNMF Info).
        if let Some(meta) = Metadata::from_info(reader.info_bytes()) {
            const SHOWN: [&str; 8] =
                [n_("Title"), n_("Original title"), n_("Type"), n_("Status"), n_("Authors and team"), n_("Translation"), n_("Genres"), n_("Age rating")];
            let mut credits = Vec::new();
            for (label, value) in meta.display_rows() {
                if label == tr("Authors and team") {
                    credits.push(value);
                } else if SHOWN.iter().any(|s| tr(s) == label) {
                    about.add(&widgets::property_row(label, &value));
                }
            }
            if !credits.is_empty() {
                about.add(&widgets::property_row(tr("Authors and team"), &credits.join("\n")));
            }
        }
        let mut formats: Vec<String> = Vec::new();
        for p in reader.pages() {
            let f = PageFormat::from(p.format).to_string();
            if !formats.contains(&f) {
                formats.push(f);
            }
        }
        about.add(&widgets::property_row(tr("Page count"), &format!("{} · {}", reader.pages_count(), formats.join(", "))));
        about.add(&widgets::property_row(tr("Redundancy"), &reader.parity_layout().map_or(tr("none").into(), |l| format!("{}%", l.redundancy))));
        about.add(&widgets::property_row(tr("Integrity"), &if health.is_ok() { tr("metadata intact").to_string() } else { health.issues.join("; ") }));
        let more = adw::ButtonRow::builder().title(tr("More in VNMF Info")).end_icon_name("go-next-symbolic").build();
        more.set_action_name(Some("win.info"));
        about.add(&more);
        page.add(&about);

        let toc = Toc::from_reader(reader);
        if toc.is_structured() {
            // Contents: a group per volume, a chapter — an expander row with a jump button and pages.
            for volume in &toc.volumes {
                let group = adw::PreferencesGroup::new();
                group.set_title(&glib::markup_escape_text(&if toc.has_volumes { volume.label() } else { tr("Contents").into() }));
                for chapter in &volume.chapters {
                    let range = &chapter.pages;
                    let expander = adw::ExpanderRow::builder()
                        .title(glib::markup_escape_text(&chapter.label()))
                        .subtitle(if range.len() == 1 { trf!("p. {0}", range.start + 1) } else { trf!("pp. {0}–{1}", range.start + 1, range.end) })
                        .build();
                    let go = gtk::Button::builder()
                        .icon_name("go-next-symbolic")
                        .tooltip_text(tr("Read from the start of the chapter"))
                        .valign(gtk::Align::Center)
                        .build();
                    go.add_css_class("flat");
                    let (this, start) = (self.clone(), range.start);
                    go.connect_clicked(move |_| this.navigate_from_sidebar(start));
                    expander.add_suffix(&go);
                    // A chapter's pages — only once it's expanded.
                    let (this, range) = (self.clone(), range.clone());
                    widgets::fill_on_expand(&expander, move |expander| {
                        let Some(reader) = this.0.state.borrow().reader.clone() else { return };
                        for i in range.clone() {
                            expander.add_row(&this.sidebar_page_row(i, &reader.pages()[i]));
                        }
                    });
                    group.add(&expander);
                }
                page.add(&group);
            }
            return page;
        }

        let pages = adw::PreferencesGroup::builder().title(tr("Pages")).build();
        for (i, rec) in reader.pages().iter().enumerate() {
            pages.add(&self.sidebar_page_row(i, rec));
        }
        page.add(&pages);
        page
    }

    fn sidebar_page_row(&self, i: usize, rec: &vnmf_core::PageRecord) -> adw::ActionRow {
        let role = role_name(rec);
        let row = adw::ActionRow::builder().title(trf!("Page {0}", i + 1)).activatable(true).build();
        if role != "Normal" {
            row.set_subtitle(&role);
        }
        let this = self.clone();
        row.connect_activated(move |_| this.navigate_from_sidebar(i));
        row
    }

    /// Navigation from the contents: in a narrow window the sidebar closes.
    fn navigate_from_sidebar(&self, page: usize) {
        self.go_to(page);
        if self.0.split.is_collapsed() {
            self.0.split.set_show_sidebar(false);
        }
    }

    // ------------------------------------------------------------------
    // Spreads
    // ------------------------------------------------------------------

    /// Page size — from the first fragment.
    fn page_size(&self, page: usize) -> (u32, u32) {
        self.0.state.borrow().chunks.iter().find(|c| c.page == page).map_or((1, 1), |c| (c.width.max(1), c.height.max(1)))
    }

    /// Splits pages into display groups: a spread is two pages side by side. Shown alone are
    /// the cover (per the setting), wide pages and single-image spread pages.
    fn page_role(&self, page: usize) -> Option<PageRole> {
        let state = self.0.state.borrow();
        let role = state.reader.as_ref()?.pages().get(page)?.role;
        PageRole::try_from(role).ok()
    }

    fn page_infos(&self) -> Vec<PageInfo> {
        let n = self.pages_count();
        let (chapter_starts, volume_starts) = {
            let state = self.0.state.borrow();
            let chapters: HashSet<usize> = state.toc.chapters().map(|c| c.pages.start).filter(|&p| p > 0).collect();
            let volumes: HashSet<usize> = state.toc.volumes.iter().filter(|v| state.toc.has_volumes && v.volume != 0).map(|v| v.pages.start).collect();
            (chapters, volumes)
        };
        (0..n)
            .map(|p| {
                let (width, height) = self.page_size(p);
                PageInfo {
                    width,
                    height,
                    role: self.page_role(p),
                    multi_chunk: self.page_chunks(p).len() > 1,
                    chapter_start: chapter_starts.contains(&p),
                    volume_start: volume_starts.contains(&p),
                }
            })
            .collect()
    }

    /// Display groups: spreads from the file (left/right half roles) are always together;
    /// in spread mode — pairs of pages (see `layout::group_pages`).
    fn rebuild_groups(&self) {
        if self.0.state.borrow().reader.is_none() {
            return;
        }
        let (double, cover_alone) = {
            let s = self.0.settings.borrow();
            (s.double_page, s.cover_alone)
        };
        let infos = self.page_infos();
        let groups = layout::group_pages(&infos, double, cover_alone);
        let mut state = self.0.state.borrow_mut();
        // A different split (spread enabled, etc.) — the same group number now has different contents.
        if state.groups != groups {
            self.0.rendered_group.set(None);
        }
        state.groups = groups;
        state.page_unit = layout::page_unit(&infos);
    }

    fn group_index(&self, page: usize) -> usize {
        self.0.state.borrow().groups.iter().position(|g| g.contains(&page)).unwrap_or(0)
    }

    // ------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------

    /// A step in reading order (+1 — the next page / spread).
    fn step(&self, delta: isize) {
        let (mode, current) = {
            let s = self.0.state.borrow();
            (s.mode, s.current)
        };
        if mode == Mode::Paged {
            let groups = self.0.state.borrow().groups.clone();
            let g = self.group_index(current) as isize + delta;
            if let Some(group) = groups.get(g.clamp(0, groups.len() as isize - 1) as usize) {
                self.go_to(group[0]);
            }
        } else {
            let target = (current as isize + delta).clamp(0, self.pages_count().saturating_sub(1) as isize);
            self.go_to(target as usize);
        }
    }

    /// A "left/right" step on screen: when reading right to left — reversed.
    fn step_visual(&self, direction: isize) {
        let rtl = self.0.state.borrow().rtl;
        self.step(if rtl { -direction } else { direction });
    }

    /// Jump to the start of the neighbouring chapter (in reading order).
    fn chapter_step(&self, delta: isize) {
        let (current, starts) = {
            let state = self.0.state.borrow();
            (state.current, state.toc.chapters().map(|c| c.pages.start).collect::<Vec<_>>())
        };
        if starts.len() < 2 {
            return;
        }
        let index = starts.iter().rposition(|&s| s <= current).unwrap_or(0) as isize;
        // Back from the middle of a chapter — first to its start (like in media players).
        let target = if delta < 0 && starts[index as usize] < current { index } else { index + delta };
        let target = target.clamp(0, starts.len() as isize - 1) as usize;
        self.go_to(starts[target]);
    }

    fn chapter_step_visual(&self, direction: isize) {
        let rtl = self.0.state.borrow().rtl;
        self.chapter_step(if rtl { -direction } else { direction });
    }

    /// The "Volume · Chapter" card for the chapter the page belongs to (paged mode;
    /// in the strip the chapter start is marked by a header between fragments).
    fn show_card(&self, page: usize) {
        let text = {
            let state = self.0.state.borrow();
            let toc = &state.toc;
            if !toc.is_structured() || state.mode != Mode::Paged {
                return;
            }
            toc.locate(page).map(|(v, c)| {
                let volume = if toc.has_volumes { v.label() } else { String::new() };
                (volume, c.label())
            })
        };
        let Some((volume, chapter)) = text else { return };
        self.0.card_volume.set_label(&volume);
        self.0.card_volume.set_visible(!volume.is_empty());
        self.0.card_chapter.set_label(&chapter);
        self.0.card.set_reveal_child(true);
        if let Some(id) = self.0.card_timer.take() {
            id.remove();
        }
        let this = self.clone();
        *self.0.card_timer.borrow_mut() = Some(glib::timeout_add_local_once(CARD_TIME, move || {
            this.0.card_timer.take();
            this.0.card.set_reveal_child(false);
        }));
    }

    /// Chapter buttons on the bar — only for files with structure.
    fn update_chapter_buttons(&self) {
        let structured = self.0.state.borrow().toc.chapters_count() > 1;
        for button in &self.0.chapter_buttons {
            button.set_visible(structured);
        }
    }

    fn go_to(&self, page: usize) {
        let mode = {
            let mut state = self.0.state.borrow_mut();
            let Some(reader) = state.reader.as_ref() else { return };
            let last = reader.pages_count().saturating_sub(1);
            state.current = page.min(last);
            state.mode
        };
        match mode {
            Mode::Paged => {
                self.render_paged();
                self.poke_ui();
            }
            Mode::Strip => self.scroll_strip_to_current(),
        }
        self.sync_position();
    }

    fn sync_position(&self) {
        let (total, current, name) = {
            let state = self.0.state.borrow();
            let Some(reader) = state.reader.as_ref() else { return };
            let name = state.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (reader.pages_count().max(1), state.current, name)
        };
        self.0.syncing.set(true);
        self.0.scale.set_range(1.0, (total as f64).max(1.0001));
        self.0.scale.set_value((current + 1) as f64);
        self.0.syncing.set(false);
        self.0.position.set_label(&format!("{} / {total}", current + 1));
        let position = {
            let state = self.0.state.borrow();
            state.toc.is_structured().then(|| state.toc.position_label(current)).flatten()
        };
        self.0.title.set_subtitle(&match position {
            Some(position) => trf!("{0} · page {1} of {2}", position, current + 1, total),
            None => trf!("{0} — page {1} of {2}", name, current + 1, total),
        });

        self.remember_position();
        self.media_page_changed();
    }

    /// Remember where reading is: the page, and in the strip — the fragment and the point at the top edge of the window
    /// (a long webtoon page is dozens of fragments, the page number isn't enough).
    fn remember_position(&self) {
        let (uuid, mut pos, strip) = {
            let state = self.0.state.borrow();
            (state.uuid.clone(), Position::page(state.current), state.mode == Mode::Strip)
        };
        if uuid.is_empty() || !self.0.settings.borrow().remember_position {
            return;
        }
        // While the position is being restored, the scroll is still the old one — keep the start of the current page.
        if strip
            && self.0.pending_strip.get().is_none()
            && let Some((row, fraction, _)) = self.strip_anchor(0.0)
        {
            let chunks = &self.0.state.borrow().chunks;
            // The top of the window on a chapter header (it's part of the gap above the next row) —
            // that's the start of the next page, not the end of the previous one.
            let StripGeometry { tops, heights } = self.strip_geometry();
            let header = self.0.strip_headers.borrow().get(row + 1).copied().unwrap_or(0.0);
            let y = tops[row] + fraction * (tops[row + 1] - tops[row]);
            if header > 0.0 && y >= tops[row] + heights[row] {
                pos = Position::page(chunks[row + 1].page);
            } else if let Some(meta) = chunks.get(row) {
                let first = chunks.partition_point(|c| c.page < meta.page);
                pos = Position::new(meta.page, row - first, fraction);
            }
        }
        if self.0.settings.borrow().position(&uuid) != Some(pos) {
            self.0.settings.borrow_mut().set_position(&uuid, pos);
            self.schedule_save();
        }
    }

    fn set_mode(&self, mode: Mode) {
        self.0.state.borrow_mut().mode = mode;
        match mode {
            Mode::Paged => {
                self.0.views.set_visible_child_name("paged");
                self.render_paged();
            }
            Mode::Strip => {
                self.0.views.set_visible_child_name("strip");
                // A webtoon has no bottom bar — only the strip; chapters are marked by headers.
                self.0.bar.set_reveal_child(false);
                self.0.card.set_reveal_child(false);
                self.scroll_strip_to_current();
            }
        }
        self.0.bar.set_visible(mode == Mode::Paged);
        self.update_zoom_label();
        self.poke_ui();
        // A different mode — different rules for effects (page / row in the strip).
        {
            let mut m = self.0.media.borrow_mut();
            m.sfx_gen += 1;
            m.sfx.clear();
            m.inside.clear();
            m.shown = None;
        }
        self.media_page_changed();
        self.update_strip_sfx();
    }

    fn set_rtl(&self, rtl: bool) {
        self.0.state.borrow_mut().rtl = rtl;
        // The slider is "mirrored" too: the first page is on the right.
        self.0.scale.set_inverted(rtl);
        // Chapter buttons — left/right on screen; the tooltip says where exactly in reading order.
        let [left, right] = &self.0.chapter_buttons;
        let (l, r) = if rtl { (tr("Next chapter"), tr("Previous chapter")) } else { (tr("Previous chapter"), tr("Next chapter")) };
        left.set_tooltip_text(Some(&format!("{l} (Ctrl+←)")));
        right.set_tooltip_text(Some(&format!("{r} (Ctrl+→)")));
        self.schedule_build();
    }

    // ------------------------------------------------------------------
    // Page attachments: sound, text, dark versions
    // ------------------------------------------------------------------

    fn sound_on(&self) -> bool {
        self.0.settings.borrow().sound && self.0.state.borrow().has_audio
    }

    fn dark_active(&self) -> bool {
        self.0.settings.borrow().dark_pages && self.0.state.borrow().has_dark && adw::StyleManager::default().is_dark()
    }

    /// Stop all sound and cancel deferred starts.
    fn stop_media(&self) {
        let mut m = self.0.media.borrow_mut();
        m.music = None;
        m.music_pending = None;
        m.music_expired = None;
        m.sfx.clear();
        m.inside.clear();
        m.shown = None;
        m.music_gen += 1;
        m.sfx_gen += 1;
    }

    /// Sound from attachment `k`, ready to start (volume = attachment × master).
    fn sound_of(&self, k: usize) -> Option<Sound> {
        let reader = self.0.state.borrow().reader.clone()?;
        let rec = *reader.aux().get(k)?;
        let sound = Sound::new(reader.get_aux_data(k).ok()?).map_err(|e| eprintln!("vnmf-reader: sound: {e}")).ok()?;
        sound.set_loop(rec.has_flag(AuxFlags::LOOP));
        sound.set_volume(rec.gain() * f64::from(self.0.settings.borrow().volume) / 100.0);
        Some(sound)
    }

    fn aux_delay(&self, k: usize) -> u32 {
        self.0.state.borrow().reader.as_ref().and_then(|r| r.aux().get(k).map(|a| a.delay_ms.get())).unwrap_or(0)
    }

    /// After a page (group) change: text, music and (in paged mode) effects.
    fn media_page_changed(&self) {
        let (paged, current) = {
            let state = self.0.state.borrow();
            if state.reader.is_none() {
                return;
            }
            (state.mode == Mode::Paged, state.current)
        };
        let key = if paged { self.group_index(current) } else { current };
        if self.0.media.borrow().shown == Some((paged, key)) {
            return;
        }
        self.0.media.borrow_mut().shown = Some((paged, key));
        self.update_text_panel();
        self.update_music();
        if paged {
            let generation = {
                let mut m = self.0.media.borrow_mut();
                m.sfx_gen += 1;
                m.sfx.clear();
                m.sfx_gen
            };
            if self.sound_on() {
                let pages = self.current_pages();
                let reader = self.0.state.borrow().reader.clone();
                for k in pages.iter().flat_map(|&p| reader.as_ref().map(|r| playback::sfx_of(r, p)).unwrap_or_default()) {
                    self.play_sfx(k, generation);
                }
            }
        }
    }

    /// Pages on screen: the group in paged mode, the current one in the strip.
    fn current_pages(&self) -> Vec<usize> {
        let state = self.0.state.borrow();
        if state.mode == Mode::Paged {
            let gi = self.group_index(state.current);
            state.groups.get(gi).cloned().unwrap_or_else(|| vec![state.current])
        } else {
            vec![state.current]
        }
    }

    fn play_sfx(&self, k: usize, generation: u64) {
        let delay = self.aux_delay(k);
        let this = self.clone();
        let start = move || {
            if this.0.media.borrow().sfx_gen != generation {
                return;
            }
            if let Some(sound) = this.sound_of(k) {
                sound.play();
                this.0.media.borrow_mut().sfx.insert(k, sound);
                this.limit_duration(k, false, generation);
            }
        };
        if delay == 0 {
            start();
        } else {
            glib::timeout_add_local_once(Duration::from_millis(u64::from(delay)), start);
        }
    }

    /// `duration_ms` of attachment `k`: after that long since the start the sound fades out (music also
    /// doesn't start again on its pages). `generation` — the start generation.
    fn limit_duration(&self, k: usize, music: bool, generation: u64) {
        let duration = self.0.state.borrow().reader.as_ref().and_then(|r| r.aux().get(k).map(|a| a.duration_ms.get())).unwrap_or(0);
        if duration == 0 {
            return;
        }
        let this = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(u64::from(duration)), move || {
            let sound = {
                let mut m = this.0.media.borrow_mut();
                if music {
                    if m.music_gen != generation || m.music.as_ref().map(|(n, _)| *n) != Some(k) {
                        return;
                    }
                    m.music_expired = Some(k);
                    m.music.take().map(|(_, s)| s)
                } else {
                    if m.sfx_gen != generation {
                        return;
                    }
                    m.sfx.remove(&k)
                }
            };
            if let Some(sound) = sound {
                sound.fade_out(800);
            }
        });
    }

    /// Music for the current page: the same one keeps playing, a different one replaces it.
    fn update_music(&self) {
        let want = if self.sound_on() {
            let state = self.0.state.borrow();
            let current = state.current;
            let chapter = state.toc.locate(current).map_or(0..usize::MAX, |(_, c)| c.pages.clone());
            state.reader.as_ref().and_then(|r| playback::music_at(r, current, chapter.start..chapter.end.min(r.pages_count())))
        } else {
            None
        };
        let (playing, pending, expired) = {
            let m = self.0.media.borrow();
            (m.music.as_ref().map(|(k, _)| *k), m.music_pending, m.music_expired)
        };
        if want.is_some() && (want == playing || want == pending) {
            return;
        }
        if want.is_some() && want == expired {
            return; // this music's time is already up — silence until a different one
        }
        self.0.media.borrow_mut().music_expired = None;
        let generation = {
            let mut m = self.0.media.borrow_mut();
            m.music = None;
            m.music_pending = want;
            m.music_gen += 1;
            m.music_gen
        };
        let Some(k) = want else { return };
        let this = self.clone();
        let start = move || {
            if this.0.media.borrow().music_gen != generation {
                return;
            }
            let sound = this.sound_of(k);
            let started = {
                let mut m = this.0.media.borrow_mut();
                m.music_pending = None;
                sound.map(|sound| {
                    sound.play();
                    m.music = Some((k, sound));
                })
            };
            if started.is_some() {
                this.limit_duration(k, true, generation);
            }
        };
        match self.aux_delay(k) {
            0 => start(),
            delay => {
                glib::timeout_add_local_once(Duration::from_millis(u64::from(delay)), start);
            }
        }
    }

    /// Strip: an effect plays when its trigger line appears on screen (in the top 85%).
    fn update_strip_sfx(&self) {
        if self.0.state.borrow().mode != Mode::Strip || !self.sound_on() {
            return;
        }
        let Some(reader) = self.0.state.borrow().reader.clone() else { return };
        let adj = self.0.strip_scroller.vadjustment();
        let (top, bottom) = (adj.value(), adj.value() + adj.page_size() * 0.85);
        if adj.page_size() <= 1.0 {
            return;
        }
        let StripGeometry { tops, heights: shown } = self.strip_geometry();
        let chunks = self.0.state.borrow().chunks.clone();
        let generation = self.0.media.borrow().sfx_gen;
        for (k, rec) in reader.aux().iter().enumerate().filter(|(_, a)| a.kind() == AuxKind::Sfx) {
            let page = rec.page_idx.get() as usize;
            let first = chunks.partition_point(|c| c.page < page);
            let rows: Vec<ChunkMeta> = chunks[first..].iter().take_while(|c| c.page == page).copied().collect();
            if rows.is_empty() {
                continue;
            }
            let heights: Vec<u32> = rows.iter().map(|c| c.height).collect();
            let (j, fraction) = playback::locate_y(&heights, rec.trigger_y.get());
            let Some(&row_top) = tops.get(first + j) else { continue };
            let y = row_top + fraction * shown[first + j];
            let inside = y >= top && y <= bottom;
            let was = self.0.media.borrow().inside.contains(&k);
            if inside && !was {
                self.0.media.borrow_mut().inside.insert(k);
                self.play_sfx(k, generation);
            } else if !inside && was {
                let mut m = self.0.media.borrow_mut();
                m.inside.remove(&k);
                // A looped effect ("while visible") — stop it; a one-shot — let it finish.
                if rec.has_flag(AuxFlags::LOOP) {
                    m.sfx.remove(&k);
                }
            }
        }
    }

    /// Sound was turned on / off / the volume changed.
    fn sound_changed(&self) {
        let on = self.0.settings.borrow().sound;
        self.0.sound_button.set_icon_name(if on { "audio-volume-high-symbolic" } else { "audio-volume-muted-symbolic" });
        self.stop_media();
        if on {
            self.media_page_changed();
            self.update_strip_sfx();
        } else {
            self.update_text_panel();
        }
    }

    /// Text of the pages on screen — in the panel (if it's open).
    fn update_text_panel(&self) {
        let show = self.0.settings.borrow().show_text && self.0.state.borrow().has_text;
        self.0.text_panel.set_reveal_child(show);
        if !show {
            return;
        }
        let pages = self.current_pages();
        let Some(reader) = self.0.state.borrow().reader.clone() else { return };
        let texts: Vec<String> = pages
            .iter()
            .flat_map(|&p| reader.page_aux_range(p).zip(reader.page_aux(p)).filter(|(_, a)| a.kind() == AuxKind::Text).map(|(k, _)| k).collect::<Vec<_>>())
            .filter_map(|k| reader.get_aux_data(k).ok().map(playback::text_layer))
            .filter(|t| !t.is_empty())
            .collect();
        let numbers: Vec<String> = pages.iter().map(|p| (p + 1).to_string()).collect();
        self.0.text_title.set_label(&trf!("Text · {0} {1}", if pages.len() > 1 { tr("pages") } else { tr("page") }, numbers.join("–")));
        if texts.is_empty() {
            self.0.text_label.set_label(tr("There is no text on this page"));
            self.0.text_label.add_css_class("dim-label");
        } else {
            self.0.text_label.set_label(&texts.join("\n\n"));
            self.0.text_label.remove_css_class("dim-label");
        }
    }

    /// The dark theme or settings changed — reload the page images.
    fn dark_changed(&self) {
        if !self.0.state.borrow().has_dark {
            return;
        }
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.cache.borrow_mut().clear();
        self.0.loading.borrow_mut().clear();
        for (_, picture) in self.0.strip_rows.borrow().iter() {
            picture.picture().set_paintable(None::<&gdk::Paintable>);
        }
        self.render_paged();
        self.schedule_strip_update();
    }

    // ------------------------------------------------------------------
    // Paged mode
    // ------------------------------------------------------------------

    fn page_chunks(&self, page: usize) -> Vec<ChunkMeta> {
        // The fragments go in page order: a binary search instead of walking the whole book.
        let state = self.0.state.borrow();
        let start = state.chunks.partition_point(|c| c.page < page);
        state.chunks[start..].iter().take_while(|c| c.page == page).copied().collect()
    }

    /// Preloading: `Settings::preload` units (spreads in paged mode, fragments in the strip) on
    /// **both sides** of the shown ones (`first..=last`), nearest first, forward before back at the
    /// same distance. `unit(i)` — the fragments of unit `i` (empty — out of range). What doesn't fit
    /// into the cache budget is skipped, symmetrically from the far end.
    fn prefetch_around(&self, first: usize, last: usize, unit: impl Fn(usize) -> Vec<ChunkMeta>) {
        let preload = self.0.settings.borrow().preload as usize;
        let budget = self.0.cache.borrow().max_bytes().saturating_sub(SCREEN_RESERVE);
        let mut wanted = Vec::new();
        let mut spent = 0usize;
        'fill: for offset in 1..=preload {
            for i in [last.checked_add(offset), first.checked_sub(offset)].into_iter().flatten() {
                for meta in unit(i) {
                    spent += meta.width as usize * meta.height as usize * 4;
                    if spent > budget {
                        break 'fill;
                    }
                    wanted.push(meta);
                }
            }
        }
        {
            // The far ones are marked as used first, the shown ones last: if something has to go
            // out of the cache, it'll be the far fragments, not the next one.
            let mut cache = self.0.cache.borrow_mut();
            for meta in wanted.iter().rev() {
                cache.touch(meta.chunk);
            }
            for meta in (first..=last).flat_map(&unit) {
                cache.touch(meta.chunk);
            }
        }
        for meta in wanted {
            self.prefetch(meta);
        }
    }

    /// Preloading in the strip: fragments above and below the visible ones (repeated only when
    /// the visible range changes). A fragment is the strip's "page": in manga read as a strip it
    /// is a page, in a webtoon — a piece of the episode (the whole episode is one page there).
    fn prefetch_strip(&self, visible: (usize, usize)) {
        if self.0.state.borrow().mode != Mode::Strip {
            return;
        }
        let key = (visible.0, visible.1, self.0.settings.borrow().preload, self.0.generation.get());
        if self.0.strip_prefetched.replace(Some(key)) == Some(key) {
            return;
        }
        let chunks = self.0.state.borrow().chunks.clone();
        self.prefetch_around(visible.0, visible.1, |i| chunks.get(i).copied().into_iter().collect());
    }

    /// Rebuild the display on the next iteration (several changes in a row — one rebuild).
    fn schedule_build(&self) {
        if self.0.render_pending.replace(true) {
            return;
        }
        let this = self.clone();
        glib::idle_add_local_once(move || {
            this.0.render_pending.set(false);
            this.render_paged();
        });
    }

    /// Recompute sizes on the next iteration (window resize).
    fn schedule_layout(&self) {
        let this = self.clone();
        glib::idle_add_local_once(move || {
            this.layout_paged();
            this.update_zoom_label();
        });
    }

    /// Display the current page / spread: creates the group's widgets. Sizes — `layout_paged`.
    fn render_paged(&self) {
        let (current, rtl) = {
            let state = self.0.state.borrow();
            if state.reader.is_none() || state.mode != Mode::Paged {
                return;
            }
            (state.current, state.rtl)
        };
        let groups = self.0.state.borrow().groups.clone();
        let gi = self.group_index(current);
        let group = groups.get(gi).cloned().unwrap_or_else(|| vec![current]);
        let infos = self.page_infos();

        let column = group.len() == 1 && self.page_chunks(group[0]).len() > 1;
        let (container, items): (gtk::Box, Vec<(AspectPicture, f64)>) = if column {
            // A page made of several fragments (a webtoon in paged mode): as a column.
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let chunks = self.page_chunks(group[0]);
            let last = chunks.len().saturating_sub(1);
            let items = chunks
                .into_iter()
                .enumerate()
                .map(|(i, meta)| {
                    let picture = self.page_picture(meta);
                    // Like in the strip: under the next fragment, so the seam doesn't show through.
                    picture.set_bleed(i32::from(i < last));
                    column.append(&picture);
                    (picture, f64::from(meta.height) / f64::from(meta.width.max(1)))
                })
                .collect();
            (column, items)
        } else {
            // A single page or a spread: on screen from left to right (a spread from the file — by roles).
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let items = layout::visual_order(&group, &infos, rtl)
                .into_iter()
                .filter_map(|page| {
                    let meta = self.page_chunks(page).first().copied()?;
                    let picture = self.page_picture(meta);
                    row.append(&picture);
                    let (w, h) = self.page_size(page);
                    Some((picture, f64::from(w) / f64::from(h)))
                })
                .collect();
            (row, items)
        };
        container.set_halign(gtk::Align::Center);
        container.set_valign(gtk::Align::Center);
        *self.0.paged_items.borrow_mut() = (column, items);
        self.layout_paged();
        self.0.paged_scroller.set_child(Some(&container));

        // A new page — to its start (when reading right to left — the right edge).
        // Zoom and window resizing don't reset the position: `pending_*` restores it.
        if self.0.rendered_group.replace(Some(gi)) != Some(gi) {
            // Start of a chapter (or volume) — a card with the title.
            let start = {
                let state = self.0.state.borrow();
                group.iter().copied().find(|&p| state.toc.chapters().any(|c| c.pages.start == p))
            };
            if let Some(page) = start {
                self.show_card(page);
            }
            let (h, v) = (self.0.paged_scroller.hadjustment(), self.0.paged_scroller.vadjustment());
            let vw = f64::from(self.0.viewport.get().0.max(1));
            v.set_value(0.0);
            h.set_value(if rtl { h.upper() } else { 0.0 });
            self.0.pending_v.set(Some((0.0, 0.0)));
            self.0.pending_h.set(Some(if rtl { (1.0, vw) } else { (0.0, 0.0) }));
        }
        self.update_zoom_label();

        // Pre-decoding `preload` spreads (pages) forward and as many back.
        self.prefetch_around(gi, gi, |g| groups.get(g).into_iter().flatten().flat_map(|&p| self.page_chunks(p)).collect());
    }

    /// Image sizes of the shown group by zoom and window size (without rebuilding widgets).
    fn layout_paged(&self) {
        if self.0.state.borrow().mode != Mode::Paged {
            return;
        }
        let (fit, zoom) = {
            let s = self.0.settings.borrow();
            (s.fit, s.zoom)
        };
        let (vw, vh) = match self.0.viewport.get() {
            (w, h) if w > 1 && h > 1 => (f64::from(w), f64::from(h)),
            _ => (1000.0, 800.0),
        };
        let items = self.0.paged_items.borrow();
        let (column, ref pictures) = *items;
        let ratios: Vec<f64> = pictures.iter().map(|(_, r)| *r).collect();
        let (sizes, effective) = if column {
            layout::column_sizes(&ratios, vw, fit, zoom)
        } else {
            layout::row_sizes(&ratios, self.0.state.borrow().page_unit, (vw, vh), fit, zoom)
        };
        let (mut cw, mut ch) = (0.0f64, 0.0f64);
        for ((picture, _), (w, h)) in pictures.iter().zip(&sizes) {
            picture.set_fixed_size(*w as i32, *h as i32);
            if column {
                cw = *w;
                ch += h;
            } else {
                cw += w;
                ch = *h;
            }
        }
        self.0.expected_size.set((cw, ch));
        self.0.effective_zoom.set(effective);
        drop(items);
        self.update_zoom_label();
    }

    /// A page image: the exact size is set by `layout_paged`. Exactly exact (both the minimum
    /// and the natural size): a plain `gtk::Picture` has the full image size as its natural size,
    /// and a page narrower than the window wouldn't be centered.
    fn page_picture(&self, meta: ChunkMeta) -> AspectPicture {
        let picture = AspectPicture::new();
        picture.set_fixed_size(1, 1);
        picture.set_paintable(Some(&imaging::placeholder(meta.width, meta.height)));
        self.load_into(picture.picture(), meta);
        picture
    }

    // ------------------------------------------------------------------
    // Strip
    // ------------------------------------------------------------------

    /// Builds the strip: a row per fragment; images are loaded only near the visible area.
    fn build_strip(&self) {
        while let Some(child) = self.0.strip_box.first_child() {
            self.0.strip_box.remove(&child);
        }
        let chunks = self.0.state.borrow().chunks.clone();
        // A header above the first fragment of each chapter (if there are several chapters or volumes).
        let headers: Vec<Option<(String, String)>> = {
            let state = self.0.state.borrow();
            let toc = &state.toc;
            chunks
                .iter()
                .enumerate()
                .map(|(i, meta)| {
                    let first_chunk = i == 0 || chunks[i - 1].page != meta.page;
                    if !toc.is_structured() || !first_chunk {
                        return None;
                    }
                    let (v, c) = toc.locate(meta.page)?;
                    (c.pages.start == meta.page).then(|| {
                        let volume = if toc.has_volumes && v.pages.start == meta.page { v.label() } else { String::new() };
                        (volume, c.label())
                    })
                })
                .collect()
        };
        *self.0.strip_headers.borrow_mut() = headers.iter().map(|h| if h.is_some() { f64::from(STRIP_HEADER) } else { 0.0 }).collect();
        let rows: Vec<(adw::Clamp, AspectPicture)> = chunks
            .iter()
            .zip(&headers)
            .map(|(meta, header)| {
                if let Some((volume, chapter)) = header {
                    self.0.strip_box.append(&strip_header(volume, chapter));
                }
                let picture = AspectPicture::new();
                picture.set_dimensions(meta.width, meta.height);
                let clamp = adw::Clamp::builder().child(&picture).build();
                self.0.strip_box.append(&clamp);
                (clamp, picture)
            })
            .collect();
        *self.0.strip_rows.borrow_mut() = rows;
        self.apply_strip_layout();
    }

    /// Strip width and gaps — for all rows at once (zoom, settings).
    fn apply_strip_layout(&self) {
        let (width, gap) = {
            let s = self.0.settings.borrow();
            (s.strip_width, s.strip_gap)
        };
        let headers = self.0.strip_headers.borrow();
        for (i, (clamp, picture)) in self.0.strip_rows.borrow().iter().enumerate() {
            clamp.set_maximum_size(width);
            clamp.set_tightening_threshold(width);
            clamp.set_margin_bottom(gap);
            picture.set_natural_width(width);
            // Without a gap the fragments join — extend under the next one so the seam doesn't show through
            // (but not under a chapter header).
            let joined = gap == 0 && headers.get(i + 1).is_some_and(|&h| h == 0.0);
            picture.set_bleed(i32::from(joined));
        }
        drop(headers);
        self.schedule_strip_update();
    }

    /// Top edges of all strip rows (and the total height as the last element).
    fn strip_tops(&self) -> Vec<f64> {
        self.strip_geometry().tops
    }

    /// Strip layout for the current sizes — even before GTK applies it (so the position
    /// after a zoom can be set in the same frame). Heights are asked from the widgets themselves, with the same
    /// measurements `gtk::Box` uses: that way they match the real layout at any
    /// screen scale, fonts and scrollbars. (FIX: they used to be computed by a separate formula; when it
    /// diverged from GTK, the position after a zoom wasn't restored, and visible rows were unloaded.)
    /// While the strip isn't shown — the same formula as an estimate.
    fn strip_geometry(&self) -> StripGeometry {
        let rows = self.0.state.borrow().chunks.len();
        let width = self.0.strip_box.width();
        if width > 0 {
            let (mut tops, mut heights, mut y) = (Vec::with_capacity(rows + 1), Vec::with_capacity(rows), 0.0);
            let mut child = self.0.strip_box.first_child();
            while let Some(widget) = child {
                if widget.is_visible() {
                    // Size including margins (the strip gap is the row's bottom margin).
                    let height = f64::from(widget.measure(gtk::Orientation::Vertical, width).1);
                    if widget.is::<adw::Clamp>() {
                        tops.push(y);
                        heights.push(height - f64::from(widget.margin_top() + widget.margin_bottom()));
                    }
                    y += height;
                }
                child = widget.next_sibling();
            }
            tops.push(y);
            if heights.len() == rows {
                return StripGeometry { tops, heights };
            }
        }
        let (width, gap) = {
            let s = self.0.settings.borrow();
            (f64::from(s.strip_width), f64::from(s.strip_gap))
        };
        let row_width = width.min(self.strip_area_width());
        let ratios: Vec<f64> = self.0.state.borrow().chunks.iter().map(|c| f64::from(c.height) / f64::from(c.width.max(1))).collect();
        StripGeometry {
            tops: layout::strip_tops(&ratios, &self.0.strip_headers.borrow(), row_width, gap),
            heights: ratios.iter().map(|k| (row_width * k).round()).collect(),
        }
    }

    /// The row and the fraction within it at point `anchor_y` of the visible area.
    fn strip_anchor(&self, anchor_y: f64) -> Option<(usize, f64, f64)> {
        let y = self.0.strip_scroller.vadjustment().value() + anchor_y;
        let tops = self.strip_tops();
        let row = tops.windows(2).position(|w| y < w[1])?;
        let height = (tops[row + 1] - tops[row]).max(1.0);
        Some((row, ((y - tops[row]) / height).clamp(0.0, 1.0), anchor_y))
    }

    /// Restore the position as soon as the scroller knows the new strip height (zoom, mode switch).
    /// Set the saved strip point. The new strip height is known even before layout (it's measured
    /// from the widgets), so the scroller learns it right away, and the position goes into the same frame as the new
    /// sizes. FIX: the position used to be set in `changed` — i.e. in the middle of layout, when GTK no longer
    /// shifts the content in this frame: a different place of the strip remained on screen (a jump on zoom),
    /// and images were loaded for the place that was considered visible — and the visible rows were empty.
    fn apply_strip_pending(&self, adj: &gtk::Adjustment) {
        let Some((row, fraction, anchor_y)) = self.0.pending_strip.get() else { return };
        if self.0.strip_box.width() <= 0 {
            return; // the strip isn't shown yet: sizes are unknown
        }
        let tops = self.strip_tops();
        if row + 1 >= tops.len() {
            return;
        }
        self.0.pending_strip.set(None);
        // GTK will set the same height itself during layout; without it the value would be clamped by the old one.
        let upper = tops.last().copied().unwrap_or(0.0).max(adj.page_size());
        if adj.upper() != upper {
            adj.set_upper(upper);
        }
        let target = tops[row] + fraction * (tops[row + 1] - tops[row]) - anchor_y;
        let max = (upper - adj.page_size()).max(adj.lower());
        adj.set_value(target.clamp(adj.lower(), max));
    }

    fn scroll_strip_to_current(&self) {
        let current = self.0.state.borrow().current;
        self.scroll_strip_to(Position::page(current));
    }

    /// Scroll the strip so that point `pos` ends up near the top edge of the window.
    fn scroll_strip_to(&self, pos: Position) {
        let row = {
            let chunks = &self.0.state.borrow().chunks;
            let first = chunks.partition_point(|c| c.page < pos.page);
            let count = chunks[first..].iter().take_while(|c| c.page == pos.page).count();
            if count == 0 {
                return;
            }
            // The file may have been rebuilt with a different slicing — no further than the page's last fragment.
            first + pos.chunk.min(count - 1)
        };
        // If the layout already exists — right away; otherwise after it (in `changed`).
        // The start of a chapter — together with the header above it.
        let header = if pos.chunk == 0 && pos.offset == 0.0 { self.0.strip_headers.borrow().get(row).copied().unwrap_or(0.0) } else { 0.0 };
        self.0.pending_strip.set(Some((row, pos.offset, header)));
        self.apply_strip_pending(&self.0.strip_scroller.vadjustment());
        self.schedule_strip_update();
    }

    fn schedule_strip_update(&self) {
        if self.0.strip_update_pending.replace(true) {
            return;
        }
        let this = self.clone();
        glib::idle_add_local_once(move || {
            this.0.strip_update_pending.set(false);
            this.update_strip_visible();
        });
    }

    /// Loads the images of visible (and neighbouring) rows and releases distant ones.
    fn update_strip_visible(&self) {
        if self.0.state.borrow().mode != Mode::Strip {
            return;
        }
        let adj = self.0.strip_scroller.vadjustment();
        let (value, page) = (adj.value(), adj.page_size().max(1.0));
        let (near_top, near_bottom) = (value - page, value + 2.0 * page);
        let (far_top, far_bottom) = (value - 4.0 * page, value + 5.0 * page);
        let chunks = self.0.state.borrow().chunks.clone();
        let rows = self.0.strip_rows.borrow().clone();
        let tops = self.strip_tops();
        // The new sizes are already there, but the scroll is still old (zoom until the next frame): the visible rows
        // would be determined incorrectly — the truly visible ones would be unloaded. After layout
        // `changed` will call this again.
        if self.0.pending_strip.get().is_some() && (adj.upper() - tops.last().copied().unwrap_or(0.0).max(adj.page_size())).abs() > 2.0 {
            return;
        }
        for (i, (_, picture)) in rows.iter().enumerate() {
            let (top, bottom) = (tops[i], tops[i + 1]);
            let picture = picture.picture();
            if bottom >= near_top && top <= near_bottom {
                if picture.paintable().is_none()
                    && let Some(meta) = chunks.get(i)
                {
                    self.load_into(picture, *meta);
                }
            } else if (bottom < far_top || top > far_bottom) && picture.paintable().is_some() {
                // The texture stays in the cache; the widget releases it so a long strip doesn't hold everything.
                picture.set_paintable(None::<&gdk::Paintable>);
            }
        }
        // The rows actually on screen (not the "near" margin above).
        let shown: Vec<usize> = (0..rows.len()).filter(|&i| tops[i + 1] > value && tops[i] < value + page).collect();
        if let (Some(&first), Some(&last)) = (shown.first(), shown.last()) {
            self.prefetch_strip((first, last));
        }
    }

    /// Page number in the top third of the visible strip area.
    fn track_strip_position(&self, adj: &gtk::Adjustment) {
        // While the position is still being restored — the current page is known, don't overwrite it.
        if self.0.state.borrow().mode != Mode::Strip || self.0.pending_strip.get().is_some() {
            return;
        }
        let Some((row, _, _)) = self.strip_anchor(adj.page_size() / 3.0) else { return };
        let Some(page) = self.0.state.borrow().chunks.get(row).map(|c| c.page) else { return };
        if page != self.0.state.borrow().current {
            self.0.state.borrow_mut().current = page;
            self.sync_position();
        } else {
            self.remember_position();
        }
    }

    // ------------------------------------------------------------------
    // Image loading
    // ------------------------------------------------------------------

    /// Chunk data + CRC check. A damaged page is shown as is and goes into the banner.
    fn chunk_bytes(&self, meta: ChunkMeta) -> Option<Vec<u8>> {
        let reader = self.0.state.borrow().reader.clone()?;
        if self.dark_active() {
            let part = meta.chunk.saturating_sub(reader.pages().get(meta.page)?.first_chunk_idx.get() as usize);
            if let Some(k) = playback::dark_of(&reader, meta.page, part)
                && let Ok(data) = reader.get_aux_data(k)
            {
                return Some(data.to_vec());
            }
        }
        let data = reader.get_chunk_data(meta.chunk).ok()?.to_vec();
        if !reader.verify_chunk_crc(meta.chunk).unwrap_or(false) {
            self.report_bad_page(meta.page);
        }
        Some(data)
    }

    fn load_into(&self, picture: &gtk::Picture, meta: ChunkMeta) {
        // A marker of which chunk this (possibly reused) widget is waiting for.
        picture.set_widget_name(&meta.chunk.to_string());
        if let Some(texture) = self.0.cache.borrow_mut().get(meta.chunk) {
            picture.set_paintable(Some(&texture));
            return;
        }
        self.decode(meta, Some(picture));
    }

    fn prefetch(&self, meta: ChunkMeta) {
        if !self.0.cache.borrow().contains(meta.chunk) {
            self.decode(meta, None);
        }
    }

    /// Decodes a chunk into the cache and hands it to all widgets waiting for it.
    /// If the chunk is already being decoded — the widget just joins the queue (FIX: it used to be skipped
    /// and stayed a placeholder, e.g. on the saved page when opening a file).
    fn decode(&self, meta: ChunkMeta, picture: Option<&gtk::Picture>) {
        {
            let mut loading = self.0.loading.borrow_mut();
            if let Some(waiters) = loading.get_mut(&meta.chunk) {
                waiters.extend(picture.map(|p| p.downgrade()));
                return;
            }
            loading.insert(meta.chunk, picture.map(|p| p.downgrade()).into_iter().collect());
        }
        let Some(bytes) = self.chunk_bytes(meta) else {
            self.0.loading.borrow_mut().remove(&meta.chunk);
            return;
        };
        let (this, generation) = (self.clone(), self.0.generation.get());
        glib::spawn_future_local(async move {
            let result = imaging::decode(bytes).await;
            if this.0.generation.get() != generation {
                return; // a different file was opened; `loading` is already cleared
            }
            let waiters = this.0.loading.borrow_mut().remove(&meta.chunk).unwrap_or_default();
            // Only widgets that still show this chunk (the strip reuses rows).
            let waiters = waiters.iter().filter_map(|w| w.upgrade()).filter(|p| p.widget_name() == meta.chunk.to_string());
            match result {
                Ok(texture) => {
                    this.0.cache.borrow_mut().insert(meta.chunk, texture.clone());
                    for picture in waiters {
                        picture.set_paintable(Some(&texture));
                    }
                    this.schedule_strip_update();
                }
                // A decoding error is not file damage (CRC is checked separately in chunk_bytes):
                // the image may be in a format the system doesn't have.
                Err(e) => {
                    let mut asked = false;
                    for picture in waiters {
                        picture.set_tooltip_text(Some(&trf!("Could not display the page: {0}", e)));
                        asked = true;
                    }
                    if asked && this.0.undecodable.borrow_mut().insert(meta.page) {
                        widgets::toast(
                            &this.0.toasts,
                            &trf!("Page {0} could not be displayed: the format isn't supported by the system decoder", meta.page + 1),
                        );
                    }
                }
            }
        });
    }
}

impl AppWindow for ReaderWindow {
    fn present(&self) {
        self.0.window.present();
    }

    fn gtk_window(&self) -> gtk::Window {
        self.0.window.clone().upcast()
    }

    fn open(&self, path: PathBuf) {
        let reader = match VnmfReader::open(&path) {
            Ok(r) => Rc::new(r),
            Err(e) => {
                let health =
                    Health { issues: vec![trf!("the file can't be opened: {0}", e)], prognosis: tr("the metadata may be recoverable"), data_checked: false };
                health_ui::update_banner(&self.0.banner, &health, &path);
                widgets::toast(&self.0.toasts, &trf!("Could not open: {0}", e));
                return;
            }
        };
        self.stop_media();
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.cache.borrow_mut().clear();
        self.0.loading.borrow_mut().clear();
        self.0.undecodable.borrow_mut().clear();
        self.0.rendered_group.set(None);

        // Sizes of all fragments — from the image headers, without decoding.
        let mut chunks = Vec::new();
        for (i, page) in reader.pages().iter().enumerate() {
            for chunk in page.chunk_range() {
                let (width, height) =
                    reader.get_chunk_data(chunk).ok().and_then(vnmf_core::image_dimensions).unwrap_or((page.width.get().max(1), page.height.get().max(1)));
                chunks.push(ChunkMeta { page: i, chunk, width, height });
            }
        }

        let webtoon = reader.has_flag(HeaderBitEnum::IsWebToon);
        let rtl = reader.pages().first().is_some_and(|p| p.flags.get() & PageBitFlags::READING_RTL != 0);
        let health = assess(&reader, None);
        health_ui::update_banner(&self.0.banner, &health, &path);
        self.0.sidebar.set_child(Some(&self.build_sidebar(&reader, &health)));
        let uuid = vnmf::format_uuid(reader.uuid());

        {
            let mut state = self.0.state.borrow_mut();
            state.reader = Some(reader.clone());
            state.path = Some(path.clone());
            state.uuid = uuid.clone();
            state.chunks = chunks;
            state.current = 0;
            state.bad_pages.clear();
            state.toc = Toc::from_reader(&reader);
            state.has_audio = playback::has_kind(reader.aux(), AuxKind::Music) || playback::has_kind(reader.aux(), AuxKind::Sfx);
            state.has_text = playback::has_kind(reader.aux(), AuxKind::Text);
            state.has_dark = playback::has_kind(reader.aux(), AuxKind::DarkImage);
        }
        {
            let state = self.0.state.borrow();
            self.0.sound_button.set_visible(state.has_audio);
            self.0.text_button.set_visible(state.has_text);
            for (name, on) in [("sound", state.has_audio), ("text", state.has_text), ("dark-pages", state.has_dark)] {
                if let Some(action) = self.0.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                    action.set_enabled(on);
                }
            }
        }
        self.update_chapter_buttons();
        self.rebuild_groups();
        self.build_strip();

        self.set_action_state("rtl", rtl.to_variant());
        self.set_rtl(rtl);
        self.set_action_state("mode", if webtoon { "strip" } else { "paged" }.to_variant());

        self.0.stack.set_visible_child_name("content");
        self.set_mode(if webtoon { Mode::Strip } else { Mode::Paged });

        // Continue from where reading stopped.
        let saved = {
            let s = self.0.settings.borrow();
            if s.remember_position { s.position(&uuid) } else { None }
        };
        match saved.filter(|p| !p.is_start() && p.page < reader.pages_count()) {
            Some(pos) => {
                self.go_to(pos.page);
                if self.0.state.borrow().mode == Mode::Strip {
                    self.scroll_strip_to(pos);
                }
                let title = if pos.page > 0 { trf!("Continued from page {0}", pos.page + 1) } else { tr("Continued from where you left off").to_string() };
                let toast = adw::Toast::builder().title(title).button_label(tr("From the start")).timeout(5).build();
                let this = self.clone();
                toast.connect_button_clicked(move |_| this.go_to(0));
                self.0.toasts.add_toast(toast.clone());
                // Our own timer: the libadwaita countdown stops while the toast button has focus, and in the
                // reader window it gets focus by itself (there are no other buttons in the content) — the toast never disappeared.
                glib::timeout_add_local_once(Duration::from_secs(5), move || toast.dismiss());
            }
            None => self.go_to(0),
        }

        let title = Metadata::from_info(reader.info_bytes()).and_then(|m| m.title);
        self.0.title.set_title(title.as_deref().unwrap_or("VNMF Reader"));
        self.0.window.set_title(Some(title.as_deref().unwrap_or("VNMF Reader")));
    }
}

/// Scroll point to save: the fraction of the content under point `offset` of the visible area.
fn anchor(adj: &gtk::Adjustment, offset: f64) -> Option<(f64, f64)> {
    (adj.upper() > 0.0).then(|| ((adj.value() + offset) / adj.upper(), offset))
}

/// Restores the saved point for the new content size `expected`. `ahead` — the size is known
/// in advance (zoom): the scroller learns it right away, and the position goes into the same frame as the new
/// sizes (GTK will set the same during layout). Otherwise — only once the scroller already knows
/// the new size (intermediate notifications while the content is being replaced are skipped).
fn apply_pending(adj: &gtk::Adjustment, cell: &Cell<Option<(f64, f64)>>, expected: f64, ahead: bool) {
    let Some((fraction, offset)) = cell.get() else { return };
    let mut upper = adj.upper();
    if ahead {
        upper = expected.max(adj.page_size());
        if adj.upper() != upper {
            adj.set_upper(upper);
        }
    } else if (upper - expected.max(adj.page_size())).abs() > 2.0 {
        return;
    }
    cell.set(None);
    let max = (upper - adj.page_size()).max(adj.lower());
    adj.set_value((fraction * upper - offset).clamp(adj.lower(), max));
}

/// Key `target` taking the layout into account: a match by symbol or by physical key
/// (in any layout group — e.g. "в" in place of "d" in Ukrainian).
fn is_key(key: gdk::Key, keycode: u32, target: gdk::Key) -> bool {
    key.to_lower() == target
        || gdk::Display::default().and_then(|d| d.map_keycode(keycode)).is_some_and(|keys| keys.iter().any(|(_, k)| k.to_lower() == target))
}

/// Zoom button in the header: "100%" + a popover with −/+/reset and the fit mode.
fn zoom_menu_button() -> gtk::MenuButton {
    let out = gtk::Button::builder().icon_name("zoom-out-symbolic").tooltip_text(tr("Zoom out (Ctrl+−)")).action_name("win.zoom-out").build();
    let reset = gtk::Button::builder().label("100%").tooltip_text(tr("Full window width (Ctrl+0)")).action_name("win.zoom-reset").hexpand(true).build();
    let zin = gtk::Button::builder().icon_name("zoom-in-symbolic").tooltip_text(tr("Zoom in (Ctrl+=)")).action_name("win.zoom-in").build();
    let row = gtk::Box::builder().spacing(0).build();
    row.add_css_class("linked");
    row.append(&out);
    row.append(&reset);
    row.append(&zin);

    let fit_page = gtk::CheckButton::builder().label(tr("Fit page")).action_name("win.fit").build();
    fit_page.set_action_target_value(Some(&"page".to_variant()));
    let fit_width = gtk::CheckButton::builder().label(tr("Fit width")).action_name("win.fit").group(&fit_page).build();
    fit_width.set_action_target_value(Some(&"width".to_variant()));
    let hint = gtk::Label::builder()
        .label(tr("100% — the page at full window width; a spread takes the space of two pages.\n\"Fit page\" shows the whole spread.\nCtrl+scroll or pinch — zoom around the cursor."))
        .wrap(true)
        .xalign(0.0)
        .build();
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .width_request(220)
        .build();
    column.append(&row);
    column.append(&fit_page);
    column.append(&fit_width);
    column.append(&hint);

    let popover = gtk::Popover::builder().child(&column).build();
    gtk::MenuButton::builder().label("100%").tooltip_text(tr("Zoom")).popover(&popover).build()
}

fn reader_menu() -> gtk::MenuButton {
    let menu = gio::Menu::new();

    let view = gio::Menu::new();
    view.append(Some(tr("Pages")), Some("win.mode::paged"));
    view.append(Some(tr("Strip (webtoon)")), Some("win.mode::strip"));
    menu.append_section(Some(tr("Mode")), &view);

    let reading = gio::Menu::new();
    reading.append(Some(tr("Right to left")), Some("win.rtl"));
    reading.append(Some(tr("Spread (two pages)")), Some("win.double"));
    reading.append(Some(tr("Fullscreen")), Some("win.fullscreen"));
    menu.append_section(None, &reading);

    let extras = gio::Menu::new();
    extras.append(Some(tr("Page sound")), Some("win.sound"));
    extras.append(Some(tr("Page text")), Some("win.text"));
    extras.append(Some(tr("Dark page versions")), Some("win.dark-pages"));
    menu.append_section(None, &extras);

    let chapters = gio::Menu::new();
    chapters.append(Some(tr("Previous chapter")), Some("win.prev-chapter"));
    chapters.append(Some(tr("Next chapter")), Some("win.next-chapter"));
    menu.append_section(None, &chapters);

    let other = gio::Menu::new();
    other.append(Some(tr("Reading settings…")), Some("win.preferences"));
    other.append(Some(tr("Open in VNMF Info")), Some("win.info"));
    other.append(Some(tr("About VNMF Reader")), Some("win.about"));
    menu.append_section(None, &other);

    gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).primary(true).tooltip_text(tr("Main menu")).build()
}

/// A chapter header in the strip: exactly `STRIP_HEADER` px tall (positions depend on it).
fn strip_header(volume: &str, chapter: &str) -> gtk::Box {
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .valign(gtk::Align::Start)
        .height_request(STRIP_HEADER)
        .overflow(gtk::Overflow::Hidden)
        .build();
    header.add_css_class("strip-chapter-header");
    let inner = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).vexpand(true).valign(gtk::Align::Center).build();
    if !volume.is_empty() {
        let label = gtk::Label::builder().label(volume).ellipsize(gtk::pango::EllipsizeMode::End).build();
        label.add_css_class("caption-heading");
        label.add_css_class("dim-label");
        inner.append(&label);
    }
    let label = gtk::Label::builder().label(chapter).ellipsize(gtk::pango::EllipsizeMode::End).build();
    label.add_css_class("title-3");
    inner.append(&label);
    header.append(&inner);
    header
}
