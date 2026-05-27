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

use crate::app::{AppState, PreflightStatus};
use crate::config;
use crate::ui::log_view::{LogView, LogViewInput};
use crate::ui::segmented_progress::{SegmentedProgress, same_segment};
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
    /// Result of the startup preflight update check.
    PreflightResult(PreflightStatus),
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
    /// User wants to open the rollback/rebase dialog.
    ShowRebase,
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
    /// Reference to idle page for dynamic description.
    idle_page: adw::StatusPage,
    /// Preflight check result.
    preflight_status: PreflightStatus,
    /// Cached last-update text.
    last_update_text: Option<String>,
    /// Cached image info text.
    image_info: Option<String>,
    /// Segmented progress bar shown while updating.
    seg_progress: SegmentedProgress,
    /// The module key that is currently active (drives segment coloring).
    active_module: Option<&'static str>,
}

impl StatusView {
    /// Build the idle page description from current state.
    fn idle_description(&self) -> String {
        let mut parts = Vec::new();

        match &self.preflight_status {
            PreflightStatus::UpdateAvailable => parts.push("Update available".to_string()),
            PreflightStatus::UpToDate => parts.push("System is up to date".to_string()),
            _ => parts.push("Your system is managed by uupd".to_string()),
        }

        if let Some(ref text) = self.last_update_text {
            parts.push(text.clone());
        }

        if let Some(ref info) = self.image_info {
            parts.push(info.clone());
        }

        parts.join("\n")
    }

    fn refresh_idle_description(&self) {
        self.idle_page.set_description(Some(&self.idle_description()));
    }
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
            add_child = &model.idle_page.clone() -> adw::StatusPage {} -> {
                set_name: "idle",
            },

            // ─── Updating page ──────────────────────────────────────────
            add_child = &model.toast_overlay.clone() -> adw::ToastOverlay {} -> {
                set_name: "updating",
            },

            // ─── Complete page ──────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("object-select-symbolic"),
                set_title: "Update Complete",
                set_description: Some("Restart to apply changes."),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 8,

