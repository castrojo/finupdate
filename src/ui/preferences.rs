//! Preferences dialog for finupdate.
//!
//! Built as a standalone function following the `show_shortcuts_window` pattern
//! in `app.rs` — no full relm4 component needed since it's a one-shot modal.
//!
//! Settings are saved directly from signal handlers via `Rc<RefCell<Settings>>`
//! shared across all closures. This ensures each closure always reads the latest
//! values written by other closures, preventing stale-clone overwrite bugs.
//!
//! When the dialog closes, `on_change` is called with the final `Settings` value
//! so the parent `App` component can update its in-memory copy without requiring
//! the user to restart.

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use adw::prelude::*;

use crate::settings::{Settings, UpdateInterval};

/// Build and present the preferences dialog attached to `parent`.
///
/// `on_change` is called when the dialog closes, passing the final settings
/// snapshot so the caller can update its own state.
pub fn show_preferences(
    parent: &adw::ApplicationWindow,
    mut settings: Settings,
    on_change: impl Fn(Settings) + 'static,
) {
    if let Some(auto_updates) = read_uupd_timer_enabled() {
        settings.auto_updates = auto_updates;
    }

    let shared = Rc::new(RefCell::new(settings));

    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");
    dialog.set_search_enabled(false);

    // ── Page ─────────────────────────────────────────────────────────────
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();

    build_updates_group(&page, &shared);
    build_network_group(&page, &shared);
    build_developer_group(&page, &shared);

    dialog.add(&page);

    // Notify the caller when the dialog is dismissed so in-memory settings stay current.
    let shared_close = shared.clone();
    dialog.connect_closed(move |_| {
        on_change(shared_close.borrow().clone());
    });

    dialog.present(Some(parent));
}

// ── Updates group ─────────────────────────────────────────────────────────────

fn is_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

fn read_uupd_timer_enabled() -> Option<bool> {
    let output = if is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "systemctl", "is-enabled", "uupd.timer"])
            .output()
    } else {
        Command::new("systemctl")
            .arg("is-enabled")
            .arg("uupd.timer")
            .output()
    };

    match output {
        Ok(output) => match String::from_utf8_lossy(&output.stdout).trim() {
            "enabled" => Some(true),
            "disabled" => Some(false),
            _ => None,
        },
        Err(err) => {
            tracing::warn!("Failed to read uupd.timer state: {}", err);
            None
        }
    }
}

fn build_updates_group(page: &adw::PreferencesPage, shared: &Rc<RefCell<Settings>>) {
    let group = adw::PreferencesGroup::builder()
        .title("Automatic Updates")
        .description("Configure how and when finupdate checks for updates")
        .build();

    // Auto-update toggle
    let auto_row = {
        let s = shared.borrow();
        adw::SwitchRow::builder()
            .title("Enable Automatic Updates")
            .subtitle("Allow uupd to run on its daily systemd timer")
            .active(s.auto_updates)
            .build()
    };

    {
        let shared = shared.clone();
        auto_row.connect_active_notify(move |row| {
            let active = row.is_active();
            shared.borrow_mut().auto_updates = active;
            shared.borrow().save();

            std::thread::spawn(move || {
                let (action, args) = if active {
                    ("enable", ["enable", "--now", "uupd.timer"])
                } else {
                    ("disable", ["disable", "--now", "uupd.timer"])
                };

                let status = if is_flatpak() {
                    Command::new("flatpak-spawn")
                        .args(["--host", "pkexec", "systemctl"])
                        .args(args)
                        .status()
                } else {
                    Command::new("pkexec").arg("systemctl").args(args).status()
                };

                match status {
                    Ok(status) if status.success() => {}
                    Ok(status) => {
                        tracing::warn!("Failed to {}: {}", action, status);
                    }
                    Err(err) => {
                        tracing::warn!("Failed to {}: {}", action, err);
                    }
                }
            });
        });
    }
    group.add(&auto_row);

    // Update interval combo
    let interval_row = {
        let s = shared.borrow();
        let row = adw::ComboRow::builder()
            .title("Check Interval")
            .subtitle("How often automatic updates run")
            .build();
        let model = gtk::StringList::new(&["Hourly", "Daily", "Weekly", "Custom"]);
        row.set_model(Some(&model));
        row.set_selected(s.update_interval.to_index());
        row
    };

    // Custom interval revealer (shown only when "Custom" is selected)
    let custom_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .reveal_child(shared.borrow().update_interval == UpdateInterval::Custom)
        .build();

    let custom_action_row = adw::ActionRow::builder()
        .title("Custom Interval")
        .subtitle("Number of hours between update checks")
        .build();

    let spin_adj = gtk::Adjustment::builder()
        .lower(1.0)
        .upper(168.0)
        .step_increment(1.0)
        .value(shared.borrow().custom_interval_hours as f64)
        .build();
    let custom_spin = gtk::SpinButton::builder()
        .adjustment(&spin_adj)
        .valign(gtk::Align::Center)
        .build();
    custom_action_row.add_suffix(&custom_spin);

    let custom_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    custom_box.append(&custom_action_row);
    custom_revealer.set_child(Some(&custom_box));

    {
        let shared = shared.clone();
        let custom_revealer = custom_revealer.clone();
        interval_row.connect_selected_notify(move |row| {
            let interval = UpdateInterval::from_index(row.selected());
            custom_revealer.set_reveal_child(interval == UpdateInterval::Custom);
            shared.borrow_mut().update_interval = interval;
            shared.borrow().save();
        });
    }
    {
        let shared = shared.clone();
        custom_spin.connect_value_changed(move |spin| {
            shared.borrow_mut().custom_interval_hours = spin.value() as u32;
            shared.borrow().save();
        });
    }

    group.add(&interval_row);
    group.add(&custom_revealer);

    page.add(&group);
}

