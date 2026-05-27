//! Status view component — the main content area of the app.
//!
//! Pattern: State-driven view switching
//! Uses a `gtk::Stack` to switch between different visual states:
//! - Idle: AdwStatusPage with "ready to update" messaging + last update time + image badge
//! - Updating: Progress indicator + image badge + UpdateList + live log + timer + cancel
//! - Complete: Success status page with reboot option
//! - UpToDate: "You're already up to date" status page
//! - Error: Error status page with retry option

use gtk::prelude::*;
use relm4::prelude::*;
use std::time::Instant;

use crate::app::AppState;
use crate::config;
use crate::ui::log_view::{LogView, LogViewInput};
use crate::ui::update_list::{UpdateList, UpdateListInput};

/// Input messages for the StatusView component.
#[derive(Debug)]
pub enum StatusViewInput {
    /// Parent tells us the app state changed.
    StateChanged(AppState),
    /// Append a log line to the output view.
    AppendLog(String),
    /// Clear the log buffer.
    ClearLog,
    /// Timer tick — update elapsed time display.
    TimerTick,
    /// Copy log to clipboard.
    CopyLog,
}

/// Output messages the StatusView sends to its parent.
#[derive(Debug)]
pub enum StatusViewOutput {
    /// User wants to trigger an update.
    StartUpdate,
    /// User wants to cancel the running update.
    CancelUpdate,
    /// User wants to reboot the system.
    Reboot,
}

/// The status view model.
pub struct StatusView {
    state: AppState,
    log_view: Controller<LogView>,
    update_list: Controller<UpdateList>,
    /// Direct reference to the root stack for page switching in update().
    stack: gtk::Stack,
    /// When the current update started (for elapsed timer).
    update_start: Option<Instant>,
    /// Label showing elapsed time during updates.
    elapsed_label: gtk::Label,
    /// Accumulated log text for clipboard copy.
    log_text: String,
    /// Toast overlay for copy confirmation.
    toast_overlay: adw::ToastOverlay,
    /// Label showing last update time on idle page.
    last_update_label: gtk::Label,
    /// Image info badge shown on the idle page.
    idle_image_box: gtk::Box,
}

#[relm4::component(pub)]
impl SimpleComponent for StatusView {
    type Init = AppState;
    type Input = StatusViewInput;
    type Output = StatusViewOutput;

    view! {
        #[root]
        gtk::Stack {
            set_transition_type: gtk::StackTransitionType::Crossfade,
            set_transition_duration: 200,

            // ─── Idle page ──────────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some(config::APP_ID),
                set_title: "System Up to Date",
                set_description: Some("Your system is managed by uupd.\nClick below to check for and install updates now."),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 12,

                    // Image info badge — shows e.g. "bluefin · dx"
                    #[local_ref]
                    idle_image_box -> gtk::Box {},

                    gtk::Button {
                        set_label: "Check for Updates",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        set_tooltip_text: Some("Check for and install system updates"),
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::StartUpdate).unwrap();
                        },
                    },

