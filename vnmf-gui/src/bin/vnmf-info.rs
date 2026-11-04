// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! VNMF Info — full information about a file (like `vnmfinfo`) with a cover and page viewer.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;
use vnmf::contents::Toc;
use vnmf::health::{Health, assess, chunk_formats, is_mixed, role_name, verify};
use vnmf::metadata::{self, Metadata};
use vnmf_core::header::bit_flag::HeaderFlagsFormatter;
use vnmf_core::{HeaderBitEnum, InfoFormat, PageBitFlags, PageFormat, VnmfReader, recovery};
use vnmf_gui::aspect_picture::AspectPicture;
use vnmf_gui::{AppWindow, health_ui, human_size, imaging, widgets};
use vnmf_i18n::{ntrf, tr, trf};

const APP_ID: &str = "org.vnmf.Info";
/// Width to which images in the viewer panel are downscaled.
const PREVIEW_WIDTH: i32 = 1400;

fn main() -> glib::ExitCode {
    vnmf_gui::run_app(APP_ID, InfoWindow::new)
}

/// What the viewer panel is currently showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shown {
    Preview(usize),
    Page(usize),
}

#[derive(Clone)]
struct InfoWindow(Rc<Inner>);

struct Inner {
    window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    details: adw::Bin,
    preview_area: adw::Bin,
    preview_caption: gtk::Label,
    reader: RefCell<Option<Rc<VnmfReader>>>,
    path: RefCell<Option<PathBuf>>,
    /// Page rows — to mark the damaged ones after the background check.
    /// Page rows (in the contents they are built only when a chapter is expanded — hence `Option`).
    page_rows: RefCell<Vec<Option<adw::ActionRow>>>,
    /// Damaged pages from the full check — also marked in rows built later.
    bad_pages: RefCell<Vec<usize>>,
    /// Contents chapters with page ranges (marked if a damaged page is inside).
    chapter_rows: RefCell<Vec<(adw::ExpanderRow, std::ops::Range<usize>)>>,
    /// The status row and its current prefix (a spinner, then an icon).
    integrity_row: RefCell<Option<(adw::ActionRow, gtk::Widget)>>,
    /// Load counter: the result of a stale background task is ignored.
    generation: std::cell::Cell<u64>,
}

impl InfoWindow {
    fn new(app: &adw::Application) -> Self {
        let title = adw::WindowTitle::new("VNMF Info", "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));

        let open_button = gtk::Button::builder().icon_name("document-open-symbolic").tooltip_text(tr("Open (Ctrl+O)")).build();
        open_button.set_action_name(Some("win.open"));
        header.pack_start(&open_button);
        header.pack_end(&widgets::menu_button(&[(tr("Language…"), "win.language"), (tr("About VNMF Info"), "win.about")]));

        let banner = health_ui::banner();

        let (empty, empty_button) =
            widgets::open_status_page("document-properties-symbolic", tr("VNMF file information"), tr("Open a .vnmf file or drag it into the window"));
        empty_button.set_action_name(Some("win.open"));

        // Contents: viewer panel on the left, properties on the right.
        let preview_area = adw::Bin::new();
        preview_area.set_vexpand(true);
        let preview_caption = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        preview_caption.add_css_class("dim-label");
        preview_caption.add_css_class("caption");
        let preview_box =
            gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
        preview_box.append(&preview_area);
        preview_box.append(&preview_caption);

        let details = adw::Bin::new();
        let split = adw::OverlaySplitView::builder()
            .sidebar(&preview_box)
            .content(&details)
            .sidebar_width_fraction(0.42)
            .max_sidebar_width(700.0)
            .min_sidebar_width(260.0)
            .build();

        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&split, Some("content"));
        stack.add_named(&adw::Bin::new(), Some("error"));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&toasts));

        let window = adw::ApplicationWindow::builder().application(app).title("VNMF Info").default_width(1200).default_height(800).content(&toolbar).build();

        // Narrow window — the viewer panel hides, the properties remain.
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, 720.0, adw::LengthUnit::Sp));
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        let this = Self(Rc::new(Inner {
            window,
            title,
            stack,
            banner,
            toasts,
            details,
            preview_area,
            preview_caption,
            reader: RefCell::new(None),
            path: RefCell::new(None),
            page_rows: RefCell::new(Vec::new()),
            bad_pages: RefCell::new(Vec::new()),
            chapter_rows: RefCell::new(Vec::new()),
            integrity_row: RefCell::new(None),
            generation: std::cell::Cell::new(0),
        }));
        this.install_actions();
        widgets::install_about(&this.0.window, "VNMF Info", tr("Full information about VNMF files and integrity checking."));
        // A viewer loses nothing by restarting: the same file opens again.
        let me = this.clone();
        widgets::install_language(
            &this.0.window,
            widgets::LanguageRestart::Offer(std::rc::Rc::new(move || {
                let path = me.0.path.borrow().clone();
                vnmf_gui::restart_app(&me.0.window, path);
            })),
        );
        this
    }

    fn install_actions(&self) {
        let open = gio::SimpleAction::new("open", None);
        let this = self.clone();
        open.connect_activate(move |_, _| {
            let this = this.clone();
            glib::spawn_future_local(async move {
                if let Some(path) = widgets::choose_vnmf(&this.0.window).await {
                    this.open(path);
                }
            });
        });
        self.0.window.add_action(&open);

        let this = self.clone();
        self.0.window.add_controller(widgets::file_drop_target(move |paths| {
            if let Some(path) = paths.into_iter().next() {
                this.open(path);
            }
        }));
    }
}

