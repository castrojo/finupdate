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

use adw::prelude::*;
use relm4::actions::{RelmAction, RelmActionGroup};
use relm4::prelude::*;

use crate::config;
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
    /// Handle to cancel a running update (sends kill signal to subprocess).
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
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
    /// User wants to cancel the running update.
    CancelUpdate,
    /// User wants to reboot the system.
    Reboot,
    /// User requested the About dialog.
    ShowAbout,
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        // AdwApplicationWindow is required (not gtk::ApplicationWindow) for
        // libadwaita adaptive features and proper style inheritance.
        #[root]
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
                    pack_end = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_tooltip_text: Some("Main Menu"),
                        set_menu_model: Some(&main_menu),
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

    menu! {
        main_menu: {
            "_About Finpilot" => AboutAction,
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
                StatusViewOutput::CancelUpdate => AppMsg::CancelUpdate,
                StatusViewOutput::Reboot => AppMsg::Reboot,
            });

        let toast_overlay = adw::ToastOverlay::new();

        let model = App {
            state: AppState::Idle,
            log_lines: Vec::new(),
            toast_overlay: toast_overlay.clone(),
            status_view,
            cancel_tx: None,
        };

        let widgets = view_output!();

        // Register the About action using relm4's action system.
        let about_action: RelmAction<AboutAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::ShowAbout);
            })
        };

        let mut group = RelmActionGroup::<WindowActionGroup>::new();
        group.add_action(about_action);
        group.register_for_widget(&root);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::StartUpdate => {
                // Prevent double-starts.
                if self.state == AppState::Updating {
                    return;
                }

                tracing::info!("Starting system update via uupd");
                self.state = AppState::Updating;
                self.log_lines.clear();

                // Forward state to the child view.
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Updating));
                self.status_view
                    .emit(StatusViewInput::ClearLog);

                // Create a cancellation channel for this update run.
                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
                self.cancel_tx = Some(cancel_tx);

                // Spawn the async update worker on a background thread.
                let input_sender = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("Failed to create tokio runtime");

                    rt.block_on(async move {
                        let mut worker = UpdateWorker::new();
                        let mut rx = worker.run().await;

                        // Use select! to race between events and cancellation.
                        tokio::pin!(cancel_rx);

                        loop {
                            tokio::select! {
                                event = rx.recv() => {
                                    match event {
                                        Some(UpdateEvent::Output(line)) => {
                                            input_sender.emit(AppMsg::OutputLine(line));
                                        }
                                        Some(UpdateEvent::Complete) => {
                                            input_sender.emit(AppMsg::UpdateComplete);
                                            break;
                                        }
                                        Some(UpdateEvent::Error(err)) => {
                                            input_sender.emit(AppMsg::UpdateFailed(err));
                                            break;
                                        }
                                        None => break,
                                    }
                                }
                                _ = &mut cancel_rx => {
                                    // User cancelled — the worker's process will be
                                    // dropped when this scope exits, killing it.
                                    input_sender.emit(AppMsg::UpdateFailed(
                                        "Update cancelled by user".to_string()
                                    ));
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
                self.cancel_tx = None;
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Complete));

                // HIG: Use AdwToast for transient success notifications.
                let toast = adw::Toast::new("System update complete — restart to apply");
                toast.set_timeout(0); // Persistent until dismissed
                toast.set_button_label(Some("Dismiss"));
                self.toast_overlay.add_toast(toast);
            }

            AppMsg::UpdateFailed(err) => {
                tracing::error!("System update failed: {}", err);
                self.state = AppState::Error(err.clone());
                self.cancel_tx = None;
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Error(err)));
            }

            AppMsg::CancelUpdate => {
                if let Some(tx) = self.cancel_tx.take() {
                    tracing::info!("User requested update cancellation");
                    let _ = tx.send(());
                }
            }

            AppMsg::Reboot => {
                tracing::info!("User requested system reboot");
                // Use the same sandbox-aware pattern to invoke systemctl reboot.
                std::thread::spawn(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("Failed to create tokio runtime");

                    rt.block_on(async {
                        let result = if crate::update_worker::is_flatpak() {
                            tokio::process::Command::new("flatpak-spawn")
                                .args(["--host", "systemctl", "reboot"])
                                .status()
                                .await
                        } else {
                            tokio::process::Command::new("systemctl")
                                .arg("reboot")
                                .status()
                                .await
                        };

                        if let Err(e) = result {
                            tracing::error!("Failed to initiate reboot: {}", e);
                        }
                    });
                });
            }

            AppMsg::ShowAbout => {
                let about = adw::AboutDialog::builder()
                    .application_name("Finpilot")
                    .application_icon(config::APP_ID)
                    .developer_name("Project Bluefin")
                    .version(config::VERSION)
                    .website("https://projectbluefin.io")
                    .issue_url("https://github.com/castrojo/finupdate/issues")
                    .license_type(gtk::License::MitX11)
                    .developers(vec!["Project Bluefin Contributors"])
                    .comments("A friendly system update frontend for Bluefin")
                    .build();

                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        about.present(window);
                    }
                }
            }
        }
    }
}

// Action group and actions for the window-level menu.
relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");