                    gtk::Button {
                        set_label: "Restart…",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::Reboot).unwrap();
                        },
                    },

                    gtk::Button {
                        set_label: "Restart Later",
                        add_css_class: "flat",
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
                set_icon_name: Some("emblem-ok-symbolic"),
                set_title: "Up to Date",
                set_description: Some("No updates available."),

                #[wrap(Some)]
                set_child = &gtk::Button {
                    set_label: "Done",
                    add_css_class: "pill",
                    set_halign: gtk::Align::Center,
                    connect_clicked[sender] => move |_| {
                        sender.input(StatusViewInput::StateChanged(AppState::Idle));
                    },
                },
            } -> {
                set_name: "up_to_date",
            },

            // ─── Error page ─────────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("dialog-warning-symbolic"),
                set_title: "Update Failed",
                set_description: Some("Something went wrong."),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 8,

                    gtk::Button {
                        set_label: "Retry",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::StartUpdate).unwrap();
                        },
                    },

                    gtk::Button {
                        set_label: "Dismiss",
                        add_css_class: "flat",
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

        let toast_overlay = adw::ToastOverlay::new();

        // ── Idle page (built imperatively) ──────────────────────────────
        let idle_page = adw::StatusPage::builder()
            .icon_name(config::APP_ID)
            .title("System Update")
            .build();

        let idle_buttons = gtk::Box::new(gtk::Orientation::Vertical, 8);
        idle_buttons.set_halign(gtk::Align::Center);

        let check_btn = gtk::Button::builder()
            .label("Check for Updates")
            .build();
        check_btn.add_css_class("suggested-action");
        check_btn.add_css_class("pill");
        let start_sender = sender.output_sender().clone();
        check_btn.connect_clicked(move |_| {
            let _ = start_sender.send(StatusViewOutput::StartUpdate);
        });

        let rebase_btn = gtk::Button::builder()
            .label("Previous Versions…")
            .build();
        rebase_btn.add_css_class("flat");
        let rebase_sender = sender.output_sender().clone();
        rebase_btn.connect_clicked(move |_| {
            let _ = rebase_sender.send(StatusViewOutput::ShowRebase);
        });

        idle_buttons.append(&check_btn);
        idle_buttons.append(&rebase_btn);
        idle_page.set_child(Some(&idle_buttons));

        // Build the "updating" page content imperatively.
        let seg_progress = SegmentedProgress::new();

        // Image info label for the updating page header.
        let updating_image_label = gtk::Label::new(read_image_info().as_deref());
        updating_image_label.add_css_class("caption");
        updating_image_label.add_css_class("dim-label");
        updating_image_label.add_css_class("monospace");
        updating_image_label.set_margin_top(8);
        updating_image_label.set_margin_bottom(4);
        updating_image_label.set_visible(read_image_info().is_some());

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

        // HIG: Clamp non-log content to consistent max-width (matches log_clamp).
        let header_clamp = adw::Clamp::new();
        header_clamp.set_maximum_size(800);
        let header_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        header_box.append(&seg_progress.widget());
        header_box.append(&updating_image_label);
        header_box.append(update_list.widget());
        header_clamp.set_child(Some(&header_box));

        updating_content.append(&header_clamp);
        updating_content.append(&log_clamp);
        updating_content.append(&bottom_bar);

        toast_overlay.set_child(Some(&updating_content));

        let model = StatusView {
            state: init,
            log_view,
            update_list,
            stack: root.clone(),
            update_start: None,
            elapsed_label: elapsed_label.clone(),
            log_text: String::new(),
            toast_overlay,
            idle_page,
            preflight_status: PreflightStatus::Checking,
            last_update_text: get_last_update_time(),
            image_info: read_image_info(),
            seg_progress,
            active_module: None,
        };

        let widgets = view_output!();

        // Set initial idle description and visible page.
        model.refresh_idle_description();
        root.set_visible_child_name("idle");

        // Update elapsed timer every 250ms while the "updating" page is visible.
        let stack_ref = root.clone();
        let timer_sender = sender.input_sender().clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            if stack_ref.visible_child_name().as_deref() == Some("updating") {
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
                        self.seg_progress.reset();
                        self.active_module = None;
                    }
                    AppState::Complete => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkAllComplete);
                        self.seg_progress.mark_all_complete();
                        self.active_module = None;
                    }
                    AppState::Error(_) => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkCurrentFailed);
                        if let Some(key) = self.active_module {
                            self.seg_progress.set_module_failed(key);
                        }
                        self.active_module = None;
                    }
                    AppState::UpToDate => {
                        self.update_start = None;
                    }
                    AppState::Idle => {
                        self.update_start = None;
                        self.last_update_text = get_last_update_time();
                        self.refresh_idle_description();
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
                self.update_list
                    .emit(UpdateListInput::ProcessLine(line.clone()));
                self.log_view.emit(LogViewInput::AppendLine(line.clone()));

                // Drive segmented progress from log output.
                if let Some(new_key) = extract_module_key(&line) {
                    let is_same_seg = self
                        .active_module
                        .map(|prev| same_segment(prev, new_key))
                        .unwrap_or(false);
                    if !is_same_seg {
                        // Complete the previous segment only when switching to a
                        // different visual segment (brew→distrobox stays in Dev Tools).
                        if let Some(prev) = self.active_module {
                            self.seg_progress.set_module_complete(prev);
                        }
                        self.seg_progress.set_module_active(new_key);
                    }
                    self.active_module = Some(new_key);
                } else if line.contains("level=ERROR") || line.contains("level=error") {
                    if let Some(key) = self.active_module {
                        self.seg_progress.set_module_failed(key);
                    }
                }
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

            StatusViewInput::PreflightResult(status) => {
                self.preflight_status = status;
                self.refresh_idle_description();
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

/// Extract a static module key from a uupd log line.
///
/// uupd emits `module_name=System` (capital first letter) in structured log
/// lines when it begins processing a module. Map those to the lowercase keys
/// used by [`SegmentedProgress`].
fn extract_module_key(line: &str) -> Option<&'static str> {
    if line.contains("module_name=System") {
        Some("system")
    } else if line.contains("module_name=Flatpak") {
        Some("flatpak")
    } else if line.contains("module_name=Brew") {
        Some("brew")
    } else if line.contains("module_name=Distrobox") {
        Some("distrobox")
    } else {
        None
    }
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
