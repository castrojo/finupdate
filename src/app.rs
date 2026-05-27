//! Top-level application component.
//!
//! ## relm4 Design Rationale
//!
//! This module demonstrates the **canonical relm4 component pattern** for Bluefin apps:
//!
//! 1. **Single top-level component** owns the `AdwApplicationWindow` and the state machine.
//!    It is the sole orchestrator — child components communicate UP via `Output` messages
//!    and receive commands DOWN via `emit()` on their controller handle.
//!
//! 2. **Message-driven state** — all state transitions happen through `AppMsg` variants
//!    processed in a single `update()` method. This makes state transitions explicit,
//!    traceable (via `tracing`), and impossible to miss. No widget callbacks mutate
//!    state directly.
//!
//! 3. **Forward pattern** — child component outputs are mapped to parent inputs via
//!    `.forward(sender, |output| match output { ... })`. This decouples children from
//!    the parent's message type.
//!
//! 4. **Action groups** — menu items and keyboard shortcuts use relm4's action system
//!    (`new_action_group!`, `new_stateless_action!`) rather than raw GAction. This keeps
//!    type safety and connects naturally to the message bus.
//!
//! 5. **Separate async thread** — long-running work (subprocess) runs on a tokio runtime
//!    in `std::thread::spawn`. Results flow back via `sender.emit()` which is thread-safe
//!    and queues messages on the GLib main loop.
//!
//! ## State machine
//!
//!   Idle → Updating → (Complete | Error) → Idle
//!
//! ## Component hierarchy
//!
//!   App (this)
//!   └── StatusView (content area, owns LogView)
//!       └── LogView (scrollable text output)
//!
//! ## Why SimpleComponent (not Component)?
//!
//! `SimpleComponent` is sufficient because:
//! - We don't need `CommandOutput` (we use manual thread + channel instead for streaming)
//! - We don't produce output messages (top-level component has no parent)
//! - The simpler trait reduces boilerplate
//!
//! Use full `Component` with `CommandOutput` when you need a single async result
//! (not streaming). Use `AsyncComponent` when the init itself is async.

use adw::prelude::*;
use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::prelude::*;

use crate::config;
use crate::settings::Settings;
use crate::ui::preferences::show_preferences;
use crate::ui::rebase_dialog::show_rebase_dialog;
use crate::ui::status_view::{StatusView, StatusViewInput, StatusViewOutput};
use crate::update_worker::{SimulationScenario, UpdateEvent, UpdateWorker, run_simulated};

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
    /// uupd exited with code 77 — system is already current, nothing to do.
    UpToDate,
    /// Update failed with an error message.
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreflightStatus {
    Checking,
    UpdateAvailable,
    UpToDate,
    Unknown,
}

/// Top-level model.
pub struct App {
    state: AppState,
    preflight_status: PreflightStatus,
    /// Selected developer-mode simulation scenario.
    sim_scenario: SimulationScenario,
    /// Accumulated output lines from the uupd process.
    log_lines: Vec<String>,
    /// Toast overlay reference for showing transient notifications.
    toast_overlay: adw::ToastOverlay,
    /// Child component: the main status/content view.
    status_view: Controller<StatusView>,
    /// Handle to cancel a running update (sends kill signal to subprocess).
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Reference to header bar for dynamic subtitle updates.
    header_bar: adw::HeaderBar,
    /// Banner shown when developer mode is active.
    dev_banner: adw::Banner,
    /// Persistent user preferences.
    settings: Settings,
}

