// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! The "Reading settings" dialog (Ctrl+,). Changes apply immediately and are saved.

use adw::prelude::*;

use crate::ReaderWindow;
use crate::settings::{Background, STRIP_WIDTH_MAX, STRIP_WIDTH_MIN, Settings};
use vnmf_gui::widgets;
use vnmf_i18n::tr;

/// A switch row bound to a boolean settings field.
fn switch(window: &ReaderWindow, title: &str, subtitle: &str, get: fn(&Settings) -> bool, set: fn(&mut Settings, bool)) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).active(get(&window.0.settings.borrow())).build();
    let window = window.clone();
    row.connect_active_notify(move |row| {
        set(&mut window.0.settings.borrow_mut(), row.is_active());
        window.settings_changed();
    });
    row
}

/// A spin row for an integer settings field.
fn spin(window: &ReaderWindow, title: &str, subtitle: &str, range: (f64, f64, f64), get: fn(&Settings) -> i32, set: fn(&mut Settings, i32)) -> adw::SpinRow {
    let adjustment = gtk::Adjustment::new(f64::from(get(&window.0.settings.borrow())), range.0, range.1, range.2, range.2 * 5.0, 0.0);
    let row = adw::SpinRow::builder().title(title).subtitle(subtitle).adjustment(&adjustment).build();
    let window = window.clone();
    row.connect_value_notify(move |row| {
        set(&mut window.0.settings.borrow_mut(), row.value() as i32);
        window.settings_changed();
    });
    row
}

pub fn show(window: &ReaderWindow) {
    let dialog = adw::PreferencesDialog::builder().title(tr("Reading settings")).search_enabled(true).build();
    let page = adw::PreferencesPage::builder().title(tr("Reading")).icon_name("x-office-document-symbolic").build();

    page.add(&language_group(window, &dialog));

    // View
    let view = adw::PreferencesGroup::builder().title(tr("View")).build();
    let labels: Vec<&str> = Background::ALL.iter().map(|b| b.label()).collect();
    let background = adw::ComboRow::builder()
        .title(tr("Background"))
        .subtitle(tr("Color around the page"))
        .model(&gtk::StringList::new(&labels))
        .selected(Background::ALL.iter().position(|b| *b == window.0.settings.borrow().background).unwrap_or(0) as u32)
        .build();
    let w = window.clone();
    background.connect_selected_notify(move |row| {
        w.0.settings.borrow_mut().background = Background::ALL[row.selected() as usize % Background::ALL.len()];
        w.settings_changed();
    });
    view.add(&background);
    view.add(&switch(
        window,
        tr("Turn pages by clicking the edges"),
        tr("Click in the left half of the window — left, in the right half — right"),
        |s| s.click_zones,
        |s, v| s.click_zones = v,
    ));
    view.add(&switch(
        window,
        tr("Hide the navigation bar"),
        tr("The floating bar disappears while the mouse is still"),
        |s| s.autohide_bar,
        |s, v| s.autohide_bar = v,
    ));
    view.add(&switch(
        window,
        tr("Hide the header in fullscreen"),
        tr("Appears when the mouse is near the top edge"),
        |s| s.hide_header_fullscreen,
        |s, v| s.hide_header_fullscreen = v,
    ));
    view.add(&spin(
        window,
        tr("Preloading"),
        tr("How many pages to prepare ahead of the current one and as many behind it"),
        (0.0, 6.0, 1.0),
        |s| s.preload as i32,
        |s, v| s.preload = v.max(0) as u32,
    ));
    page.add(&view);

    // Manga
    let manga = adw::PreferencesGroup::builder().title(tr("Manga")).description(tr("Paged mode")).build();
    let double = switch(window, tr("Spread"), tr("Two pages side by side; wide pages shown alone (D key)"), |s| s.double_page, |s, v| s.double_page = v);
    // The "Spread" menu item has the same state.
    let w = window.clone();
    double.connect_active_notify(move |row| {
        if let Some(action) = w.0.window.lookup_action("double").and_downcast::<gtk::gio::SimpleAction>() {
            action.set_state(&row.is_active().to_variant());
        }
    });
    manga.add(&double);
    manga.add(&switch(window, tr("Cover alone"), tr("In a spread the first page is shown by itself"), |s| s.cover_alone, |s, v| s.cover_alone = v));
    page.add(&manga);

    // Webtoon
    let strip = adw::PreferencesGroup::builder().title(tr("Webtoon")).description(tr("Strip mode")).build();
    strip.add(&spin(
        window,
        tr("Strip width, px"),
        tr("Also changed by zooming in the strip (Ctrl+scroll); 100% — full window width"),
        (f64::from(STRIP_WIDTH_MIN), f64::from(STRIP_WIDTH_MAX), 50.0),
        |s| s.strip_width,
        |s, v| s.strip_width = v,
    ));
    strip.add(&spin(window, tr("Gap between fragments, px"), tr("0 — fragments join seamlessly"), (0.0, 64.0, 2.0), |s| s.strip_gap, |s, v| s.strip_gap = v));
    page.add(&strip);

    // Page attachments
    let extras = adw::PreferencesGroup::builder()
        .title(tr("Sound and page attachments"))
        .description(tr("If the file contains them: music, sound effects, text and dark versions of pages"))
        .build();
    let sound = switch(window, tr("Sound"), tr("Page music and effects (M)"), |s| s.sound, |s, v| s.sound = v);
    let w = window.clone();
    sound.connect_active_notify(move |row| {
        if let Some(action) = w.0.window.lookup_action("sound").and_downcast::<gtk::gio::SimpleAction>() {
            action.set_state(&row.is_active().to_variant());
        }
        w.sound_changed();
    });
    extras.add(&sound);
    let volume = spin(window, tr("Volume, %"), tr("Shared by music and effects"), (0.0, 100.0, 5.0), |s| s.volume, |s, v| s.volume = v);
    let w = window.clone();
    volume.connect_value_notify(move |_| w.sound_changed());
    extras.add(&volume);
    let dark = switch(window, tr("Dark page versions"), tr("Used instead of the regular ones in a dark theme"), |s| s.dark_pages, |s, v| s.dark_pages = v);
    let w = window.clone();
    dark.connect_active_notify(move |_| w.dark_changed());
    extras.add(&dark);
    page.add(&extras);

    // Position
    let position = adw::PreferencesGroup::builder().title(tr("Reading position")).build();
    position.add(&switch(
        window,
        tr("Remember the page"),
        tr("Open each file where you left off (by file UUID)"),
        |s| s.remember_position,
        |s, v| s.remember_position = v,
    ));
    let forget = adw::ButtonRow::builder().title(tr("Forget all saved positions")).build();
    forget.add_css_class("destructive-action");
    let w = window.clone();
    forget.connect_activated(move |row| {
        w.0.settings.borrow_mut().positions.clear();
        w.flush_settings();
        row.set_sensitive(false);
        row.set_title(tr("Positions forgotten"));
    });
    position.add(&forget);
    page.add(&position);

    dialog.add(&page);
    dialog.present(Some(&window.0.window));
}

/// "Interface language" (shared by all VNMF applications); the toast offers to restart the reader
/// right away, on the same file and page.
fn language_group(window: &ReaderWindow, dialog: &adw::PreferencesDialog) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("Interface")).build();
    let w = window.clone();
    group.add(&widgets::language_row(dialog, widgets::LanguageRestart::Offer(std::rc::Rc::new(move || w.restart()))));
    group
}