impl AppWindow for InfoWindow {
    fn present(&self) {
        self.0.window.present();
    }

    fn gtk_window(&self) -> gtk::Window {
        self.0.window.clone().upcast()
    }

    fn open(&self, path: PathBuf) {
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.0.title.set_subtitle(&name);
        self.0.banner.set_revealed(false);
        *self.0.path.borrow_mut() = Some(path.clone());

        let reader = match VnmfReader::open(&path) {
            Ok(reader) => Rc::new(reader),
            Err(e) => {
                self.show_unreadable(&path, &e.to_string());
                return;
            }
        };
        *self.0.reader.borrow_mut() = Some(reader.clone());

        // Quick assessment (metadata only) — right away; the full one — in the background.
        let quick = assess(&reader, None);
        health_ui::update_banner(&self.0.banner, &quick, &path);
        self.0.details.set_child(Some(&self.build_details(&reader, &path)));
        self.0.stack.set_visible_child_name("content");

        let first = if reader.previews_count() > 0 { Shown::Preview(0) } else { Shown::Page(0) };
        if reader.previews_count() > 0 || reader.pages_count() > 0 {
            self.show(first);
        }
        self.run_full_check(path, generation);
    }
}

impl InfoWindow {
    /// Full check (CRC of all data and parity) in the background; the result updates the status and page rows.
    fn run_full_check(&self, path: PathBuf, generation: u64) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let job_path = path.clone();
            let result = gio::spawn_blocking(move || -> anyhow::Result<(Health, Vec<usize>)> {
                let reader = VnmfReader::open(&job_path)?;
                let verification = verify(&reader)?;
                let health = assess(&reader, Some(&verification));
                // Numbers of damaged pages.
                let mut pages: Vec<usize> = verification.bad_chunks.iter().filter_map(|&c| vnmf::health::chunk_owner(&reader, c)).map(|p| p - 1).collect();
                pages.dedup();
                Ok((health, pages))
            })
            .await;

