//! Top-level application component.
//!
//! Pattern: Main window component owns the `AdwApplicationWindow` and orchestrates
//! child components. Uses `AdwToolbarView` as the structural backbone (header + content),
//! with `AdwToastOverlay` wrapping the content area for transient notifications.
//!
//! State machine:
//!   Idle → Updating → (Complete | Error) → Idle
//!
//! This module demonstrates the canonical Bluefin app window structure.

use gtk::prelude::*;
use relm4::prelude::*;

use crate::ui::status_view::{StatusView, StatusViewInput, StatusViewOutput};
use crate::update_worker::{UpdateEvent, UpdateWorker};

/// Application-level state.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum AppState {
    /// No update in progress; ready to start one.
    #[default]
    Idle,
    /// Update is actively running.
    Updating,
    /// Update completed successfully.
    Complete,
    /// Update failed with an error message.
    Error(String),
}

/// Top-level model.
pub struct App {
    state: AppState,
    /// Accumulated output lines from the uupd process.
    log_lines: Vec<String>,
    /// Toast overlay reference for showing transient notifications.
    toast_overlay: adw::ToastOverlay,
    /// Child component: the main status/content view.
    status_view: Controller<StatusView>,
}

/// Messages the App component can receive.
#[derive(Debug)]
pub enum AppMsg {
    /// User clicked "Update" — start the uupd process.
    StartUpdate,
    /// A line of output arrived from the subprocess.
    OutputLine(String),
    /// The subprocess exited successfully.
    UpdateComplete,
    /// The subprocess failed.
    UpdateFailed(String),
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        // AdwApplicationWindow is required (not gtk::ApplicationWindow) for
        // libadwaita adaptive features and proper style inheritance.
        adw::ApplicationWindow {
            set_title: Some("System Update"),
            // HIG: minimum window size ensures content is never clipped.
            set_default_size: (600, 500),
            set_width_request: 360,
            set_height_request: 360,

            // AdwToolbarView is the modern GNOME pattern for header + content layout.
            // It handles the header bar integration with scrolling content automatically.
            adw::ToolbarView {
                // Top bar
                add_top_bar = &adw::HeaderBar {
                    // HIG: use a symbolic icon as the window icon hint.
                    pack_start = &gtk::Button {
                        set_icon_name: "info-symbolic",
                        set_tooltip_text: Some("About Finpilot"),
                    },
                },

                // Content area wrapped in ToastOverlay for transient notifications.
                #[wrap(Some)]
                set_content = &model.toast_overlay.clone() -> adw::ToastOverlay {
                    // The status view child component occupies the content area.
                    set_child: Some(model.status_view.widget()),
                },
            },
        }
    }

    /// Initialize the component tree.
    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Build child component: StatusView receives state updates and emits user actions.
        let status_view = StatusView::builder()
            .launch(AppState::Idle)
            .forward(sender.input_sender(), |output| match output {
                StatusViewOutput::StartUpdate => AppMsg::StartUpdate,
            });

        let toast_overlay = adw::ToastOverlay::new();

        let model = App {
            state: AppState::Idle,
            log_lines: Vec::new(),
            toast_overlay: toast_overlay.clone(),
            status_view,
        };

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::StartUpdate => {
                tracing::info!("Starting system update via uupd");
                self.state = AppState::Updating;
                self.log_lines.clear();

                // Forward state to the child view.
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Updating));
                self.status_view
                    .emit(StatusViewInput::ClearLog);

                // Spawn the async update worker on a background thread.
                // Pattern: use sender.command() to bridge async work back to the
                // component via CommandOutput. We use a simpler approach here:
                // spawn a tokio task that sends messages back via the input sender.
                let input_sender = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("Failed to create tokio runtime");

                    rt.block_on(async move {
                        let mut worker = UpdateWorker::new();
                        let mut rx = worker.run().await;

                        while let Some(event) = rx.recv().await {
                            match event {
                                UpdateEvent::Output(line) => {
                                    input_sender.emit(AppMsg::OutputLine(line));
                                }
                                UpdateEvent::Complete => {
                                    input_sender.emit(AppMsg::UpdateComplete);
                                    break;
                                }
                                UpdateEvent::Error(err) => {
                                    input_sender.emit(AppMsg::UpdateFailed(err));
                                    break;
                                }
                            }
                        }
                    });
                });
            }

            AppMsg::OutputLine(line) => {
                self.log_lines.push(line.clone());
                self.status_view
                    .emit(StatusViewInput::AppendLog(line));
            }

            AppMsg::UpdateComplete => {
                tracing::info!("System update completed successfully");
                self.state = AppState::Complete;
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Complete));

                // HIG: Use AdwToast for transient success notifications.
                let toast = adw::Toast::new("System update complete");
                toast.set_timeout(5);
                self.toast_overlay.add_toast(toast);
            }

            AppMsg::UpdateFailed(err) => {
                tracing::error!("System update failed: {}", err);
                self.state = AppState::Error(err.clone());
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Error(err)));
            }
        }
    }
}
