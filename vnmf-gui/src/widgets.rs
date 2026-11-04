// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Small widgets and dialogs shared by all applications.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use vnmf_i18n::{tr, trf};

/// `.vnmf` file filter for dialogs.
fn vnmf_filters() -> gio::ListStore {
    let vnmf = gtk::FileFilter::new();
    vnmf.set_name(Some(tr("VNMF files")));
    vnmf.add_pattern("*.vnmf");
    let all = gtk::FileFilter::new();
    all.set_name(Some(tr("All files")));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&vnmf);
    store.append(&all);
    store
}

/// Dialog for opening a `.vnmf`; `None` if the user cancelled.
pub async fn choose_vnmf(parent: &impl IsA<gtk::Window>) -> Option<PathBuf> {
    let dialog = gtk::FileDialog::builder().title(tr("Open VNMF file")).modal(true).filters(&vnmf_filters()).build();
    dialog.open_future(Some(parent)).await.ok()?.path()
}

/// File drop target: calls `on_files` with the paths of all dropped files.
pub fn file_drop_target(on_files: impl Fn(Vec<PathBuf>) + 'static) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    target.connect_drop(move |_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        if paths.is_empty() {
            return false;
        }
        on_files(paths);
        true
    });
    target
}

/// "⋮" menu button with items (label, action).
pub fn menu_button(items: &[(&str, &str)]) -> gtk::MenuButton {
    let menu = gio::Menu::new();
    for (label, action) in items {
        menu.append(Some(label), Some(action));
    }
    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text(tr("Main menu"))
        .build()
}

/// The `app.about` action with an "About" dialog.
pub fn install_about(window: &adw::ApplicationWindow, name: &str, comments: &str) {
    let action = gio::SimpleAction::new("about", None);
    let (window_weak, name, comments) = (window.downgrade(), name.to_string(), comments.to_string());
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else { return };
        let about = adw::AboutDialog::builder()
            .application_name(&name)
            .application_icon("image-x-generic")
            .version(crate::VERSION)
            .comments(&comments)
            .developer_name("void_nicole (Nicole Hitori)")
            .developers(["void_nicole (Nicole Hitori)"])
            .copyright("© 2026 void_nicole (Nicole Hitori) + AI-assist")
            // GPL version 3 "or later" (in GTK that's exactly License::Gpl30; Gpl30Only means "3 only").
            .license_type(gtk::License::Gpl30)
            .build();
        about.add_credit_section(Some(tr("Development assistance")), &["AI-assist"]);
        about.add_legal_section(
            tr("VNMF format and the vnmf-core library"),
            Some("© 2026 void_nicole (Nicole Hitori) + AI-assist"),
            gtk::License::Gpl30,
            None,
        );
        about.present(Some(&window));
    });
    window.add_action(&action);
}

/// How an application applies a new interface language (it only takes effect on the next start).
#[derive(Clone)]
pub enum LanguageRestart {
    /// Offer to restart right away (the toast's "Restart" button): for applications that lose
    /// nothing by it — the viewer reopens the same file.
    Offer(std::rc::Rc<dyn Fn()>),
    /// Only say it applies on the next start: a restart would throw away work in progress (a
    /// Creator project, the Guard file list).
    NextStart,
}

/// "Language": the system one, English or any installed translation. Shared by all VNMF
/// applications (`~/.config/vnmf/language`); toasts go to `toasts` (e.g. the dialog the row is in).
pub fn language_row(toasts: &impl IsA<adw::PreferencesDialog>, restart: LanguageRestart) -> adw::ComboRow {
    let chosen = vnmf_i18n::language();
    let mut codes: Vec<Option<String>> = std::iter::once(None).chain(vnmf_i18n::available().into_iter().map(Some)).collect();
    // A chosen language whose catalog has since been removed stays visible as the current choice.
    if chosen.is_some() && !codes.contains(&chosen) {
        codes.push(chosen.clone());
    }
    let labels: Vec<String> = codes
        .iter()
        .map(|c| c.as_deref().map_or_else(|| tr("System language").to_owned(), vnmf_i18n::language_name))
        .collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(tr("Language"))
        .subtitle(tr("For all VNMF applications; applies after a restart"))
        .model(&gtk::StringList::new(&labels))
        .selected(codes.iter().position(|c| *c == chosen).unwrap_or(0) as u32)
        .build();
    let dialog = toasts.as_ref().downgrade();
    row.connect_selected_notify(move |row| {
        let code = codes.get(row.selected() as usize).cloned().flatten();
        let Some(dialog) = dialog.upgrade() else { return };
        dialog.add_toast(language_toast(vnmf_i18n::set_language(code.as_deref()), &restart));
    });
    row
}