            if this.0.generation.get() != generation {
                return; // another file was opened meanwhile
            }
            match result {
                Ok(Ok((health, bad_pages))) => this.apply_full_check(&health, &bad_pages, &path),
                Ok(Err(e)) => widgets::toast(&this.0.toasts, &trf!("Check not completed: {0}", e)),
                Err(_) => widgets::toast(&this.0.toasts, tr("Check crashed")),
            }
        });
    }

    /// A page row (remembered; a damaged one is marked right away).
    fn page_row(&self, r: &VnmfReader, i: usize, structured: bool) -> adw::ActionRow {
        let p = &r.pages()[i];
        let count = p.chunk_count.get() as usize;
        let bytes: u64 = r.chunks().get(p.chunk_range()).unwrap_or_default().iter().map(|c| u64::from(c.length())).sum();
        let format = if is_mixed(p) {
            chunk_formats(r, i).iter().map(ToString::to_string).collect::<Vec<_>>().join("+")
        } else {
            PageFormat::from(p.format).to_string()
        };
        let mut parts = vec![format!("{}×{}", p.width.get(), p.height.get()), format, role_name(p)];
        // In the contents the page already sits in its chapter — the chapter number is needed only without contents.
        if structured {
            parts.push(trf!("p. {0} in chapter", p.page_number_in_chapter.get()));
        } else {
            parts.push(trf!("ch. {0} · p. {1}", p.chapter_id.get(), p.page_number_in_chapter.get()));
        }
        parts.push(human_size(bytes));
        if count > 1 {
            parts.push(ntrf!("{0} fragment", "{0} fragments", count, count));
        }
        let names = PageBitFlags::names(p.flags.get());
        if !names.is_empty() {
            parts.push(names.join(", "));
        }
        let aux: Vec<String> = r.page_aux(i).iter().map(|a| a.kind().label().to_lowercase()).collect();
        if !aux.is_empty() {
            parts.push(trf!("attachments: {0}", aux.join(", ")));
        }
        let row = adw::ActionRow::builder().title(trf!("Page {0}", i + 1)).subtitle(parts.join(" · ")).activatable(true).build();
        let this = self.clone();
        row.connect_activated(move |_| this.show(Shown::Page(i)));
        if self.0.bad_pages.borrow().contains(&i) {
            mark_bad(&row);
        }
        if let Some(slot) = self.0.page_rows.borrow_mut().get_mut(i) {
            *slot = Some(row.clone());
        }
        row
    }

    fn apply_full_check(&self, health: &Health, bad_pages: &[usize], path: &Path) {
        health_ui::update_banner(&self.0.banner, health, path);
        if let Some((row, prefix)) = self.0.integrity_row.borrow_mut().as_mut() {
            row.remove(&*prefix);
            *prefix = integrity_icon(health).upcast();
            row.add_prefix(&*prefix);
            set_integrity_text(row, health);
        }
        *self.0.bad_pages.borrow_mut() = bad_pages.to_vec();
        for &i in bad_pages {
            if let Some(Some(row)) = self.0.page_rows.borrow().get(i) {
                mark_bad(row);
            }
        }
        for (expander, pages) in self.0.chapter_rows.borrow().iter() {
            let bad = bad_pages.iter().filter(|p| pages.contains(p)).count();
            if bad > 0 {
                let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
                icon.add_css_class("error");
                icon.set_tooltip_text(Some(&trf!("Damaged pages: {0}", bad)));
                expander.add_suffix(&icon);
                // Expand so the damaged page is visible right away.
                expander.set_expanded(true);
            }
        }
    }

    fn show_unreadable(&self, path: &Path, error: &str) {
        let bytes = std::fs::read(path).unwrap_or_default();
        let copies = recovery::inspect_headers(&bytes);
        let state = |ok: bool, tables_ok: bool| match (ok, tables_ok) {
            (false, _) => tr("header damaged"),
            (true, false) => tr("header intact, tables damaged"),
            (true, true) => tr("intact"),
        };
        let primary = state(copies.primary.is_ok(), copies.primary_tables.is_ok());
        let tail = match &copies.tail {
            None => tr("none"),
            Some(t) => state(t.is_ok(), matches!(copies.tail_tables, Some(Ok(())))),
        };
        let command = health_ui::recovery_command(path);
        let description = trf!(
            "{0}\n\nPrimary metadata copy: {1}\nBackup metadata copy: {2}\n\nRun recovery in a terminal:\n<tt>{3}</tt>",
            error,
            primary,
            tail,
            glib::markup_escape_text(&command)
        );
        let copy = gtk::Button::builder().label(tr("Copy command")).halign(gtk::Align::Center).build();
        copy.add_css_class("pill");
        copy.add_css_class("suggested-action");
        let toasts = self.0.toasts.clone();
        copy.connect_clicked(move |b| {
            b.clipboard().set_text(&command);
            widgets::toast(&toasts, tr("Command copied"));
        });
        let page =
            adw::StatusPage::builder().icon_name("dialog-warning-symbolic").title(tr("The file can't be opened")).description(description).child(&copy).build();
        page.add_css_class("compact");

        if let Some(old) = self.0.stack.child_by_name("error") {
            self.0.stack.remove(&old);
        }
        self.0.stack.add_named(&page, Some("error"));
        self.0.stack.set_visible_child_name("error");
    }

    // ------------------------------------------------------------------
    // Properties
    // ------------------------------------------------------------------

    fn build_details(&self, r: &Rc<VnmfReader>, path: &Path) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        // Status
        let state = adw::PreferencesGroup::builder().title(tr("Integrity")).build();
        let integrity = adw::ActionRow::builder().title(tr("Checking…")).subtitle(tr("metadata, data and redundancy")).build();
        let spinner = adw::Spinner::new();
        integrity.add_prefix(&spinner);
        state.add(&integrity);
        *self.0.integrity_row.borrow_mut() = Some((integrity, spinner.upcast()));
        page.add(&state);

        // File
        let h = r.header();
        let flags = h.flags.get();
        let (major, minor) = r.version();
        let data_bytes: u64 =
            r.chunks().iter().map(|c| u64::from(c.length())).sum::<u64>() + r.previews().iter().map(|p| u64::from(p.length.get())).sum::<u64>();
        let file = adw::PreferencesGroup::builder().title(tr("File")).build();
        file.add(&widgets::property_row(tr("Path"), &path.display().to_string()));
        file.add(&widgets::property_row(tr("Size"), &human_size(file_size)));
        file.add(&widgets::property_row(
            tr("Format"),
            &if r.legacy_version().is_some() {
                trf!("VNMF {0}.{1} — old format, read-only (rebuild in VNMF Creator)", major, minor)
            } else {
                format!("VNMF {major}.{minor}")
            },
        ));
        if !r.aux().is_empty() {
            let mut kinds: BTreeMap<vnmf_core::AuxKind, usize> = BTreeMap::new();
            for a in r.aux() {
                *kinds.entry(a.kind()).or_default() += 1;
            }
            let list: Vec<String> = kinds.iter().map(|(k, n)| format!("{} ×{n}", k.label().to_lowercase())).collect();
            file.add(&widgets::property_row(tr("Page attachments"), &list.join(", ")));
        }
        file.add(&widgets::property_row("UUID", &vnmf::format_uuid(r.uuid())));
        file.add(&widgets::property_row(tr("Block size"), &r.block_size().to_string()));
        file.add(&widgets::property_row(tr("Flags"), &format!("{flags:#06x} {}", HeaderFlagsFormatter(flags))));
        file.add(&widgets::property_row(tr("Metadata backup copy"), if r.has_flag(HeaderBitEnum::HasTailDup) { tr("yes") } else { tr("no") }));
        file.add(&widgets::property_row(
            tr("Redundancy"),
            &match r.parity_layout() {
                Some(l) => trf!(
                    "{0}% Reed–Solomon: {1} of parity, recovers contiguous damage of up to {2}",
                    l.redundancy,
                    human_size(l.len()),
                    human_size(l.max_burst_bytes())
                ),
                None => tr("none").into(),
            },
        ));
        file.add(&widgets::property_row(
            tr("Data"),
            &trf!(
                "{0} in {1} chunks · overhead {2} ({3}%)",
                human_size(data_bytes),
                r.chunks_count(),
                human_size(file_size.saturating_sub(data_bytes)),
                format!("{:.1}", 100.0 * file_size.saturating_sub(data_bytes) as f64 / file_size.max(1) as f64)
            ),
        ));
        page.add(&file);

        // Info
        page.add(&info_group(r));

        // Previews
        if r.previews_count() > 0 {
            let group = adw::PreferencesGroup::builder().title(trf!("Previews ({0})", r.previews_count())).build();
            for (i, p) in r.previews().iter().enumerate() {
                let row = adw::ActionRow::builder()
                    .title(trf!("Preview #{0} · volume {1}", i, p.volume_id.get()))
                    .subtitle(format!("{}×{} · {} · {}", p.width.get(), p.height.get(), PageFormat::from(p.format), human_size(u64::from(p.length.get()))))
                    .activatable(true)
                    .build();
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let this = self.clone();
                row.connect_activated(move |_| this.show(Shown::Preview(i)));
                group.add(&row);
            }
            page.add(&group);
        }

        // Pages
        let mut formats: BTreeMap<String, usize> = BTreeMap::new();
        for p in r.pages() {
            *formats.entry(PageFormat::from(p.format).to_string()).or_default() += 1;
        }
        let summary: Vec<String> = formats.iter().map(|(f, n)| format!("{f} ×{n}")).collect();
        let toc = Toc::from_reader(r);
        *self.0.page_rows.borrow_mut() = vec![None; r.pages_count()];
        self.0.bad_pages.borrow_mut().clear();
        let structured = toc.is_structured();

        let pages_word = |range: &std::ops::Range<usize>| {
            if range.len() == 1 {
                trf!("p. {0}", range.start + 1)
            } else {
                ntrf!("pp. {0}–{1} · {2} page", "pp. {0}–{1} · {2} pages", range.len(), range.start + 1, range.end, range.len())
            }
        };
        let mut chapter_rows = Vec::new();
        if toc.is_structured() {
            // Contents: a group per volume, a chapter — an expander row with pages.
            let volumes = toc.volumes.iter().filter(|v| v.volume != 0).count();
            let structure =
                if volumes > 0 { trf!("volumes {0}, chapters {1}", volumes, toc.chapters_count()) } else { trf!("chapters {0}", toc.chapters_count()) };
            let head = adw::PreferencesGroup::builder()
                .title(trf!("Pages ({0})", r.pages_count()))
                .description(format!("{} · {structure}", summary.join(", ")))
                .build();
            page.add(&head);
            for volume in &toc.volumes {
                let group = adw::PreferencesGroup::new();
                if toc.has_volumes {
                    group.set_title(&glib::markup_escape_text(&volume.label()));
                    group.set_description(Some(&pages_word(&volume.pages)));
                }
                for chapter in &volume.chapters {
                    let expander = adw::ExpanderRow::builder().title(glib::markup_escape_text(&chapter.label())).subtitle(pages_word(&chapter.pages)).build();
                    // A chapter's pages — only once it's expanded (contents of thousands of pages).
                    let (this, reader, range) = (self.clone(), r.clone(), chapter.pages.clone());
                    widgets::fill_on_expand(&expander, move |expander| {
                        for i in range.clone() {
                            expander.add_row(&this.page_row(&reader, i, true));
                        }
                    });
                    group.add(&expander);
                    chapter_rows.push((expander, chapter.pages.clone()));
                }
                page.add(&group);
            }
        } else {
            let pages = adw::PreferencesGroup::builder().title(trf!("Pages ({0})", r.pages_count())).description(summary.join(", ")).build();
            for i in 0..r.pages_count() {
                pages.add(&self.page_row(r, i, structured));
            }
            page.add(&pages);
        }
        *self.0.chapter_rows.borrow_mut() = chapter_rows;
        page
    }

    // ------------------------------------------------------------------
    // Viewer panel
    // ------------------------------------------------------------------

    fn show(&self, what: Shown) {
        let Some(reader) = self.0.reader.borrow().clone() else { return };

        let (chunks, caption): (Vec<Vec<u8>>, String) = match what {
            Shown::Preview(i) => {
                let rec = reader.previews()[i];
                let data = reader.get_preview_data(i).map(<[u8]>::to_vec).unwrap_or_default();
                let caption = trf!("Preview #{0} · {1}×{2} · {3}", i, rec.width.get(), rec.height.get(), PageFormat::from(rec.format));
                (vec![data], caption)
            }
            Shown::Page(i) => {
                let rec = reader.pages()[i];
                let data = reader.get_page_chunks_data(i).map(|v| v.iter().map(|c| c.to_vec()).collect()).unwrap_or_default();
                let mut caption = trf!("Page {0} of {1} · {2}×{3} · {4}", i + 1, reader.pages_count(), rec.width.get(), rec.height.get(), role_name(&rec));
                let toc = Toc::from_reader(&reader);
                if toc.is_structured()
                    && let Some(position) = toc.position_label(i)
                {
                    caption = format!("{position}\n{caption}");
                }
                (data, caption)
            }
        };
        self.0.preview_caption.set_label(&caption);

        // A single fragment fits into the panel; several (webtoon) — a scrollable strip.
        let pictures: Vec<gtk::Picture> = if chunks.len() == 1 {
            let (w, h) = vnmf_core::image_dimensions(&chunks[0]).unwrap_or((1, 1));
            let picture = gtk::Picture::for_paintable(&imaging::placeholder(w, h));
            picture.set_can_shrink(true);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_vexpand(true);
            self.0.preview_area.set_child(Some(&picture));
            vec![picture]
        } else {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let pictures = chunks
                .iter()
                .map(|data| {
                    let (w, h) = vnmf_core::image_dimensions(data).unwrap_or((1, 1));
                    let strip = AspectPicture::new();
                    strip.set_dimensions(w, h);
                    column.append(&strip);
                    strip.picture().clone()
                })
                .collect();
            let scrolled = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).vexpand(true).build();
            self.0.preview_area.set_child(Some(&scrolled));
            pictures
        };

        for (picture, data) in pictures.into_iter().zip(chunks) {
            glib::spawn_future_local(async move {
                match imaging::decode_scaled(data, PREVIEW_WIDTH).await {
                    Ok(texture) => picture.set_paintable(Some(&texture)),
                    Err(e) => {
                        picture.set_paintable(gtk::gdk::Paintable::NONE);
                        picture.set_tooltip_text(Some(&trf!("Could not display: {0}", e)));
                    }
                }
            });
        }
    }
}