/// Messages the App component can receive.
#[derive(Debug)]
pub enum AppMsg {
    /// User clicked "Update" — optionally bypass the metered-network confirmation.
    StartUpdate { skip_metered_check: bool },
    /// A line of output arrived from the subprocess.
    OutputLine(String),
    /// The subprocess exited successfully.
    UpdateComplete,
    /// The subprocess reported that the system is already up to date (exit 77).
    UpdateUpToDate,
    /// The subprocess failed.
    UpdateFailed(String),
    /// User wants to cancel the running update.
    CancelUpdate,
    /// User wants to reboot the system.
    RequestReboot,
    /// User confirmed reboot in the dialog.
    ConfirmReboot,
    /// User requested the Rebase History dialog.
    ShowRebaseDialog,
    /// User requested the About dialog.
    ShowAbout,
    /// User requested the Preferences dialog.
    ShowPreferences,
    /// Settings were updated in the preferences dialog.
    SettingsChanged(Settings),
    /// Result of the startup preflight update check.
    PreflightResult(PreflightStatus),
    /// Developer mode toggle from the hamburger menu.
    ToggleDevMode(bool),
    /// Update the selected developer-mode simulation scenario.
    SetSimScenario(SimulationScenario),
    /// Quit the application.
    Quit,
    /// Window close was requested — check if we should allow it.
    CloseRequest,
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        #[root]
        adw::ApplicationWindow {
            set_title: Some("System Update"),
            // HIG: minimum window size ensures content is never clipped.
            set_default_size: (750, 700),
            set_width_request: 400,
            set_height_request: 500,

            // AdwToolbarView is the modern GNOME pattern for header + content layout.
            // It handles the header bar integration with scrolling content automatically.
            adw::ToolbarView {
                // Top bar — store reference for dynamic subtitle changes.
                add_top_bar = &model.header_bar.clone() -> adw::HeaderBar {
                    pack_end = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_tooltip_text: Some("Main Menu"),
                        set_menu_model: Some(&main_menu),
                    },
                },

                // Developer mode banner — shown when dev_mode is active.
                add_top_bar = &model.dev_banner.clone() -> adw::Banner {
                    set_title: "Developer Mode — updates are simulated",
                    set_revealed: model.settings.dev_mode,
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
            "_Preferences" => PreferencesAction,
            "_Developer Mode" => DeveloperModeAction,
            section! {
                "Simulate _Success" => SimSuccessAction,
                "Simulate _Failure" => SimFailureAction,
                "Simulate Already _Up To Date" => SimUpToDateAction,
            },
            section! {
                "_Rebase to Previous Version…" => RebaseAction,
            },
            section! {
                "_About Finupdate" => AboutAction,
                "_Keyboard Shortcuts" => ShortcutsAction,
                "_Quit" => QuitAction,
            }
        }
    }

    /// Initialize the component tree.
    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Build child component: StatusView receives state updates and emits user actions.
        let status_view =
            StatusView::builder()
                .launch(AppState::Idle)
                .forward(sender.input_sender(), |output| match output {
                    StatusViewOutput::StartUpdate => AppMsg::StartUpdate {
                        skip_metered_check: false,
                    },
                    StatusViewOutput::CancelUpdate => AppMsg::CancelUpdate,
                    StatusViewOutput::Reboot => AppMsg::RequestReboot,
                    StatusViewOutput::ShowRebase => AppMsg::ShowRebaseDialog,
                });

        let toast_overlay = adw::ToastOverlay::new();
        let header_bar = adw::HeaderBar::new();
        let dev_banner = adw::Banner::new("Developer Mode — updates are simulated");

        let settings = Settings::load();

        let model = App {
            state: AppState::Idle,
            preflight_status: PreflightStatus::Checking,
            sim_scenario: SimulationScenario::Success,
            log_lines: Vec::new(),
            toast_overlay: toast_overlay.clone(),
            status_view,
            cancel_tx: None,
            header_bar,
            dev_banner,
            settings,
        };

        let widgets = view_output!();