// ── Network group ─────────────────────────────────────────────────────────────

fn build_network_group(page: &adw::PreferencesPage, shared: &Rc<RefCell<Settings>>) {
    let group = adw::PreferencesGroup::builder()
        .title("Network")
        .description(
            "Control update behavior based on connection type. \
             Manual updates always work regardless of these settings.",
        )
        .build();

    let metered_row = {
        let s = shared.borrow();
        adw::SwitchRow::builder()
            .title("Pause on Metered Connections")
            .subtitle("Suspend automatic updates on limited or cellular connections")
            .active(s.pause_on_metered)
            .build()
    };

    // Connection status badge — only visible when the toggle is on.
    let status_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .reveal_child(shared.borrow().pause_on_metered)
        .build();

    let monitor = gtk::gio::NetworkMonitor::default();
    let is_metered = monitor.is_network_metered();
    let (status_text, css_class) = if is_metered {
        (
            "⚠  Metered connection active — automatic updates are paused",
            "warning",
        )
    } else {
        (
            "✓  Unmetered connection — automatic updates will run normally",
            "success",
        )
    };

    let status_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(12)
        .margin_end(12)
        .margin_top(4)
        .margin_bottom(8)
        .build();
    let status_label = gtk::Label::new(Some(status_text));
    status_label.add_css_class("caption");
    status_label.add_css_class(css_class);
    status_label.set_halign(gtk::Align::Start);
    status_box.append(&status_label);
    status_revealer.set_child(Some(&status_box));

    {
        let shared = shared.clone();
        let status_revealer = status_revealer.clone();
        metered_row.connect_active_notify(move |row| {
            let active = row.is_active();
            status_revealer.set_reveal_child(active);
            shared.borrow_mut().pause_on_metered = active;
            shared.borrow().save();
        });
    }

    group.add(&metered_row);
    group.add(&status_revealer);

    page.add(&group);
}

// ── Developer group ───────────────────────────────────────────────────────────

fn build_developer_group(page: &adw::PreferencesPage, shared: &Rc<RefCell<Settings>>) {
    let group = adw::PreferencesGroup::builder()
        .title("Developer")
        .description(
            "Tools for UI development and testing. \
             These settings do not affect normal operation.",
        )
        .build();

    let dev_row = {
        let s = shared.borrow();
        adw::SwitchRow::builder()
            .title("Developer Mode")
            .subtitle("Simulate updates without running uupd or touching the real system")
            .active(s.dev_mode)
            .build()
    };

    {
        let shared = shared.clone();
        dev_row.connect_active_notify(move |row| {
            shared.borrow_mut().dev_mode = row.is_active();
            shared.borrow().save();
        });
    }
    group.add(&dev_row);

    page.add(&group);
}