fn integrity_icon(health: &Health) -> gtk::Image {
    let (icon, css) = if health.is_ok() { ("object-select-symbolic", "success") } else { ("dialog-warning-symbolic", "error") };
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class(css);
    image
}

fn set_integrity_text(row: &adw::ActionRow, health: &Health) {
    if health.is_ok() {
        row.set_title(tr("File is intact"));
        row.set_subtitle(tr("metadata, data and redundancy checked"));
    } else {
        row.set_title(&ntrf!("Damaged: {0} problem", "Damaged: {0} problems", health.issues.len(), health.issues.len()));
        row.set_subtitle(&trf!("{0}\nPrognosis: {1}", health.issues.join("\n"), health.prognosis));
    }
}

/// The "Metadata" group: ComicInfo fields in plain language + raw contents.
fn info_group(r: &VnmfReader) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("Metadata")).build();
    let Some(info) = r.info_bytes() else {
        group.add(&adw::ActionRow::builder().title(tr("None")).build());
        return group;
    };
    let format = InfoFormat::detect(info);
    let meta = Metadata::from_info(Some(info));
    let current = serde_json::from_slice::<Value>(info).ok().and_then(|v| v.get("schema")?.as_str().map(|s| s.starts_with("vnmf-metadata/"))).unwrap_or(false);
    let kind = match (&meta, current) {
        (Some(_), true) => trf!("VNMF metadata ({0})", metadata::SCHEMA),
        (Some(_), false) => tr("ComicInfo (format 1.1) — shown as VNMF metadata").to_string(),
        (None, _) => format.to_string(),
    };
    group.set_description(Some(&format!("{kind} · {}", human_size(info.len() as u64))));

    if let Some(meta) = &meta {
        // Several values with the same label (people, catalogs, links) — on one line.
        let mut merged: Vec<(&str, String)> = Vec::new();
        for (label, value) in meta.display_rows() {
            match merged.last_mut() {
                Some((last, text)) if *last == label => {
                    text.push('\n');
                    text.push_str(&value);
                }
                _ => merged.push((label, value)),
            }
        }
        for (label, value) in merged {
            let row = widgets::property_row(label, &value);
            row.set_subtitle_lines(0);
            group.add(&row);
        }
    }

    let raw = adw::ExpanderRow::builder().title(tr("Raw contents")).subtitle(format.to_string()).build();
    let text = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(12)
        .right_margin(12)
        .build();
    text.buffer().set_text(&String::from_utf8_lossy(info));
    raw.add_row(&text);
    group.add(&raw);
    group
}

/// Damaged page marker.
fn mark_bad(row: &adw::ActionRow) {
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    icon.add_css_class("error");
    icon.set_tooltip_text(Some(tr("Page data is damaged (CRC mismatch)")));
    row.add_suffix(&icon);
    row.add_css_class("error");
}