fn language_toast(saved: std::io::Result<()>, restart: &LanguageRestart) -> adw::Toast {
    if let Err(e) = saved {
        return adw::Toast::new(&trf!("Could not save the language: {0}", e));
    }
    match restart {
        LanguageRestart::Offer(restart) => {
            let toast = adw::Toast::builder()
                .title(tr("The language will change after a restart"))
                .button_label(tr("Restart"))
                .timeout(0)
                .build();
            let restart = restart.clone();
            toast.connect_button_clicked(move |_| restart());
            toast
        }
        LanguageRestart::NextStart => adw::Toast::builder()
            .title(tr("The language will change the next time the application starts"))
            .timeout(0)
            .build(),
    }
}

/// The `win.language` action: a small "Language" dialog with [`language_row`] — for applications
/// without their own settings dialog (put "Language…" into the main menu).
pub fn install_language(window: &adw::ApplicationWindow, restart: LanguageRestart) {
    let action = gio::SimpleAction::new("language", None);
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else { return };
        let dialog = adw::PreferencesDialog::builder().title(tr("Language")).content_width(480).content_height(210).build();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .description(tr("The language of the VNMF applications. Translations are installed with them; the system language is used by default."))
            .build();
        group.add(&language_row(&dialog, restart.clone()));
        page.add(&group);
        dialog.add(&page);
        dialog.present(Some(&window));
    });
    window.add_action(&action);
}

/// A "name — value" row for property pages; the value can be selected and copied.
/// Values come from files (titles like "Pepper&Carrot"), so they are plain text, not Pango markup.
pub fn property_row(title: &str, value: &str) -> adw::ActionRow {
    // Markup off before the text is set: construct properties have no guaranteed order.
    let row = adw::ActionRow::builder().use_markup(false).subtitle_selectable(true).build();
    row.set_title(title);
    row.set_subtitle(value);
    row.add_css_class("property");
    row
}

/// "Open a file" empty state with a button and a drag-and-drop hint.
pub fn open_status_page(icon: &str, title: &str, description: &str) -> (adw::StatusPage, gtk::Button) {
    let button = gtk::Button::builder().label(tr("Open…")).halign(gtk::Align::Center).build();
    button.add_css_class("pill");
    button.add_css_class("suggested-action");
    let page = adw::StatusPage::builder().icon_name(icon).title(title).description(description).child(&button).build();
    (page, button)
}

/// Shows a message in the toast overlay.
pub fn toast(overlay: &adw::ToastOverlay, text: &str) {
    overlay.add_toast(adw::Toast::builder().title(glib::markup_escape_text(text)).timeout(4).build());
}

/// Fills an expander row only on first expansion.
///
/// A table of contents with hundreds of chapters and thousands of pages would otherwise build thousands of rows at once
/// (hundreds of milliseconds and memory), although the user expands one or two chapters.
pub fn fill_on_expand(expander: &adw::ExpanderRow, fill: impl Fn(&adw::ExpanderRow) + 'static) {
    let filled = std::cell::Cell::new(false);
    let run = move |row: &adw::ExpanderRow| {
        if row.is_expanded() && !filled.replace(true) {
            fill(row);
        }
    };
    run(expander);
    expander.connect_expanded_notify(run);
}
