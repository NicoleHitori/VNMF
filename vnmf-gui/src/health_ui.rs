// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Showing file integrity status: a "file is damaged" banner and a dialog with advice.
//!
//! The assessment itself is `vnmf::health` (the same as in the `vnmfinfo` CLI), so the GUI and CLI always
//! say the same thing.

use std::path::Path;

use adw::prelude::*;
use vnmf::health::Health;
use vnmf_i18n::{ntrf, tr, trf};

/// Recovery command for a specific file.
pub fn recovery_command(path: &Path) -> String {
    let p = path.display().to_string();
    if p.chars().all(|c| c.is_alphanumeric() || "/._-+".contains(c)) {
        format!("vnmfrecovery {p}")
    } else {
        format!("vnmfrecovery '{}'", p.replace('\'', "'\\''"))
    }
}

/// Banner below the window title; hidden while there are no problems.
pub fn banner() -> adw::Banner {
    let banner = adw::Banner::new("");
    banner.set_button_label(Some(tr("What to do?")));
    banner.set_revealed(false);
    banner
}

/// Updates the banner from the check result. The button opens [`show_details`].
pub fn update_banner(banner: &adw::Banner, health: &Health, path: &Path) {
    if health.is_ok() {
        banner.set_revealed(false);
        return;
    }
    banner.set_title(&ntrf!("File is damaged: {0} problem. {1}", "File is damaged: {0} problems. {1}", health.issues.len(), health.issues.len(), capitalize(health.prognosis)));
    banner.set_revealed(true);

    // Reconnect the handler for the current state.
    let (issues, prognosis, path) = (health.issues.clone(), health.prognosis, path.to_path_buf());
    // SAFETY: the data key is used only here and always with the SignalHandlerId type.
    if let Some(old) = unsafe { banner.steal_data::<gtk::glib::SignalHandlerId>("vnmf-handler") } {
        banner.disconnect(old);
    }
    let id = banner.connect_button_clicked(move |banner| {
        if let Some(window) = banner.root().and_downcast::<gtk::Window>() {
            show_details(&window, &issues, prognosis, &path);
        }
    });
    unsafe { banner.set_data("vnmf-handler", id) };
}

/// Dialog with the list of problems, the prognosis and the recovery command (with a copy button).
pub fn show_details(parent: &impl IsA<gtk::Widget>, issues: &[String], prognosis: &str, path: &Path) {
    let command = recovery_command(path);
    let body = trf!("{0}\n\nPrognosis: {1}.\n\nRun recovery in a terminal:\n{2}", issues.iter().map(|i| format!("• {i}")).collect::<Vec<_>>().join("\n"), prognosis, command);
    let dialog = adw::AlertDialog::new(Some(tr("File is damaged")), Some(&body));
    dialog.add_response("close", tr("Close"));
    dialog.add_response("copy", tr("Copy command"));
    dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("copy"));
    dialog.set_close_response("close");
    let parent_widget = parent.as_ref().clone();
    dialog.connect_response(None, move |_, response| {
        if response == "copy" {
            parent_widget.clipboard().set_text(&command);
        }
    });
    dialog.present(Some(parent));
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}