        // ─── Actions ────────────────────────────────────────────────────
        let about_action: RelmAction<AboutAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::ShowAbout);
            })
        };

        let preferences_action: RelmAction<PreferencesAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::ShowPreferences);
            })
        };

        let quit_action: RelmAction<QuitAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::Quit);
            })
        };

        let shortcuts_action: RelmAction<ShortcutsAction> = {
            let root_clone = root.clone();
            RelmAction::new_stateless(move |_| {
                show_shortcuts_window(&root_clone);
            })
        };

        // Stateful checkmark toggle for developer mode in the hamburger menu.
        let dev_mode_action: RelmAction<DeveloperModeAction> = {
            let sender = sender.input_sender().clone();
            let initial = model.settings.dev_mode;
            RelmAction::new_stateful(&initial, move |_, state: &mut bool| {
                let new_state = !*state;
                *state = new_state;
                sender.emit(AppMsg::ToggleDevMode(new_state));
            })
        };

        let rebase_action: RelmAction<RebaseAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::ShowRebaseDialog);
            })
        };

        let sim_success_action: RelmAction<SimSuccessAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::SetSimScenario(SimulationScenario::Success));
            })
        };

        let sim_failure_action: RelmAction<SimFailureAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::SetSimScenario(SimulationScenario::Failure));
            })
        };

        let sim_uptodate_action: RelmAction<SimUpToDateAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::SetSimScenario(SimulationScenario::AlreadyUpToDate));
            })
        };

        let mut group = RelmActionGroup::<WindowActionGroup>::new();
        group.add_action(about_action);
        group.add_action(preferences_action);
        group.add_action(quit_action);
        group.add_action(shortcuts_action);
        group.add_action(dev_mode_action);
        group.add_action(rebase_action);
        group.add_action(sim_success_action);
        group.add_action(sim_failure_action);
        group.add_action(sim_uptodate_action);
        group.register_for_widget(&root);

        // ─── Keyboard Shortcuts ─────────────────────────────────────────
        let app = relm4::main_application();
        app.set_accelerators_for_action::<QuitAction>(&["<primary>q"]);
        app.set_accelerators_for_action::<ShortcutsAction>(&["<primary>question"]);

        // ─── Close Request Handler ──────────────────────────────────────
        // Intercept window close to warn if an update is in progress.
        let close_sender = sender.input_sender().clone();
        root.connect_close_request(move |_| {
            close_sender.emit(AppMsg::CloseRequest);
            // Inhibit default close — we handle it in update().
            gtk::glib::Propagation::Stop
        });

        // Defer preflight check until the GLib main loop is running to avoid
        // racing with component initialization (the thread could finish before
        // the relm4 message loop processes the first idle).
        let input_sender = sender.input_sender().clone();
        gtk::glib::idle_add_local_once(move || {
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create tokio runtime");
                rt.block_on(async move {
                    let mut cmd = if std::path::Path::new("/.flatpak-info").exists() {
                        let mut c = tokio::process::Command::new("flatpak-spawn");
                        c.arg("--host").arg("uupd").arg("update-check");
                        c
                    } else {
                        let mut c = tokio::process::Command::new("uupd");
                        c.arg("update-check");
                        c
                    };
                    let status = match cmd.status().await {
                        Ok(s) => s,
                        Err(_) => {
                            input_sender.emit(AppMsg::PreflightResult(PreflightStatus::Unknown));
                            return;
                        }
                    };
                    let result = match status.code() {
                        Some(0) => PreflightStatus::UpdateAvailable,
                        Some(77) => PreflightStatus::UpToDate,
                        _ => PreflightStatus::Unknown,
                    };
                    input_sender.emit(AppMsg::PreflightResult(result));
                });
            });
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::StartUpdate { skip_metered_check } => {
                // Prevent double-starts.
                if self.state == AppState::Updating {
                    return;
                }

                if !skip_metered_check
                    && self.settings.pause_on_metered
                    && gtk::gio::NetworkMonitor::default().is_network_metered()
                {
                    let dialog = adw::AlertDialog::new(
                        Some("Metered Connection Detected"),
                        Some(
                            "You're on a limited or cellular connection. Automatic updates are paused, but you can continue manually.",
                        ),
                    );
                    dialog.add_response("cancel", "_Cancel");
                    dialog.add_response("proceed", "_Update Anyway");
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");

                    let update_sender = sender.input_sender().clone();
                    dialog.connect_response(None, move |_, response| {
                        if response == "proceed" {
                            update_sender.emit(AppMsg::StartUpdate {
                                skip_metered_check: true,
                            });
                        }
                    });

                    if let Some(root) = self.status_view.widget().root() {
                        if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                            dialog.present(Some(window));
                        }
                    }

                    return;
                }

                tracing::info!("Starting system update via uupd");
                self.state = AppState::Updating;
                self.log_lines.clear();

                // Update header subtitle to indicate activity.
                self.update_subtitle();

                // Forward state to the child view.
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Updating));
                self.status_view.emit(StatusViewInput::ClearLog);

                // Create a cancellation channel for this update run.
                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
                self.cancel_tx = Some(cancel_tx);

                // Spawn the async update worker on a background thread.
                // cancel_rx is passed INTO the worker so it can kill the real process.
                let input_sender = sender.input_sender().clone();
                let dev_mode = self.settings.dev_mode;
                let sim_scenario = self.sim_scenario;
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("Failed to create tokio runtime");

                    rt.block_on(async move {
                        let mut rx = if dev_mode {
                            tracing::info!(
                                ?sim_scenario,
                                "Developer mode active — running simulated update"
                            );
                            run_simulated(sim_scenario, cancel_rx).await
                        } else {
                            UpdateWorker::new().run(cancel_rx).await
                        };

                        while let Some(event) = rx.recv().await {
                            match event {
                                UpdateEvent::Output(line) => {
                                    input_sender.emit(AppMsg::OutputLine(line));
                                }
                                UpdateEvent::Complete => {
                                    input_sender.emit(AppMsg::UpdateComplete);
                                    break;
                                }
                                UpdateEvent::UpToDate => {
                                    input_sender.emit(AppMsg::UpdateUpToDate);
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
                self.status_view.emit(StatusViewInput::AppendLog(line));
            }

            AppMsg::UpdateComplete => {
                tracing::info!("System update completed successfully");
                self.state = AppState::Complete;
                self.cancel_tx = None;
                self.update_subtitle();
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Complete));

                // HIG: Use AdwToast for transient success notifications.
                let toast = adw::Toast::new("System update complete — restart to apply");
                toast.set_timeout(0); // Persistent until dismissed
                toast.set_button_label(Some("Dismiss"));
                self.toast_overlay.add_toast(toast);

                // Send a desktop notification if window is not focused.
                send_notification(
                    "update-complete",
                    "System Update Complete",
                    "Your system has been updated. Restart to apply changes.",
                );
            }

            AppMsg::UpdateUpToDate => {
                tracing::info!("System is already up to date (uupd exit 77)");
                self.state = AppState::UpToDate;
                self.cancel_tx = None;
                self.update_subtitle();
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::UpToDate));
            }

            AppMsg::UpdateFailed(err) => {
                tracing::error!("System update failed: {}", err);
                self.state = AppState::Error(err.clone());
                self.cancel_tx = None;
                self.update_subtitle();

                // Notify the user if window is backgrounded.
                send_notification("update-failed", "System Update Failed", &err);
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Error(err)));
            }

            AppMsg::CancelUpdate => {
                if let Some(tx) = self.cancel_tx.take() {
                    tracing::info!("User requested update cancellation");
                    let _ = tx.send(());
                }
            }

            AppMsg::RequestReboot => {
                // HIG: Destructive actions require confirmation.
                let dialog = adw::AlertDialog::builder()
                    .heading("Restart System?")
                    .body("The system will restart to apply updates. Save any open work before continuing.")
                    .build();

                dialog.add_response("cancel", "_Cancel");
                dialog.add_response("restart", "_Restart");
                dialog.set_response_appearance("restart", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                let reboot_sender = sender.input_sender().clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "restart" {
                        reboot_sender.emit(AppMsg::ConfirmReboot);
                    }
                });

                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        dialog.present(Some(window));
                    }
                }
            }

            AppMsg::ConfirmReboot => {
                if self.settings.dev_mode {
                    tracing::warn!(
                        "Reboot suppressed — developer mode is active. \
                         Would have called `systemctl reboot`."
                    );
                    let toast = adw::Toast::new("Reboot suppressed (developer mode)");
                    toast.set_timeout(3);
                    self.toast_overlay.add_toast(toast);
                } else {
                    tracing::info!("User confirmed system reboot");
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
            }

            AppMsg::ShowRebaseDialog => {
                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        show_rebase_dialog(window, self.settings.dev_mode);
                    }
                }
            }

            AppMsg::ShowAbout => {
                let about = adw::AboutDialog::builder()
                    .application_name("Finupdate")
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
                        about.present(Some(window));
                    }
                }
            }

            AppMsg::ShowPreferences => {
                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        let sender = sender.input_sender().clone();
                        show_preferences(window, self.settings.clone(), move |updated| {
                            sender.emit(AppMsg::SettingsChanged(updated));
                        });
                    }
                }
            }

            AppMsg::SettingsChanged(new_settings) => {
                tracing::debug!("Settings updated: dev_mode={}", new_settings.dev_mode);
                self.settings = new_settings;
                self.dev_banner.set_revealed(self.settings.dev_mode);
            }

            AppMsg::PreflightResult(status) => {
                self.preflight_status = status.clone();
                self.status_view
                    .emit(StatusViewInput::PreflightResult(status));
            }

            AppMsg::ToggleDevMode(enabled) => {
                tracing::info!("Developer mode toggled via menu: {}", enabled);
                self.settings.dev_mode = enabled;
                self.settings.save();
                self.dev_banner.set_revealed(enabled);
            }

            AppMsg::SetSimScenario(scenario) => {
                tracing::info!(?scenario, "Selected developer simulation scenario");
                self.sim_scenario = scenario;
            }

            AppMsg::Quit => {
                // If update in progress, treat like close request (ask first).
                if self.state == AppState::Updating {
                    sender.input(AppMsg::CloseRequest);
                } else {
                    relm4::main_application().quit();
                }
            }

            AppMsg::CloseRequest => {
                if self.state == AppState::Updating {
                    // Warn user that an update is running.
                    let dialog = adw::AlertDialog::builder()
                        .heading("Update in Progress")
                        .body("An update is currently running. Closing now may leave your system in an inconsistent state.")
                        .build();

                    dialog.add_response("cancel", "_Keep Waiting");
                    dialog.add_response("close", "_Close Anyway");
                    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");

                    dialog.connect_response(None, move |_, response| {
                        if response == "close" {
                            relm4::main_application().quit();
                        }
                    });

                    if let Some(root) = self.status_view.widget().root() {
                        if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                            dialog.present(Some(window));
                        }
                    }
                } else {
                    // Not updating — close immediately.
                    relm4::main_application().quit();
                }
            }
        }
    }
}

