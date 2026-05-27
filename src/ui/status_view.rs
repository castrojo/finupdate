//! Status view component — the main content area of the app.
//!
//! Pattern: State-driven view switching
//! Uses a `gtk::Stack` to switch between different visual states:
//! - Idle: AdwStatusPage with "ready to update" messaging
//! - Updating: Progress indicator + live log + cancel button
//! - Complete: Success status page with reboot option
//! - Error: Error status page with retry option
//!
//! This pattern avoids manual show/hide logic and leverages GTK's
//! built-in transition animations.

use gtk::prelude::*;
use relm4::prelude::*;

use crate::app::AppState;
use crate::ui::log_view::{LogView, LogViewInput};

/// Input messages for the StatusView component.
#[derive(Debug)]
pub enum StatusViewInput {
    /// Parent tells us the app state changed.
    StateChanged(AppState),
    /// Append a log line to the output view.
    AppendLog(String),
    /// Clear the log buffer.
    ClearLog,
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
    /// Direct reference to the root stack for page switching in update().
    stack: gtk::Stack,
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
                set_icon_name: Some("system-software-update-symbolic"),
                set_title: "System Up to Date",
                set_description: Some("Your system is managed by uupd.\nClick below to check for and install updates now."),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 12,

                    gtk::Button {
                        set_label: "Check for Updates",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        set_tooltip_text: Some("Check for and install system updates"),
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::StartUpdate).unwrap();
                        },
                    },
                },
            } -> {
                set_name: "idle",
            },

            // ─── Updating page ──────────────────────────────────────────
            add_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 0,

                #[name = "progress_bar"]
                gtk::ProgressBar {
                    set_show_text: false,
                    add_css_class: "osd",
                },

                adw::Clamp {
                    set_maximum_size: 800,
                    set_vexpand: true,
                    set_child: Some(model.log_view.widget()),
                },

                // Cancel button at bottom of updating view.
                gtk::Box {
                    set_halign: gtk::Align::Center,
                    set_margin_top: 12,
                    set_margin_bottom: 12,

                    gtk::Button {
                        set_label: "Cancel",
                        add_css_class: "pill",
                        add_css_class: "destructive-action",
                        set_tooltip_text: Some("Cancel the running update"),
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::CancelUpdate).unwrap();
                        },
                    },
                },
            } -> {
                set_name: "updating",
            },

            // ─── Complete page ──────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("emblem-ok-symbolic"),
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

            // ─── Error page ─────────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("dialog-error-symbolic"),
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
        let log_view = LogView::builder()
            .launch(())
            .detach();

        let model = StatusView {
            state: init,
            log_view,
            stack: root.clone(),
        };

        let widgets = view_output!();

        // Set initial visible page.
        root.set_visible_child_name("idle");

        // Pulse the progress bar only when updating page is visible.
        let progress_bar = widgets.progress_bar.clone();
        let stack_ref = root.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            if stack_ref.visible_child_name().as_deref() == Some("updating") {
                progress_bar.pulse();
            }
            gtk::glib::ControlFlow::Continue
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            StatusViewInput::StateChanged(new_state) => {
                // Switch the visible stack page based on current state.
                let stack_name = match &new_state {
                    AppState::Idle => "idle",
                    AppState::Updating => "updating",
                    AppState::Complete => "complete",
                    AppState::Error(_) => "error",
                };
                self.stack.set_visible_child_name(stack_name);

                // Update error description dynamically via stack child lookup.
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
                self.log_view.emit(LogViewInput::AppendLine(line));
            }
            StatusViewInput::ClearLog => {
                self.log_view.emit(LogViewInput::Clear);
            }
        }
    }
}