                    // Show last update time if known.
                    #[local_ref]
                    last_update_label -> gtk::Label {
                        add_css_class: "dim-label",
                        add_css_class: "caption",
                    },
                },
            } -> {
                set_name: "idle",
            },

            // ─── Updating page ──────────────────────────────────────────
            // Built imperatively in init() — the view! macro cannot inline-
            // construct children for pre-existing widget references.
            add_child = &model.toast_overlay.clone() -> adw::ToastOverlay {} -> {
                set_name: "updating",
            },

            // ─── Complete page ──────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("object-select-symbolic"),
                set_title: "Update Complete",
                set_description: Some("Your system has been updated.\nRestart to apply changes."),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 12,

                    gtk::Button {
                        set_label: "Restart…",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        set_tooltip_text: Some("Restart the system to apply updates"),
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::Reboot).unwrap();
                        },
                    },

                    gtk::Button {
                        set_label: "Done",
                        add_css_class: "pill",
                        set_tooltip_text: Some("Dismiss and return to idle"),
                        connect_clicked[sender] => move |_| {
                            sender.input(StatusViewInput::StateChanged(AppState::Idle));
                        },
                    },
                },
            } -> {
                set_name: "complete",
            },

            // ─── Up to date page ────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("object-select-symbolic"),
                set_title: "Already Up to Date",
                set_description: Some("Your system is current.\nNo updates are available right now."),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 12,

                    gtk::Button {
                        set_label: "Done",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        set_tooltip_text: Some("Return to the main screen"),
                        connect_clicked[sender] => move |_| {
                            sender.input(StatusViewInput::StateChanged(AppState::Idle));
                        },
                    },
                },
            } -> {
                set_name: "up_to_date",
            },

            // ─── Error page ─────────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("dialog-warning-symbolic"),
                set_title: "Update Failed",
                set_description: Some("An unexpected error occurred"),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 12,

                    gtk::Button {
                        set_label: "Retry",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        set_tooltip_text: Some("Retry system update"),
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::StartUpdate).unwrap();
                        },
                    },

                    gtk::Button {
                        set_label: "Dismiss",
                        add_css_class: "pill",
                        set_tooltip_text: Some("Dismiss error and return to idle"),
                        connect_clicked[sender] => move |_| {
                            sender.input(StatusViewInput::StateChanged(AppState::Idle));
                        },
                    },
                },
            } -> {
                set_name: "error",
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let log_view = LogView::builder().launch(()).detach();
        let update_list = UpdateList::builder().launch(()).detach();

        let elapsed_label = gtk::Label::new(Some("0:00"));
        elapsed_label.add_css_class("dim-label");
        elapsed_label.add_css_class("caption");
        elapsed_label.add_css_class("monospace");

        let last_update_label = gtk::Label::new(None);
        let toast_overlay = adw::ToastOverlay::new();

        // Image info badge for the idle page.
        let idle_image_box = build_image_badge(read_image_info().as_deref());

        // Build the "updating" page content imperatively.
        let progress_bar = gtk::ProgressBar::new();
        progress_bar.set_show_text(false);
        progress_bar.add_css_class("osd");

        // Image badge for the updating page header.
        let updating_image_box = build_image_badge(read_image_info().as_deref());
        updating_image_box.set_margin_top(8);
        updating_image_box.set_margin_bottom(4);

        let log_clamp = adw::Clamp::new();
        log_clamp.set_maximum_size(800);
        log_clamp.set_vexpand(true);
        log_clamp.set_child(Some(log_view.widget()));

        let copy_btn = gtk::Button::builder()
            .label("Copy Log")
            .tooltip_text("Copy log output to clipboard")
            .icon_name("edit-copy-symbolic")
            .build();
        copy_btn.add_css_class("pill");
        let copy_sender = sender.input_sender().clone();
        copy_btn.connect_clicked(move |_| {
            copy_sender.emit(StatusViewInput::CopyLog);
        });

        let cancel_btn = gtk::Button::builder()
            .label("Cancel")
            .tooltip_text("Cancel the running update")
            .build();
        cancel_btn.add_css_class("pill");
        cancel_btn.add_css_class("destructive-action");
        let cancel_sender = sender.output_sender().clone();
        cancel_btn.connect_clicked(move |_| {
            let _ = cancel_sender.send(StatusViewOutput::CancelUpdate);
        });

        let bottom_bar = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        bottom_bar.set_halign(gtk::Align::Center);
        bottom_bar.set_margin_top(12);
        bottom_bar.set_margin_bottom(12);
        bottom_bar.append(&elapsed_label);
        bottom_bar.append(&copy_btn);
        bottom_bar.append(&cancel_btn);

        let updating_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        updating_content.append(&progress_bar);
        updating_content.append(&updating_image_box);
        updating_content.append(update_list.widget());
        updating_content.append(&log_clamp);
        updating_content.append(&bottom_bar);

        toast_overlay.set_child(Some(&updating_content));

        // Populate last update time on the idle page.
        if let Some(text) = get_last_update_time() {
            last_update_label.set_label(&text);
            last_update_label.set_visible(true);
        } else {
            last_update_label.set_visible(false);
        }

        let model = StatusView {
            state: init,
            log_view,
            update_list,
            stack: root.clone(),
            update_start: None,
            elapsed_label: elapsed_label.clone(),
            log_text: String::new(),
            toast_overlay,
            last_update_label: last_update_label.clone(),
            idle_image_box: idle_image_box.clone(),
        };

        let idle_image_box = &model.idle_image_box;
        let last_update_label = &model.last_update_label;
        let widgets = view_output!();

        // Set initial visible page.
        root.set_visible_child_name("idle");

        // Pulse progress bar + update elapsed timer every 250ms.
        let stack_ref = root.clone();
        let timer_sender = sender.input_sender().clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            if stack_ref.visible_child_name().as_deref() == Some("updating") {
                progress_bar.pulse();
                timer_sender.emit(StatusViewInput::TimerTick);
            }
            gtk::glib::ControlFlow::Continue
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            StatusViewInput::StateChanged(new_state) => {
                let stack_name = match &new_state {
                    AppState::Idle => "idle",
                    AppState::Updating => "updating",
                    AppState::Complete => "complete",
                    AppState::UpToDate => "up_to_date",
                    AppState::Error(_) => "error",
                };
                self.stack.set_visible_child_name(stack_name);

                match &new_state {
                    AppState::Updating => {
                        self.update_start = Some(Instant::now());
                        self.elapsed_label.set_label("0:00");
                        self.update_list.emit(UpdateListInput::Reset);
                    }
                    AppState::Complete => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkAllComplete);
                    }
                    AppState::Error(_) => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkCurrentFailed);
                    }
                    AppState::UpToDate => {
                        self.update_start = None;
                    }
                    AppState::Idle => {
                        self.update_start = None;
                        if let Some(text) = get_last_update_time() {
                            self.last_update_label.set_label(&text);
                            self.last_update_label.set_visible(true);
                        }
                    }
                }

                // Dynamically set error description from the error payload.
                if let AppState::Error(ref err) = new_state {
                    if let Some(page) = self.stack.child_by_name("error") {
                        if let Ok(status_page) = page.downcast::<adw::StatusPage>() {
                            status_page.set_description(Some(err.as_str()));
                        }
                    }
                }

                self.state = new_state;
            }

            StatusViewInput::AppendLog(line) => {
                if !self.log_text.is_empty() {
                    self.log_text.push('\n');
                }
                self.log_text.push_str(&line);
                self.update_list.emit(UpdateListInput::ProcessLine(line.clone()));
                self.log_view.emit(LogViewInput::AppendLine(line));
            }

            StatusViewInput::ClearLog => {
                self.log_text.clear();
                self.log_view.emit(LogViewInput::Clear);
            }

            StatusViewInput::TimerTick => {
                if let Some(start) = self.update_start {
                    let elapsed = start.elapsed();
                    let secs = elapsed.as_secs();
                    let mins = secs / 60;
                    let remaining_secs = secs % 60;
                    self.elapsed_label
                        .set_label(&format!("{}:{:02}", mins, remaining_secs));
                }
            }

            StatusViewInput::CopyLog => {
                if let Some(display) = gtk::gdk::Display::default() {
                    let clipboard = display.clipboard();
                    clipboard.set_text(&self.log_text);
                    let toast = adw::Toast::new("Log copied to clipboard");
                    toast.set_timeout(3);
                    self.toast_overlay.add_toast(toast);
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a pill-style image info badge widget.
/// Returns a hidden, zero-height box if no text is available.
fn build_image_badge(text: Option<&str>) -> gtk::Box {
    let badge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    badge.set_halign(gtk::Align::Center);
    badge.set_visible(text.is_some());

    if let Some(label_text) = text {
        badge.add_css_class("card");

        let label = gtk::Label::new(Some(label_text));
        label.add_css_class("caption");
        label.add_css_class("monospace");
        label.set_margin_start(12);
        label.set_margin_end(12);
        label.set_margin_top(4);
        label.set_margin_bottom(4);

        badge.append(&label);
    }

    badge
}

/// Read the current OS image name and variant from `/etc/os-release`.
/// Tries `/run/host/etc/os-release` first for Flatpak compatibility.
fn read_image_info() -> Option<String> {
    // /run/host/etc/os-release is populated when the Flatpak has host filesystem access.
    let candidates = ["/run/host/etc/os-release", "/etc/os-release"];

    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            let mut image_id: Option<String> = None;
            let mut variant_id: Option<String> = None;

            for line in content.lines() {
                if let Some(v) = line.strip_prefix("IMAGE_ID=") {
                    image_id = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("VARIANT_ID=") {
                    variant_id = Some(v.trim_matches('"').to_string());
                }
            }

            let result = match (image_id, variant_id) {
                (Some(id), Some(var)) => Some(format!("{}  ·  {}", id, var)),
                (Some(id), None) => Some(id),
                _ => None,
            };

            if result.is_some() {
                return result;
            }
        }
    }

    None
}

/// Try to determine when the last successful update ran.
fn get_last_update_time() -> Option<String> {
    let paths = ["/var/lib/uupd/last-run", "/var/lib/uupd/.last-run"];

    for path in &paths {
        if let Ok(metadata) = std::fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                let elapsed = modified.elapsed().ok()?;
                let hours = elapsed.as_secs() / 3600;
                if hours < 1 {
                    return Some("Last update: less than an hour ago".to_string());
                } else if hours < 24 {
                    return Some(format!("Last update: {} hours ago", hours));
                } else {
                    let days = hours / 24;
                    return Some(format!("Last update: {} days ago", days));
                }
            }
        }
    }

    if let Ok(metadata) = std::fs::metadata("/sysroot/ostree/deploy") {
        if let Ok(modified) = metadata.modified() {
            if let Ok(elapsed) = modified.elapsed() {
                let hours = elapsed.as_secs() / 3600;
                if hours < 24 {
                    return Some(format!("System deployed: {} hours ago", hours));
                } else {
                    let days = hours / 24;
                    return Some(format!("System deployed: {} days ago", days));
                }
            }
        }
    }

    None
}