impl App {
    /// Update the header bar subtitle to reflect current state.
    fn update_subtitle(&self) {
        let subtitle = match &self.state {
            AppState::Idle => None,
            AppState::Updating => Some("Updating…"),
            AppState::Complete => Some("Update complete"),
            AppState::UpToDate => Some("Already up to date"),
            AppState::Error(_) => Some("Update failed"),
        };
        // AdwHeaderBar doesn't have subtitle — set window title instead.
        if let Some(root) = self.status_view.widget().root() {
            if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                let title = match subtitle {
                    Some(s) => format!("System Update — {}", s),
                    None => "System Update".to_string(),
                };
                window.set_title(Some(&title));
            }
        }
    }
}

/// Show the keyboard shortcuts window.
fn show_shortcuts_window(window: &adw::ApplicationWindow) {
    let shortcuts = gtk::ShortcutsWindow::builder()
        .transient_for(window)
        .modal(true)
        .build();

    let section = gtk::ShortcutsSection::builder()
        .section_name("shortcuts")
        .visible(true)
        .build();

    let group = gtk::ShortcutsGroup::builder().title("Application").build();

    let shortcut_quit = gtk::ShortcutsShortcut::builder()
        .title("Quit")
        .accelerator("<Primary>q")
        .build();

    let shortcut_shortcuts = gtk::ShortcutsShortcut::builder()
        .title("Keyboard Shortcuts")
        .accelerator("<Primary>question")
        .build();

    group.add_shortcut(&shortcut_quit);
    group.add_shortcut(&shortcut_shortcuts);
    section.add_group(&group);
    shortcuts.add_section(&section);
    shortcuts.set_visible(true);
}

/// Send a desktop notification via GApplication.
/// Notifications appear in the system notification area if the app is backgrounded.
fn send_notification(id: &str, title: &str, body: &str) {
    let app = relm4::main_application();
    let notification = gtk::gio::Notification::new(title);
    notification.set_body(Some(body));
    notification.set_icon(&gtk::gio::ThemedIcon::new(
        "software-update-available-symbolic",
    ));
    app.send_notification(Some(id), &notification);
}

// Action group and actions for the window-level menu.
relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");
relm4::new_stateless_action!(PreferencesAction, WindowActionGroup, "preferences");
relm4::new_stateless_action!(QuitAction, WindowActionGroup, "quit");
relm4::new_stateless_action!(ShortcutsAction, WindowActionGroup, "show-shortcuts");
relm4::new_stateless_action!(RebaseAction, WindowActionGroup, "rebase-history");
relm4::new_stateless_action!(SimSuccessAction, WindowActionGroup, "sim-success");
relm4::new_stateless_action!(SimFailureAction, WindowActionGroup, "sim-failure");
relm4::new_stateless_action!(SimUpToDateAction, WindowActionGroup, "sim-uptodate");
relm4::new_stateful_action!(DeveloperModeAction, WindowActionGroup, "dev-mode", (), bool);
