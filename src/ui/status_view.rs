//! Status view component — the main content area of the app.
//!
//! Pattern: State-driven view switching
//! Uses a `gtk::Stack` to switch between different visual states:
//! - Idle: Card-based overview with hero, update banner, and settings actions
//! - Updating: Progress indicator + image badge + UpdateList + live log + timer + cancel
//! - Complete: Success status page with reboot option
//! - UpToDate: "You're already up to date" status page
//! - Error: Error status page with retry option

use adw::prelude::*;
use relm4::prelude::*;
use serde_json::Value;
use std::process::Command;
use std::time::Instant;

use crate::app::{AppState, PreflightStatus};
use crate::settings::Settings;
use crate::ui::log_view::{LogView, LogViewInput};
use crate::ui::segmented_progress::{SegmentedProgress, same_segment};
use crate::ui::update_list::{UpdateList, UpdateListInput};
use crate::registry_client::ImageVersion;

/// Mock deployment representation for the collapsible version history list.
#[derive(Debug, Clone)]
pub struct MockDeployment {
    pub id: String,
    pub state: String, // "current" | "staged" | "previous" | "archived"
    pub title: String,
    pub image: String,
    pub tag: String,
    pub digest: String,
    pub deployed: String,
    pub deployed_full: String,
    pub size: String,
    pub kernel: String,
    pub package_count: u32,
    pub signer: String,
    pub pinned: bool,
}

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
    /// Dismiss the staged reboot banner.
    DismissBanner,
    /// Copy log to clipboard.
    CopyLog,
    /// Navigate stack to a page name
    ShowPage(String),
    /// Start registry URI editing
    EditRegistryUri,
    /// Save registry URI
    SaveRegistryUri(String),
    /// Cancel registry URI editing
    CancelRegistryUri,
    /// Select tag in Image Source
    SelectTag(String),
    /// Toggle pinned status of history deployment
    TogglePin(String),
    /// Roll back to a specific deployment
    RollbackTo(MockDeployment),
    /// Confirm rollback
    ConfirmRollback,
    /// Set a deployment as default boot
    SetDefaultBoot(MockDeployment),
    /// Select a version in Changelog
    SelectChangelogVersion(String),
    /// Registry versions loaded in background
    RegistryVersionsLoaded(Vec<crate::registry_client::ImageVersion>),
    /// Github commits loaded in background
    GithubCommitsLoaded(Vec<(String, String, String)>),
    /// SBOM package diff loaded in background
    SbomDiffLoaded(crate::sbom_diff::SbomDiffResult),
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
    /// User wants to open the update check dialog.
    OpenCheckDialog,
    /// Notify parent when page changes
    PageChanged(String),
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
    /// Root widget for the idle page.
    idle_page: gtk::ScrolledWindow,
    /// Hero row showing the current image summary.
    hero_row: adw::ActionRow,
    /// Status pill shown in the hero row suffix.
    status_pill: gtk::Label,
    /// Banner group shown when action is needed.
    update_banner_group: adw::PreferencesGroup,
    /// Banner row with dynamic title/subtitle.
    banner_title_row: adw::ActionRow,
    /// Banner install button.
    banner_install_btn: gtk::Button,
    /// Banner whats new button.
    banner_whats_new_btn: gtk::Button,
    /// Banner restart button.
    banner_restart_btn: gtk::Button,
    /// Banner discard button.
    banner_discard_btn: gtk::Button,
    /// Automatic updates toggle in the settings card.
    auto_update_switch: gtk::Switch,
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
    /// Whether an update has been staged and needs a reboot.
    reboot_pending: bool,

    // Redesigned settings & subpage state variables
    registry_uri: String,
    registry_editing: bool,
    selected_tag: String,
    deployments: Vec<MockDeployment>,
    expanded_deployment_id: Option<String>,
    changelog_version: String,
    registry_versions: Vec<crate::registry_client::ImageVersion>,
    github_commits: Vec<(String, String, String)>,
    sbom_diff: Option<crate::sbom_diff::SbomDiffResult>,

    // Redesigned settings UI widget references for dynamic updates
    registry_label: gtk::Label,
    registry_row_sub: gtk::Label,
    registry_edit_box: gtk::Box,
    registry_entry: gtk::Entry,
    tag_combo: gtk::ComboBoxText,
    tag_row: adw::ActionRow,
    history_list_box: gtk::ListBox,
    images_count_label: gtk::Label,
    changelog_box: gtk::Box,
    changelog_version_label: gtk::Label,
    changelog_date_label: gtk::Label,
    changelog_summary_label: gtk::Label,
    changelog_diff_box: gtk::Box,
    changelog_removed_box: gtk::Box,
    changelog_commit_box: gtk::Box,
    changelog_install_bar: gtk::Box,

    // Dialog rollback state
    rollback_target: Option<MockDeployment>,
    reg_edit_btn: gtk::Button,
    changelog_v_buttons: Vec<gtk::Button>,
}

impl StatusView {
    fn hero_title(&self) -> String {
        self.image_info
            .clone()
            .unwrap_or_else(|| "Fedora bootc".to_string())
    }

    fn idle_subtitle(&self) -> String {
        if self.reboot_pending {
            "Reboot to update".to_string()
        } else {
            self.last_update_text
                .clone()
                .unwrap_or_else(|| "Booted 3 days ago".to_string())
        }
    }

    fn refresh_idle_description(&self) {
        self.hero_row.set_title(&self.hero_title());
        
        let tag = if self.reboot_pending { "43" } else { &self.selected_tag };
        self.hero_row.set_subtitle(&format!("Version {}  ·  {}", tag, self.idle_subtitle()));

        for class in ["status-pill-ready", "status-pill-ok", "status-pill-staged", "dim-label"] {
            self.status_pill.remove_css_class(class);
        }

        let (pill_text, pill_class) = if self.reboot_pending {
            ("Staged", "status-pill-staged")
        } else {
            match self.preflight_status {
                PreflightStatus::UpdateAvailable => ("Update ready", "status-pill-ready"),
                PreflightStatus::UpToDate => ("Up to date", "status-pill-ok"),
                PreflightStatus::Checking => ("Checking", "dim-label"),
                PreflightStatus::Unknown => ("Ready", "dim-label"),
            }
        };
        self.status_pill.set_label(pill_text);
        self.status_pill.add_css_class(pill_class);

        if self.reboot_pending {
            self.update_banner_group.set_visible(true);
            self.banner_title_row.set_title("Reboot to finish updating");
            self.banner_title_row
                .set_subtitle("Next boot uses version 43.");
            self.banner_install_btn.set_visible(false);
            self.banner_whats_new_btn.set_visible(false);
            self.banner_restart_btn.set_visible(true);
            self.banner_discard_btn.set_visible(true);
        } else if matches!(self.preflight_status, PreflightStatus::UpdateAvailable) {
            self.update_banner_group.set_visible(true);
            self.banner_title_row.set_title("Version 43 available");
            self.banner_title_row
                .set_subtitle("Includes a kernel update. 412 MB.");
            self.banner_install_btn.set_visible(true);
            self.banner_whats_new_btn.set_visible(true);
            self.banner_restart_btn.set_visible(false);
            self.banner_discard_btn.set_visible(false);
        } else {
            self.update_banner_group.set_visible(false);
        }
    }

    fn rebuild_changelog_page(&self, sender: &ComponentSender<StatusView>) {
        while let Some(child) = self.changelog_box.first_child() {
            self.changelog_box.remove(&child);
        }

        let version = self.changelog_version.as_str();

        // 1. Add version switcher (pills) at the very top of self.changelog_box
        let version_selector = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        version_selector.set_halign(gtk::Align::Center);
        version_selector.add_css_class("linked");

        if !self.registry_versions.is_empty() {
            // Display recent real versions (up to 4)
            for v in self.registry_versions.iter().rev().take(4) {
                let label = v.date.format("%m-%d").to_string(); // e.g. "05-27"
                let btn = gtk::Button::with_label(&label);
                let btn_sender = sender.input_sender().clone();
                let v_str = v.version.clone();
                btn.connect_clicked(move |_| {
                    btn_sender.emit(StatusViewInput::SelectChangelogVersion(v_str.clone()));
                });
                if v.version == self.changelog_version {
                    btn.add_css_class("suggested-action");
                }
                version_selector.append(&btn);
            }
        } else {
            // Fallback: display mock versions as dated tags
            let mock_versions = [("43", "05-27"), ("42", "05-24"), ("41", "04-15")];
            for (mv_key, mv_label) in &mock_versions {
                let btn = gtk::Button::with_label(mv_label);
                let btn_sender = sender.input_sender().clone();
                let mv_str = mv_key.to_string();
                btn.connect_clicked(move |_| {
                    btn_sender.emit(StatusViewInput::SelectChangelogVersion(mv_str.clone()));
                });
                if *mv_key == self.changelog_version {
                    btn.add_css_class("suggested-action");
                }
                version_selector.append(&btn);
            }
        }
        self.changelog_box.append(&version_selector);

        // 2. Find the selected version details
        let mut real_version: Option<&ImageVersion> = None;
        if !self.registry_versions.is_empty() {
            real_version = self.registry_versions.iter().find(|v| v.version == self.changelog_version);
        }

        let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header_box.set_margin_top(12);
        
        let info_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        info_box.set_hexpand(true);

        let tag_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        
        let tag_code = gtk::Label::builder()
            .label(&format!("{}:{}", self.registry_uri, version))
            .halign(gtk::Align::Start)
            .build();
        tag_code.add_css_class("monospace");
        tag_box.append(&tag_code);

        // Pills in header
        let is_update = if let Some(v) = real_version {
            let booted_tag = read_selected_tag();
            v.version != booted_tag && !self.reboot_pending && matches!(self.preflight_status, PreflightStatus::UpdateAvailable)
        } else {
            version == "43" && !self.reboot_pending && matches!(self.preflight_status, PreflightStatus::UpdateAvailable)
        };

        if is_update {
            let update_pill = gtk::Label::new(Some("Update"));
            update_pill.add_css_class("status-pill-ready");
            update_pill.add_css_class("caption");
            tag_box.append(&update_pill);
        } else {
            let is_booted = if let Some(v) = real_version {
                let booted_tag = read_selected_tag();
                v.version == booted_tag
            } else {
                version == "42" && !self.reboot_pending
            };
            if is_booted {
                let booted_pill = gtk::Label::new(Some("✓ Booted"));
                booted_pill.add_css_class("status-pill-ok");
                booted_pill.add_css_class("caption");
                tag_box.append(&booted_pill);
            }
        }
        info_box.append(&tag_box);

        let stable_str = if let Some(v) = real_version {
            v.version.clone()
        } else {
            match version {
                "43" => "stable-20260527".to_string(),
                "42" => "stable-20260524".to_string(),
                "41" => "stable-20260415".to_string(),
                _ => "".to_string(),
            }
        };
        let date_str = if let Some(v) = real_version {
            v.created.format("%B %-d, %Y").to_string()
        } else {
            match version {
                "43" => "May 27, 2026".to_string(),
                "42" => "May 24, 2026".to_string(),
                "41" => "Apr 15, 2026".to_string(),
                _ => "".to_string(),
            }
        };
        let meta_label = gtk::Label::builder()
            .label(&format!("{}  ·  {}", stable_str, date_str))
            .halign(gtk::Align::Start)
            .build();
        meta_label.add_css_class("caption");
        meta_label.add_css_class("dim-label");
        info_box.append(&meta_label);

        let summary_str = if let Some(v) = real_version {
            let booted_tag = read_selected_tag();
            if v.version == booted_tag {
                format!("Currently booted. Kernel {} · stable point release.", v.kernel)
            } else {
                format!("Image build. Kernel {} · git commit {}.", v.kernel, if v.revision.len() >= 7 { &v.revision[0..7] } else { &v.revision })
            }
        } else {
            match version {
                "43" => "Fedora 43 base bump · new kernel · GNOME 50.1".to_string(),
                "42" => "Currently booted. Kernel 6.13.7 · stable point release.".to_string(),
                "41" => "Final Fedora 41 maintenance build.".to_string(),
                _ => "".to_string(),
            }
        };
        let summary_label = gtk::Label::builder()
            .label(&summary_str)
            .halign(gtk::Align::Start)
            .wrap(true)
            .max_width_chars(60)
            .build();
        summary_label.add_css_class("body");
        info_box.append(&summary_label);

        header_box.append(&info_box);

        if is_update {
            let install_btn = gtk::Button::builder()
                .label("Install")
                .icon_name("object-select-symbolic")
                .build();
            install_btn.add_css_class("suggested-action");
            install_btn.add_css_class("pill");
            install_btn.set_valign(gtk::Align::Center);
            let out_sender = sender.output_sender().clone();
            install_btn.connect_clicked(move |_| {
                let _ = out_sender.send(StatusViewOutput::StartUpdate);
            });
            header_box.append(&install_btn);
        }

        self.changelog_box.append(&header_box);

        let stack_title = gtk::Label::builder()
            .label("Stack")
            .halign(gtk::Align::Start)
            .margin_top(12)
            .build();
        stack_title.add_css_class("caption");
        stack_title.add_css_class("dim-label");
        self.changelog_box.append(&stack_title);

        let grid = gtk::FlowBox::new();
        grid.set_selection_mode(gtk::SelectionMode::None);
        grid.set_max_children_per_line(3);
        grid.set_min_children_per_line(2);
        grid.set_column_spacing(8);
        grid.set_row_spacing(8);

        let stack_items: Vec<(&str, String, bool)> = if let Some(v) = real_version {
            vec![
                ("Kernel", v.kernel.clone(), false),
                ("bootc", "1.4.2".to_string(), false),
                ("systemd", "259.5".to_string(), false),
                ("flatpak", "1.17.7".to_string(), false),
            ]
        } else {
            match version {
                "43" => vec![
                    ("Kernel", "6.14.1-300.fc43".to_string(), true),
                    ("GNOME", "50.1".to_string(), true),
                    ("Mesa", "26.0.7".to_string(), false),
                    ("Podman", "5.8.2".to_string(), true),
                    ("Nvidia", "595.71.05-1".to_string(), false),
                    ("bootc", "1.4.2".to_string(), false),
                    ("systemd", "259.5".to_string(), false),
                    ("pipewire", "1.6.5".to_string(), true),
                    ("flatpak", "1.17.7".to_string(), false),
                ],
                "42" => vec![
                    ("Kernel", "6.13.7-300.fc42".to_string(), false),
                    ("GNOME", "49.4".to_string(), false),
                    ("Mesa", "26.0.6".to_string(), false),
                    ("Podman", "5.8.0".to_string(), false),
                    ("Nvidia", "595.71.05-1".to_string(), false),
                    ("bootc", "1.4.2".to_string(), false),
                    ("systemd", "259.5".to_string(), false),
                    ("pipewire", "1.6.5".to_string(), false),
                    ("flatpak", "1.17.7".to_string(), false),
                ],
                "41" => vec![
                    ("Kernel", "6.12.9-100.fc41".to_string(), false),
                    ("GNOME", "48.2".to_string(), false),
                    ("Mesa", "25.3.4".to_string(), false),
                    ("Podman", "5.6.0".to_string(), false),
                ],
                _ => vec![],
            }
        };

        for (name, ver, bumped) in stack_items {
            let pill_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            pill_box.add_css_class("card");
            pill_box.set_margin_start(2);
            pill_box.set_margin_end(2);
            pill_box.set_margin_top(2);
            pill_box.set_margin_bottom(2);
            
            let lbl_name = gtk::Label::builder()
                .label(name)
                .halign(gtk::Align::Start)
                .margin_start(8)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            lbl_name.add_css_class("body");
            
            let lbl_ver_str = if bumped {
                format!("{} ↑", ver)
            } else {
                ver
            };
            let lbl_ver = gtk::Label::builder()
                .label(&lbl_ver_str)
                .halign(gtk::Align::End)
                .hexpand(true)
                .margin_end(8)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            lbl_ver.add_css_class("monospace");
            lbl_ver.add_css_class("caption");
            if bumped {
                lbl_ver.add_css_class("success");
            } else {
                lbl_ver.add_css_class("dim-label");
            }
            
            pill_box.append(&lbl_name);
            pill_box.append(&lbl_ver);
            
            grid.append(&pill_box);
        }
        self.changelog_box.append(&grid);

        let mut upgrades_list: Vec<(String, String, String)> = Vec::new();
        let mut removals_list: Vec<String> = Vec::new();

        if let Some(ref diff) = self.sbom_diff {
            for pkg in &diff.upgraded {
                upgrades_list.push((pkg.name.clone(), pkg.old_version.clone(), pkg.new_version.clone()));
            }
            for pkg in &diff.added {
                upgrades_list.push((pkg.name.clone(), "(added)".to_string(), pkg.new_version.clone()));
            }
            for pkg in &diff.removed {
                removals_list.push(pkg.clone());
            }
        } else {
            // Fallback: mock upgrades and removals
            let mock_upgrades = match version {
                "43" => vec![
                    ("Docker", "29.5.1-1", "29.5.2-1"),
                    ("Mesa", "26.0.6-4", "26.0.7-4"),
                    ("amd-gpu-firmware", "20260410-1", "20260519-1"),
                    ("bind", "9.18.48-1", "9.18.49-1"),
                    ("pipewire", "1.6.5-1", "1.6.5-2"),
                    ("tailscale", "1.98.2-1", "1.98.3-1"),
                    ("vim", "9.2.390-1", "9.2.506-2"),
                ],
                "42" => vec![
                    ("kernel", "6.13.6", "6.13.7-300"),
                    ("firefox", "142.0", "143.0"),
                    ("gnome-shell", "49.3", "49.4"),
                ],
                "41" => vec![
                    ("kernel", "6.12.4", "6.12.9-100"),
                    ("gnome-shell", "48.1", "48.2"),
                ],
                _ => vec![],
            };
            for (pkg, from, to) in mock_upgrades {
                upgrades_list.push((pkg.to_string(), from.to_string(), to.to_string()));
            }

            let mock_removals = match version {
                "43" => vec![
                    "framework-laptop-kmod-common",
                    "libde265",
                    "uvg266",
                    "vvdec",
                ],
                _ => vec![],
            };
            for pkg in mock_removals {
                removals_list.push(pkg.to_string());
            }
        }

        if !upgrades_list.is_empty() {
            let upgrades_title = gtk::Label::builder()
                .label(&format!("Updated  ·  {}", upgrades_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            upgrades_title.add_css_class("caption");
            upgrades_title.add_css_class("dim-label");
            self.changelog_box.append(&upgrades_title);

            let list_upgrades = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_upgrades.add_css_class("card");

            for (pkg, from, to) in upgrades_list {
                let row = adw::ActionRow::builder()
                    .title(&pkg)
                    .build();
                
                let val_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                
                let from_lbl = gtk::Label::new(Some(&from));
                from_lbl.add_css_class("dim-label");
                from_lbl.add_css_class("monospace");
                from_lbl.add_css_class("caption");
                
                let arr_lbl = gtk::Label::new(Some("→"));
                arr_lbl.add_css_class("dim-label");
                
                let to_lbl = gtk::Label::new(Some(&to));
                to_lbl.add_css_class("monospace");
                to_lbl.add_css_class("caption");
                
                val_box.append(&from_lbl);
                val_box.append(&arr_lbl);
                val_box.append(&to_lbl);
                
                row.add_suffix(&val_box);
                list_upgrades.append(&row);
            }
            self.changelog_box.append(&list_upgrades);
        }

        if !removals_list.is_empty() {
            let removals_title = gtk::Label::builder()
                .label(&format!("Removed  ·  {}", removals_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            removals_title.add_css_class("caption");
            removals_title.add_css_class("dim-label");
            self.changelog_box.append(&removals_title);

            let list_removals = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_removals.add_css_class("card");

            for pkg in removals_list {
                let row = adw::ActionRow::builder()
                    .title(&pkg)
                    .build();
                let dash_lbl = gtk::Label::new(Some("−"));
                dash_lbl.add_css_class("error");
                row.add_prefix(&dash_lbl);
                list_removals.append(&row);
            }
            self.changelog_box.append(&list_removals);
        }

        let commits_list: Vec<(String, String, String)> = if !self.github_commits.is_empty() {
            self.github_commits.clone()
        } else {
            match version {
                "43" => vec![
                    ("60e72be".to_string(), "fix: ensure xdg-desktop-portal starts after gnome-keyring-daemon (#4539)".to_string(), "Yang Ye".to_string()),
                    ("b6a0f5c".to_string(), "feat(changelogs): Use SBOMs for standardized package data extraction (#4635)".to_string(), "Dylan M. Taylor".to_string()),
                    ("7036e3e".to_string(), "Adds ROCm Info utility (#4661)".to_string(), "Italo".to_string()),
                    ("7eef3d2".to_string(), "feat(extension): Add gradia capture extension (#4651)".to_string(), "Coda".to_string()),
                ],
                "42" => vec![
                    ("12ab34c".to_string(), "chore: weekly Fedora 42 base refresh".to_string(), "fedora-bootc bot".to_string()),
                ],
                _ => vec![],
            }
        };

        if !commits_list.is_empty() {
            let commits_title = gtk::Label::builder()
                .label("Commits")
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            commits_title.add_css_class("caption");
            commits_title.add_css_class("dim-label");
            self.changelog_box.append(&commits_title);

            let list_commits = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_commits.add_css_class("card");

            for (sha, msg, author) in commits_list {
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                row_box.set_margin_start(16);
                row_box.set_margin_end(16);
                row_box.set_margin_top(8);
                row_box.set_margin_bottom(8);

                let sha_lbl = gtk::Label::new(Some(&sha));
                sha_lbl.add_css_class("monospace");
                sha_lbl.add_css_class("caption");
                sha_lbl.add_css_class("dim-label");
                sha_lbl.set_valign(gtk::Align::Start);
                row_box.append(&sha_lbl);

                let msg_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
                msg_box.set_hexpand(true);
                
                let msg_lbl = gtk::Label::builder()
                    .label(&msg)
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .build();
                msg_lbl.add_css_class("body");
                msg_box.append(&msg_lbl);

                let auth_lbl = gtk::Label::builder()
                    .label(&author)
                    .halign(gtk::Align::Start)
                    .build();
                auth_lbl.add_css_class("caption");
                auth_lbl.add_css_class("dim-label");
                msg_box.append(&auth_lbl);

                row_box.append(&msg_box);
                
                list_commits.append(&row_box);
            }
            self.changelog_box.append(&list_commits);
        }

        if is_update {
            self.changelog_install_bar.set_visible(true);
        } else {
            self.changelog_install_bar.set_visible(false);
        }
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
            add_child = &model.idle_page.clone() -> gtk::ScrolledWindow {} -> {
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
        let initial_image_info = read_image_info();
        let initial_registry_uri = read_registry_uri().unwrap_or_else(|| "quay.io/fedora/fedora-bootc".to_string());
        let initial_selected_tag = read_selected_tag();
        let initial_last_update = get_last_update_time();
        let auto_updates_enabled = read_auto_updates_enabled();

        let idle_page = gtk::ScrolledWindow::new();
        idle_page.set_hscrollbar_policy(gtk::PolicyType::Never);
        idle_page.set_vscrollbar_policy(gtk::PolicyType::Automatic);
        idle_page.set_vexpand(true);

        let idle_clamp = adw::Clamp::new();
        idle_clamp.set_maximum_size(600);
        idle_clamp.set_tightening_threshold(400);
        idle_page.set_child(Some(&idle_clamp));

        let idle_content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        idle_content.set_margin_start(24);
        idle_content.set_margin_end(24);
        idle_content.set_margin_top(24);
        idle_content.set_margin_bottom(24);
        idle_clamp.set_child(Some(&idle_content));

        let hero_group = adw::PreferencesGroup::new();
        let hero_row = adw::ActionRow::builder()
            .title(initial_image_info.as_deref().unwrap_or("Fedora bootc"))
            .subtitle("Version 42  ·  Booted 3 days ago")
            .build();
        hero_row.set_activatable(false);
        
        let hero_logo_box = gtk::Box::builder()
            .css_classes(vec!["hero-logo-box".to_string()])
            .build();
        let logo_name = read_logo_icon_name();
        let hero_icon = gtk::Image::from_icon_name(&logo_name);
        hero_icon.set_pixel_size(32);
        hero_logo_box.append(&hero_icon);
        hero_row.add_prefix(&hero_logo_box);

        let status_pill = gtk::Label::new(Some("Checking"));
        status_pill.add_css_class("caption");
        status_pill.add_css_class("pill");
        status_pill.add_css_class("dim-label");
        status_pill.set_valign(gtk::Align::Center);
        hero_row.add_suffix(&status_pill);
        hero_group.add(&hero_row);
        idle_content.append(&hero_group);

        let update_banner_group = adw::PreferencesGroup::new();
        let banner_title_row = adw::ActionRow::builder()
            .title("Update available")
            .subtitle("A new system image is ready to install.")
            .build();
        banner_title_row.set_activatable(false);
        
        let banner_icon_box = gtk::Box::builder()
            .css_classes(vec!["update-banner-icon".to_string()])
            .build();
        let banner_icon = gtk::Image::from_icon_name("software-update-available-symbolic");
        banner_icon.set_pixel_size(24);
        banner_icon_box.append(&banner_icon);
        banner_title_row.add_prefix(&banner_icon_box);

        let banner_action_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let banner_whats_new_btn = gtk::Button::with_label("What's new");
        banner_whats_new_btn.add_css_class("flat");
        banner_whats_new_btn.add_css_class("accent");
        let whats_new_sender = sender.input_sender().clone();
        banner_whats_new_btn.connect_clicked(move |_| {
            whats_new_sender.emit(StatusViewInput::SelectChangelogVersion("43".to_string()));
        });

        let banner_install_btn = gtk::Button::with_label("Install");
        banner_install_btn.add_css_class("suggested-action");
        banner_install_btn.add_css_class("pill");
        let install_sender = sender.output_sender().clone();
        banner_install_btn.connect_clicked(move |_| {
            let _ = install_sender.send(StatusViewOutput::StartUpdate);
        });

        let banner_restart_btn = gtk::Button::with_label("Restart");
        banner_restart_btn.add_css_class("suggested-action");
        banner_restart_btn.add_css_class("pill");
        let restart_sender = sender.output_sender().clone();
        banner_restart_btn.connect_clicked(move |_| {
            let _ = restart_sender.send(StatusViewOutput::Reboot);
        });

        let banner_discard_btn = gtk::Button::with_label("Discard");
        banner_discard_btn.add_css_class("flat");
        let discard_sender = sender.input_sender().clone();
        banner_discard_btn.connect_clicked(move |_| {
            discard_sender.emit(StatusViewInput::DismissBanner);
        });

        banner_action_box.append(&banner_whats_new_btn);
        banner_action_box.append(&banner_install_btn);
        banner_action_box.append(&banner_restart_btn);
        banner_action_box.append(&banner_discard_btn);
        banner_title_row.add_suffix(&banner_action_box);
        update_banner_group.add(&banner_title_row);
        update_banner_group.set_visible(false);
        idle_content.append(&update_banner_group);

        // Boxed List Settings Card (Left sidebar settings style)
        let check_row = adw::ActionRow::builder()
            .title("Check for updates")
            .subtitle("System image, Flatpak, Homebrew, and Distrobox")
            .build();
        let check_btn = gtk::Button::with_label("Check");
        check_btn.add_css_class("pill");
        check_btn.set_valign(gtk::Align::Center);
        let check_sender = sender.output_sender().clone();
        check_btn.connect_clicked(move |_| {
            let _ = check_sender.send(StatusViewOutput::OpenCheckDialog);
        });
        check_row.add_suffix(&check_btn);

        let auto_row = adw::ActionRow::builder()
            .title("Automatic updates")
            .build();
        let auto_update_switch = gtk::Switch::builder()
            .active(auto_updates_enabled)
            .valign(gtk::Align::Center)
            .build();
        auto_update_switch.connect_state_set(move |switch, _| {
            apply_auto_updates_setting(switch.is_active());
            gtk::glib::Propagation::Proceed
        });
        auto_row.add_suffix(&auto_update_switch);

        // Registry/Image source row
        let source_row = adw::ActionRow::builder()
            .title("Image source")
            .activatable(true)
            .build();
        let source_sub_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let registry_row_sub = gtk::Label::new(Some(&format!("{}:{}", initial_registry_uri, initial_selected_tag)));
        registry_row_sub.add_css_class("dim-label");
        let source_chev = gtk::Image::from_icon_name("go-next-symbolic");
        source_chev.add_css_class("dim-label");
        source_sub_box.append(&registry_row_sub);
        source_sub_box.append(&source_chev);
        source_row.add_suffix(&source_sub_box);
        let source_sender = sender.input_sender().clone();
        source_row.connect_activated(move |_| {
            source_sender.emit(StatusViewInput::ShowPage("source".to_string()));
        });

        // Image history row
        let history_row = adw::ActionRow::builder()
            .title("Image history")
            .activatable(true)
            .build();
        let history_sub_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let images_count_label = gtk::Label::new(Some("3 versions"));
        images_count_label.add_css_class("dim-label");
        let history_chev = gtk::Image::from_icon_name("go-next-symbolic");
        history_chev.add_css_class("dim-label");
        history_sub_box.append(&images_count_label);
        history_sub_box.append(&history_chev);
        history_row.add_suffix(&history_sub_box);
        let history_sender = sender.input_sender().clone();
        history_row.connect_activated(move |_| {
            history_sender.emit(StatusViewInput::ShowPage("history".to_string()));
        });

        let settings_card = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .margin_bottom(12)
            .build();
        settings_card.add_css_class("card");
        settings_card.append(&check_row);
        settings_card.append(&auto_row);
        settings_card.append(&source_row);
        settings_card.append(&history_row);
        idle_content.append(&settings_card);

        // Reset Card (Powerwash & Factory reset)
        let powerwash_row = adw::ActionRow::builder()
            .title("Powerwash")
            .subtitle("Reset settings and apps. Keep your files.")
            .activatable(true)
            .build();
        let pw_chev = gtk::Image::from_icon_name("go-next-symbolic");
        pw_chev.add_css_class("dim-label");
        powerwash_row.add_suffix(&pw_chev);
        let pw_sender = sender.input_sender().clone();
        powerwash_row.connect_activated(move |_| {
            pw_sender.emit(StatusViewInput::TogglePin("powerwash".to_string()));
        });

        let factory_row = adw::ActionRow::builder()
            .title("Factory reset")
            .subtitle("Erase everything and start fresh.")
            .activatable(true)
            .build();
        factory_row.add_css_class("destructive-title");
        let fact_chev = gtk::Image::from_icon_name("go-next-symbolic");
        fact_chev.add_css_class("dim-label");
        factory_row.add_suffix(&fact_chev);
        let fact_sender = sender.input_sender().clone();
        factory_row.connect_activated(move |_| {
            fact_sender.emit(StatusViewInput::TogglePin("factory".to_string()));
        });

        let reset_card = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .margin_bottom(12)
            .build();
        reset_card.add_css_class("card");
        reset_card.append(&powerwash_row);
        reset_card.append(&factory_row);
        idle_content.append(&reset_card);

        // ── Image Source Subpage ──────────────────────────────────────────
        let source_page = gtk::ScrolledWindow::new();
        source_page.set_hscrollbar_policy(gtk::PolicyType::Never);
        source_page.set_vexpand(true);
        let source_clamp = adw::Clamp::new();
        source_clamp.set_maximum_size(600);
        let source_content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        source_content.set_margin_start(24);
        source_content.set_margin_end(24);
        source_content.set_margin_top(24);
        source_content.set_margin_bottom(24);
        source_clamp.set_child(Some(&source_content));
        source_page.set_child(Some(&source_clamp));

        let source_desc = gtk::Label::new(Some("Where this device pulls its OS image from. Changes apply on next update."));
        source_desc.add_css_class("dim-label");
        source_desc.add_css_class("caption");
        source_desc.set_margin_bottom(12);
        source_content.append(&source_desc);

        let source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        source_list.add_css_class("card");

        // Registry row
        let reg_row = adw::ActionRow::builder()
            .title("Registry")
            .build();
        let registry_label = gtk::Label::new(Some(&initial_registry_uri));
        registry_label.add_css_class("monospace");
        registry_label.add_css_class("caption");
        let reg_edit_btn = gtk::Button::with_label("Change");
        reg_edit_btn.add_css_class("flat");
        reg_edit_btn.add_css_class("accent");
        let edit_sender = sender.input_sender().clone();
        reg_edit_btn.connect_clicked(move |_| {
            edit_sender.emit(StatusViewInput::EditRegistryUri);
        });
        reg_row.add_suffix(&registry_label);
        reg_row.add_suffix(&reg_edit_btn);
        source_list.append(&reg_row);

        // Registry Edit Input row
        let registry_entry = gtk::Entry::builder()
            .placeholder_text(&initial_registry_uri)
            .build();
        registry_entry.add_css_class("entry");
        registry_entry.set_hexpand(true);
        let reg_save_btn = gtk::Button::with_label("Save");
        reg_save_btn.add_css_class("suggested-action");
        reg_save_btn.add_css_class("pill");
        let reg_entry_clone = registry_entry.clone();
        let save_sender = sender.input_sender().clone();
        reg_save_btn.connect_clicked(move |_| {
            save_sender.emit(StatusViewInput::SaveRegistryUri(reg_entry_clone.text().to_string()));
        });
        let reg_cancel_btn = gtk::Button::with_label("Cancel");
        reg_cancel_btn.add_css_class("flat");
        let cancel_sender = sender.input_sender().clone();
        reg_cancel_btn.connect_clicked(move |_| {
            cancel_sender.emit(StatusViewInput::CancelRegistryUri);
        });
        let registry_edit_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        registry_edit_box.set_margin_start(16);
        registry_edit_box.set_margin_end(16);
        registry_edit_box.set_margin_top(8);
        registry_edit_box.set_margin_bottom(8);
        registry_edit_box.set_visible(false);
        registry_edit_box.append(&registry_entry);
        registry_edit_box.append(&reg_cancel_btn);
        registry_edit_box.append(&reg_save_btn);
        
        let edit_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        edit_container.append(&registry_edit_box);
        source_list.append(&edit_container);

        // Tag row
        let tag_row = adw::ActionRow::builder()
            .title("Tag")
            .subtitle("Always the newest stable release")
            .build();
        let tag_combo = gtk::ComboBoxText::new();
        let tags = if let Some(config) = read_bootc_image_info_config() {
            config.tags
        } else {
            vec![
                "latest".to_string(),
                "42".to_string(),
                "41".to_string(),
                "testing".to_string(),
                "rawhide".to_string(),
            ]
        };

        for t in &tags {
            tag_combo.append(Some(t), &format!(":{}", t));
        }

        let active_tag = if tags.iter().any(|t| t == &initial_selected_tag) {
            Some(initial_selected_tag.as_str())
        } else {
            tags.first().map(|t| t.as_str())
        };
        tag_combo.set_active_id(active_tag);
        let select_sender = sender.input_sender().clone();
        tag_combo.connect_changed(move |combo| {
            if let Some(id) = combo.active_id() {
                select_sender.emit(StatusViewInput::SelectTag(id.to_string()));
            }
        });
        tag_combo.set_valign(gtk::Align::Center);
        tag_row.add_suffix(&tag_combo);
        source_list.append(&tag_row);

        // Signature row
        let sig_row = adw::ActionRow::builder()
            .title("Require signed images")
            .subtitle("Only install images signed by the publisher.")
            .build();
        let sig_badge = gtk::Label::new(Some("✓ On"));
        sig_badge.add_css_class("status-pill-ok");
        sig_badge.add_css_class("caption");
        sig_badge.set_valign(gtk::Align::Center);
        sig_row.add_suffix(&sig_badge);
        source_list.append(&sig_row);

        source_content.append(&source_list);
        root.add_named(&source_page, Some("source"));

        // ── Version History Subpage ──────────────────────────────────────
        let history_page = gtk::ScrolledWindow::new();
        history_page.set_hscrollbar_policy(gtk::PolicyType::Never);
        history_page.set_vexpand(true);
        let history_clamp = adw::Clamp::new();
        history_clamp.set_maximum_size(600);
        let history_content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        history_content.set_margin_start(24);
        history_content.set_margin_end(24);
        history_content.set_margin_top(24);
        history_content.set_margin_bottom(24);
        history_clamp.set_child(Some(&history_content));
        history_page.set_child(Some(&history_clamp));

        let history_desc = gtk::Label::new(Some("Past images stay on disk so you can roll back. Pin a version to keep it across upgrades."));
        history_desc.add_css_class("dim-label");
        history_desc.add_css_class("caption");
        history_desc.set_margin_bottom(12);
        history_content.append(&history_desc);

        let history_list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        history_list_box.add_css_class("card");
        history_content.append(&history_list_box);
        root.add_named(&history_page, Some("history"));

        // ── Changelogs Subpage ───────────────────────────────────────────
        let changelog_page = gtk::ScrolledWindow::new();
        changelog_page.set_hscrollbar_policy(gtk::PolicyType::Never);
        changelog_page.set_vexpand(true);
        let changelog_clamp = adw::Clamp::new();
        changelog_clamp.set_maximum_size(600);
        let changelog_content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        changelog_content.set_margin_start(24);
        changelog_content.set_margin_end(24);
        changelog_content.set_margin_top(24);
        changelog_content.set_margin_bottom(24);
        changelog_clamp.set_child(Some(&changelog_content));
        changelog_page.set_child(Some(&changelog_clamp));

        // Pills version switcher (built dynamically in rebuild_changelog_page)
        let changelog_v_buttons = Vec::new();

        let changelog_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        
        let changelog_version_label = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .build();
        changelog_version_label.add_css_class("title-3");
        
        let changelog_date_label = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .build();
        changelog_date_label.add_css_class("caption");
        changelog_date_label.add_css_class("dim-label");

        let changelog_summary_label = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .wrap(true)
            .max_width_chars(60)
            .build();
        changelog_summary_label.add_css_class("body");

        changelog_box.append(&changelog_version_label);
        changelog_box.append(&changelog_date_label);
        changelog_box.append(&changelog_summary_label);

        // Package upgrades (diffs)
        let diff_header = gtk::Label::new(Some("Upgraded packages"));
        diff_header.add_css_class("caption");
        diff_header.add_css_class("dim-label");
        diff_header.set_halign(gtk::Align::Start);
        diff_header.set_margin_top(12);
        changelog_box.append(&diff_header);

        let changelog_diff_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        changelog_diff_box.add_css_class("card");
        changelog_box.append(&changelog_diff_box);

        // Removed packages
        let removed_header = gtk::Label::new(Some("Removed packages"));
        removed_header.add_css_class("caption");
        removed_header.add_css_class("dim-label");
        removed_header.set_halign(gtk::Align::Start);
        removed_header.set_margin_top(12);
        changelog_box.append(&removed_header);

        let changelog_removed_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        changelog_removed_box.add_css_class("card");
        changelog_box.append(&changelog_removed_box);

        // Commit logs
        let commit_header = gtk::Label::new(Some("Commits"));
        commit_header.add_css_class("caption");
        commit_header.add_css_class("dim-label");
        commit_header.set_halign(gtk::Align::Start);
        commit_header.set_margin_top(12);
        changelog_box.append(&commit_header);

        let changelog_commit_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        changelog_commit_box.add_css_class("card");
        changelog_box.append(&changelog_commit_box);

        changelog_content.append(&changelog_box);

        // Dynamic Install Action bar on Changelog
        let changelog_install_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        changelog_install_bar.set_margin_top(12);
        changelog_install_bar.set_margin_bottom(12);
        let ch_install_label = gtk::Label::new(Some("Version 43 available · Includes a kernel update."));
        ch_install_label.add_css_class("caption");
        ch_install_label.add_css_class("dim-label");
        let ch_install_btn = gtk::Button::with_label("Install");
        ch_install_btn.add_css_class("suggested-action");
        ch_install_btn.add_css_class("pill");
        let ch_inst_sender = sender.output_sender().clone();
        ch_install_btn.connect_clicked(move |_| {
            let _ = ch_inst_sender.send(StatusViewOutput::StartUpdate);
        });
        changelog_install_bar.append(&ch_install_label);
        changelog_install_bar.append(&ch_install_btn);
        changelog_install_bar.set_visible(false);
        changelog_content.append(&changelog_install_bar);

        root.add_named(&changelog_page, Some("changelog"));

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

        spawn_changelog_fetch(initial_registry_uri.clone(), initial_selected_tag.clone(), sender.clone());

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
            hero_row,
            status_pill,
            update_banner_group,
            banner_title_row,
            banner_install_btn,
            banner_whats_new_btn,
            banner_restart_btn,
            banner_discard_btn,
            auto_update_switch,
            preflight_status: PreflightStatus::Checking,
            last_update_text: initial_last_update,
            image_info: initial_image_info,
            seg_progress,
            active_module: None,
            reboot_pending: false,

            registry_uri: initial_registry_uri.clone(),
            registry_editing: false,
            selected_tag: initial_selected_tag.clone(),
            deployments: get_sample_deployments(false),
            expanded_deployment_id: None,
            changelog_version: "43".to_string(),
            registry_versions: Vec::new(),
            github_commits: Vec::new(),
            sbom_diff: None,

            registry_label,
            registry_row_sub: registry_row_sub.clone(),
            registry_edit_box: registry_edit_box.clone(),
            registry_entry: registry_entry.clone(),
            tag_combo: tag_combo.clone(),
            tag_row: tag_row.clone(),
            history_list_box: history_list_box.clone(),
            images_count_label,
            changelog_box: changelog_box.clone(),
            changelog_version_label: changelog_version_label.clone(),
            changelog_date_label: changelog_date_label.clone(),
            changelog_summary_label: changelog_summary_label.clone(),
            changelog_diff_box: changelog_diff_box.clone(),
            changelog_removed_box: changelog_removed_box.clone(),
            changelog_commit_box: changelog_commit_box.clone(),
            changelog_install_bar: changelog_install_bar.clone(),
            rollback_target: None,
            reg_edit_btn: reg_edit_btn.clone(),
            changelog_v_buttons,
        };

        let widgets = view_output!();

        // Set initial idle description and visible page.
        model.refresh_idle_description();
        root.set_visible_child_name("idle");

        rebuild_history_list(
            &model.history_list_box,
            &model.deployments,
            model.expanded_deployment_id.as_deref(),
            &sender,
        );
        model.images_count_label.set_label(&format!("{} images", model.deployments.len()));
        model.rebuild_changelog_page(&sender);

        for btn in &model.changelog_v_buttons {
            if btn.label().as_deref() == Some(model.changelog_version.as_str()) {
                btn.add_css_class("suggested-action");
            } else {
                btn.remove_css_class("suggested-action");
            }
        }

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

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
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
                        self.reboot_pending = false;
                    }
                    AppState::Complete => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkAllComplete);
                        self.seg_progress.mark_all_complete();
                        self.active_module = None;
                        self.preflight_status = PreflightStatus::UpToDate;
                        self.reboot_pending = true;
                        self.refresh_idle_description();
                        self.deployments = get_sample_deployments(true);
                        rebuild_history_list(
                            &self.history_list_box,
                            &self.deployments,
                            self.expanded_deployment_id.as_deref(),
                            &sender,
                        );
                        self.images_count_label.set_label(&format!("{} images", self.deployments.len()));
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
                        self.preflight_status = PreflightStatus::UpToDate;
                        self.reboot_pending = false;
                        self.refresh_idle_description();
                    }
                    AppState::Idle => {
                        self.update_start = None;
                        self.last_update_text = get_last_update_time();
                        self.image_info = read_image_info();
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

            StatusViewInput::DismissBanner => {
                self.reboot_pending = false;
                self.preflight_status = PreflightStatus::UpToDate;
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

            StatusViewInput::ShowPage(page) => {
                let target = if page == "main" || page == "idle" {
                    "idle"
                } else {
                    &page
                };
                self.stack.set_visible_child_name(target);
                let _ = sender.output(StatusViewOutput::PageChanged(page));
            }

            StatusViewInput::EditRegistryUri => {
                self.registry_editing = true;
                self.registry_edit_box.set_visible(true);
                self.reg_edit_btn.set_visible(false);
                self.registry_entry.set_text(&self.registry_uri);
            }

            StatusViewInput::SaveRegistryUri(uri) => {
                if !uri.trim().is_empty() {
                    self.registry_uri = uri;
                    self.registry_label.set_label(&self.registry_uri);
                    
                    let name = self.registry_uri.split('/').last().unwrap_or(&self.registry_uri);
                    self.registry_row_sub.set_label(&format!("{}:{}", name, self.selected_tag));

                    let toast = adw::Toast::new("Image source updated");
                    self.toast_overlay.add_toast(toast);

                    spawn_changelog_fetch(self.registry_uri.clone(), self.selected_tag.clone(), sender.clone());
                }
                self.registry_editing = false;
                self.registry_edit_box.set_visible(false);
                self.reg_edit_btn.set_visible(true);
            }

            StatusViewInput::CancelRegistryUri => {
                self.registry_editing = false;
                self.registry_edit_box.set_visible(false);
                self.reg_edit_btn.set_visible(true);
            }

            StatusViewInput::SelectTag(tag) => {
                self.selected_tag = tag.clone();
                let desc = match tag.as_str() {
                    "latest" => "Always the newest stable release",
                    "42" => "Pinned to Fedora 42",
                    "41" => "Pinned to Fedora 41 (older)",
                    "testing" => "Pre-release builds — may be unstable",
                    "rawhide" => "Bleeding edge — not for daily use",
                    _ => "Custom tag",
                };
                self.tag_row.set_subtitle(desc);

                let name = self.registry_uri.split('/').last().unwrap_or(&self.registry_uri);
                self.registry_row_sub.set_label(&format!("{}:{}", name, self.selected_tag));

                let toast = adw::Toast::new(&format!("Tag set to :{}", tag));
                self.toast_overlay.add_toast(toast);

                spawn_changelog_fetch(self.registry_uri.clone(), self.selected_tag.clone(), sender.clone());
            }

            StatusViewInput::TogglePin(action) => {
                if let Some(id) = action.strip_prefix("expand:") {
                    if self.expanded_deployment_id.as_deref() == Some(id) {
                        self.expanded_deployment_id = None;
                    } else {
                        self.expanded_deployment_id = Some(id.to_string());
                    }
                    rebuild_history_list(
                        &self.history_list_box,
                        &self.deployments,
                        self.expanded_deployment_id.as_deref(),
                        &sender,
                    );
                    self.images_count_label.set_label(&format!("{} images", self.deployments.len()));
                } else if action == "powerwash" {
                    let window = self.stack.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                    let mut builder = adw::MessageDialog::builder()
                        .title("Powerwash?")
                        .heading("Powerwash this device?")
                        .body("`/etc` will be reset to image defaults and all installed apps will be removed. Your home directory, files, and signed-in accounts are kept.");
                    if let Some(ref w) = window {
                        builder = builder.transient_for(w);
                    }
                    let dialog = builder.build();
                    
                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("powerwash", "Powerwash");
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    dialog.set_response_appearance("powerwash", adw::ResponseAppearance::Suggested);
                    
                    let toast_overlay = self.toast_overlay.clone();
                    dialog.connect_response(None, move |dlg, response| {
                        if response == "powerwash" {
                            let toast = adw::Toast::new("Powerwash staged — reboot to apply");
                            toast_overlay.add_toast(toast);
                        }
                        dlg.close();
                    });
                    dialog.present();
                } else if action == "factory" {
                    let window = self.stack.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                    
                    let entry = gtk::Entry::builder()
                        .placeholder_text("reset")
                        .margin_top(12)
                        .margin_bottom(12)
                        .build();
                    entry.add_css_class("entry");

                    let mut builder = adw::MessageDialog::builder()
                        .title("Factory Reset?")
                        .heading("Factory reset?")
                        .body("Erases all user data, accounts, apps, rollback images, and settings, then redeploys the factory image. This cannot be undone.")
                        .extra_child(&entry);
                    if let Some(ref w) = window {
                        builder = builder.transient_for(w);
                    }
                    let dialog = builder.build();
                    
                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("reset", "Factory Reset");
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
                    dialog.set_response_enabled("reset", false);

                    let dlg_clone = dialog.clone();
                    entry.connect_changed(move |ent| {
                        let text = ent.text().to_string();
                        dlg_clone.set_response_enabled("reset", text == "reset");
                    });

                    let toast_overlay = self.toast_overlay.clone();
                    dialog.connect_response(None, move |dlg, response| {
                        if response == "reset" {
                            let toast = adw::Toast::new("Factory reset queued — reboot to begin");
                            toast_overlay.add_toast(toast);
                        }
                        dlg.close();
                    });
                    dialog.present();
                } else {
                    for d in &mut self.deployments {
                        if d.id == action {
                            d.pinned = !d.pinned;
                            let toast_msg = if d.pinned {
                                format!("Pinned {} (preventing pruning)", d.tag)
                            } else {
                                format!("Unpinned {} (allowing pruning)", d.tag)
                            };
                            let toast = adw::Toast::new(&toast_msg);
                            self.toast_overlay.add_toast(toast);
                            break;
                        }
                    }
                    rebuild_history_list(
                        &self.history_list_box,
                        &self.deployments,
                        self.expanded_deployment_id.as_deref(),
                        &sender,
                    );
                    self.images_count_label.set_label(&format!("{} images", self.deployments.len()));
                }
            }

            StatusViewInput::RollbackTo(d) => {
                let window = self.stack.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                let mut builder = adw::MessageDialog::builder()
                    .title("Roll back?")
                    .heading(format!("Roll back to {}?", d.tag))
                    .body(format!(
                        "The next boot will use {}:{}.\nYour current image stays on disk and remains available to roll forward.",
                        d.image, d.tag
                    ));
                if let Some(ref w) = window {
                    builder = builder.transient_for(w);
                }
                let dialog = builder.build();
                
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("rollback", "Roll back");
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                dialog.set_response_appearance("rollback", adw::ResponseAppearance::Suggested);
                
                let dialog_sender = sender.input_sender().clone();
                dialog.connect_response(None, move |dlg, response| {
                    if response == "rollback" {
                        dialog_sender.emit(StatusViewInput::ConfirmRollback);
                    }
                    dlg.close();
                });
                self.rollback_target = Some(d);
                dialog.present();
            }

            StatusViewInput::ConfirmRollback => {
                if let Some(target) = self.rollback_target.take() {
                    let toast = adw::Toast::new(&format!("Rolling back to {}…", target.tag));
                    self.toast_overlay.add_toast(toast);
                }
            }

            StatusViewInput::SetDefaultBoot(d) => {
                let toast = adw::Toast::new(&format!("Set {} as default boot", d.tag));
                self.toast_overlay.add_toast(toast);
            }

            StatusViewInput::SelectChangelogVersion(version) => {
                self.changelog_version = version;
                self.rebuild_changelog_page(&sender);
                self.stack.set_visible_child_name("changelog");
                let _ = sender.output(StatusViewOutput::PageChanged("changelog".to_string()));
            }

            StatusViewInput::RegistryVersionsLoaded(versions) => {
                self.registry_versions = versions;
                if let Some(latest) = self.registry_versions.last() {
                    self.changelog_version = latest.version.clone();
                }
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::GithubCommitsLoaded(commits) => {
                self.github_commits = commits;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::SbomDiffLoaded(diff) => {
                self.sbom_diff = Some(diff);
                self.rebuild_changelog_page(&sender);
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

fn read_auto_updates_enabled() -> bool {
    let output = if crate::update_worker::is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "systemctl", "is-enabled", "uupd.timer"])
            .output()
    } else {
        Command::new("systemctl")
            .args(["is-enabled", "uupd.timer"])
            .output()
    };

    match output {
        Ok(output) => match String::from_utf8_lossy(&output.stdout).trim() {
            "enabled" => true,
            "disabled" => false,
            _ => Settings::load().auto_updates,
        },
        Err(_) => Settings::load().auto_updates,
    }
}

fn apply_auto_updates_setting(active: bool) {
    let mut settings = Settings::load();
    settings.auto_updates = active;
    settings.save();

    std::thread::spawn(move || {
        let args = if active {
            ["enable", "--now", "uupd.timer"]
        } else {
            ["disable", "--now", "uupd.timer"]
        };

        let status = if crate::update_worker::is_flatpak() {
            Command::new("flatpak-spawn")
                .args(["--host", "pkexec", "systemctl"])
                .args(args)
                .status()
        } else {
            Command::new("pkexec").arg("systemctl").args(args).status()
        };

        match status {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!("Failed to toggle uupd.timer: {}", status),
            Err(err) => tracing::warn!("Failed to toggle uupd.timer: {}", err),
        }
    });
}

/// Read the current OS image name and variant from `/etc/os-release`.
/// Tries `/run/host/etc/os-release` first for Flatpak compatibility.
fn read_image_info() -> Option<String> {
    if let Some((title, _, _)) = detect_bootc_image_info() {
        return Some(title);
    }

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

fn read_logo_icon_name() -> String {
    let candidates = ["/run/host/etc/os-release", "/etc/os-release"];
    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                if let Some(v) = line.strip_prefix("LOGO=") {
                    let logo = v.trim_matches('"').to_string();
                    if !logo.is_empty() {
                        return logo;
                    }
                }
            }
        }
    }
    "distributor-logo-symbolic".to_string()
}

use std::sync::Mutex;
static BOOTC_STATUS_CACHE: Mutex<Option<Value>> = Mutex::new(None);

fn get_cached_bootc_status() -> Option<Value> {
    {
        let cache = BOOTC_STATUS_CACHE.lock().unwrap();
        if cache.is_some() {
            return cache.clone();
        }
    }

    let command_desc = if crate::update_worker::is_flatpak() {
        "flatpak-spawn --host pkexec bootc status --json"
    } else {
        "pkexec bootc status --json"
    };
    println!("[debug] read_image_info: running {}", command_desc);

    let output_result = if crate::update_worker::is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "pkexec", "bootc", "status", "--json"])
            .output()
    } else {
        Command::new("pkexec")
            .args(["bootc", "status", "--json"])
            .output()
    };

    let output = output_result.ok()?;
    println!("[debug] bootc status exit = {:?}", output.status);
    if !output.status.success() {
        return None;
    }

    let json: Value = serde_json::from_slice(&output.stdout).ok()?;
    let mut cache = BOOTC_STATUS_CACHE.lock().unwrap();
    *cache = Some(json.clone());
    Some(json)
}

fn detect_bootc_image_info() -> Option<(String, String, String)> {
    let json = get_cached_bootc_status()?;
    let image_ref = json
        .pointer("/status/booted/image/image/image")
        .or_else(|| json.pointer("/status/booted/image/image"))
        .and_then(|v| v.as_str())?;

    println!("[debug] bootc image_ref = {}", image_ref);
    let (registry, org, image, stream) = parse_image_ref_parts(image_ref)?;
    let title = format!("{}/{}", org, image);
    let registry_uri = format!("{}/{}/{}", registry, org, image);
    let selected_tag = stream
        .rsplit('-')
        .next()
        .unwrap_or(&stream)
        .to_string();

    println!("[debug] bootc detected title='{}' registry_uri='{}' selected_tag='{}'", title, registry_uri, selected_tag);
    Some((title, registry_uri, selected_tag))
}

#[derive(serde::Deserialize, Debug, Clone)]
struct BootcImageInfoConfig {
    tags: Vec<String>,
}

fn read_bootc_image_info_config() -> Option<BootcImageInfoConfig> {
    let content = if crate::update_worker::is_flatpak() {
        let output = Command::new("flatpak-spawn")
            .args(["--host", "cat", "/etc/bootc-image-info.json"])
            .output()
            .ok()?;
        if output.status.success() {
            String::from_utf8(output.stdout).ok()
        } else {
            None
        }
    } else {
        std::fs::read_to_string("/etc/bootc-image-info.json").ok()
    }?;

    serde_json::from_str(&content).ok()
}

fn parse_image_ref_parts(image_ref: &str) -> Option<(String, String, String, String)> {
    let (without_tag, tag) = image_ref.rsplit_once(':')?;
    let parts: Vec<&str> = without_tag.splitn(3, '/').collect();
    if parts.len() != 3 {
        return None;
    }

    let registry = parts[0].to_string();
    let org = parts[1].to_string();
    let image = parts[2].to_string();
    let stream = strip_date_suffix(tag).unwrap_or_else(|| tag.to_string());

    Some((registry, org, image, stream))
}

fn strip_date_suffix(tag: &str) -> Option<String> {
    for sep in ['.', '-'] {
        if let Some(pos) = tag.rfind(sep) {
            let suffix = &tag[pos + 1..];
            if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return Some(tag[..pos].to_string());
            }
        }
    }
    None
}

fn read_registry_uri() -> Option<String> {
    detect_bootc_image_info().map(|(_, registry_uri, _)| registry_uri)
}

fn read_selected_tag() -> String {
    detect_bootc_image_info()
        .map(|(_, _, tag)| tag)
        .unwrap_or_else(|| "latest".to_string())
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

fn get_real_deployments_from_json(json: &Value) -> Option<Vec<MockDeployment>> {
    let mut ds = Vec::new();
    let status = json.get("status")?;

    // 1. Staged deployment
    if let Some(staged) = status.get("staged").and_then(|v| if v.is_null() { None } else { Some(v) }) {
        let img_ref = staged.pointer("/image/image/image").or_else(|| staged.pointer("/image/image")).and_then(|v| v.as_str()).unwrap_or("");
        let digest = staged.pointer("/image/imageDigest").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = staged.pointer("/image/timestamp").and_then(|v| v.as_str()).unwrap_or("");
        
        let (org_img, tag) = if img_ref.is_empty() {
            ("Fedora bootc".to_string(), "latest".to_string())
        } else {
            let parts: Vec<&str> = img_ref.split(':').collect();
            let name = parts[0].split('/').last().unwrap_or(parts[0]).to_string();
            let tag = parts.get(1).map(|t| t.to_string()).unwrap_or_else(|| "latest".to_string());
            (name, tag)
        };

        let date_str = if timestamp.len() >= 10 { &timestamp[0..10] } else { "recently" };

        ds.push(MockDeployment {
            id: "d-staged".to_string(),
            state: "staged".to_string(),
            title: org_img,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Staged · pending reboot".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "2.2 GB".to_string(),
            kernel: "6.14.1-300.fc43.x86_64".to_string(),
            package_count: 1361,
            signer: "fedora-43 (sigstore)".to_string(),
            pinned: false,
        });
    }

    // 2. Booted deployment
    if let Some(booted) = status.get("booted").and_then(|v| if v.is_null() { None } else { Some(v) }) {
        let img_ref = booted.pointer("/image/image/image").or_else(|| booted.pointer("/image/image")).and_then(|v| v.as_str()).unwrap_or("");
        let digest = booted.pointer("/image/imageDigest").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = booted.pointer("/image/timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let pinned = booted.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false);

        let (org_img, tag) = if img_ref.is_empty() {
            ("Fedora bootc".to_string(), "latest".to_string())
        } else {
            let parts: Vec<&str> = img_ref.split(':').collect();
            let name = parts[0].split('/').last().unwrap_or(parts[0]).to_string();
            let tag = parts.get(1).map(|t| t.to_string()).unwrap_or_else(|| "latest".to_string());
            (name, tag)
        };

        let date_str = if timestamp.len() >= 10 { &timestamp[0..10] } else { "recently" };

        ds.push(MockDeployment {
            id: "d-current".to_string(),
            state: "current".to_string(),
            title: org_img,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Currently booted".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "2.1 GB".to_string(),
            kernel: "6.13.7-300.fc42.x86_64".to_string(),
            package_count: 1342,
            signer: "fedora-42 (sigstore)".to_string(),
            pinned,
        });
    }

    // 3. Rollback deployment
    if let Some(rollback) = status.get("rollback").and_then(|v| if v.is_null() { None } else { Some(v) }) {
        let img_ref = rollback.pointer("/image/image/image").or_else(|| rollback.pointer("/image/image")).and_then(|v| v.as_str()).unwrap_or("");
        let digest = rollback.pointer("/image/imageDigest").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = rollback.pointer("/image/timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let pinned = rollback.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false);

        let (org_img, tag) = if img_ref.is_empty() {
            ("Fedora bootc".to_string(), "latest".to_string())
        } else {
            let parts: Vec<&str> = img_ref.split(':').collect();
            let name = parts[0].split('/').last().unwrap_or(parts[0]).to_string();
            let tag = parts.get(1).map(|t| t.to_string()).unwrap_or_else(|| "latest".to_string());
            (name, tag)
        };

        let date_str = if timestamp.len() >= 10 { &timestamp[0..10] } else { "recently" };

        ds.push(MockDeployment {
            id: "d-rollback".to_string(),
            state: "previous".to_string(),
            title: org_img,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Rollback target".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "2.0 GB".to_string(),
            kernel: "6.13.4-200.fc42.x86_64".to_string(),
            package_count: 1338,
            signer: "fedora-42 (sigstore)".to_string(),
            pinned,
        });
    }

    if ds.is_empty() { None } else { Some(ds) }
}

fn get_real_deployments() -> Option<Vec<MockDeployment>> {
    let output_result = if crate::update_worker::is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "bootc", "status", "--json"])
            .output()
    } else {
        Command::new("bootc")
            .args(["status", "--json"])
            .output()
    };

    let output = output_result.ok()?;
    if !output.status.success() {
        return None;
    }

    let json: Value = serde_json::from_slice(&output.stdout).ok()?;
    get_real_deployments_from_json(&json)
}

fn get_sample_deployments(reboot_pending: bool) -> Vec<MockDeployment> {
    if let Some(ds) = get_real_deployments() {
        return ds;
    }

    let title = read_image_info().unwrap_or_else(|| "Fedora bootc".to_string());
    let image = read_registry_uri().unwrap_or_else(|| "quay.io/fedora/fedora-bootc".to_string());
    let mut ds = Vec::new();
    if reboot_pending {
        ds.push(MockDeployment {
            id: "d-staged".to_string(),
            state: "staged".to_string(),
            title: title.clone(),
            image: image.clone(),
            tag: "43".to_string(),
            digest: "sha256:f4e8c1a6b9d2f5a8c1e4b7d0a3c6f9b2e5a8d1c4f7b0a3e6d9c2f5b8a1e4d7c0".to_string(),
            deployed: "Staged · pending reboot".to_string(),
            deployed_full: "Staged · pending reboot".to_string(),
            size: "2.2 GB".to_string(),
            kernel: "6.14.1-300.fc43.x86_64".to_string(),
            package_count: 1361,
            signer: "fedora-43 (sigstore)".to_string(),
            pinned: false,
        });
    }
    ds.push(MockDeployment {
        id: "d-current".to_string(),
        state: "current".to_string(),
        title: title.clone(),
        image: image.clone(),
        tag: "42".to_string(),
        digest: "sha256:a8c92f1e0b7c3d4f9a2b1e6c8d0f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2".to_string(),
        deployed: "3 days ago".to_string(),
        deployed_full: "May 24, 2026 · 14:22 UTC".to_string(),
        size: "2.1 GB".to_string(),
        kernel: "6.13.7-300.fc42.x86_64".to_string(),
        package_count: 1342,
        signer: "fedora-42 (sigstore)".to_string(),
        pinned: false,
    });
    ds.push(MockDeployment {
        id: "d-prev".to_string(),
        state: "previous".to_string(),
        title: title.clone(),
        image: image.clone(),
        tag: "42".to_string(),
        digest: "sha256:b3e1188fd91a7eaa0c6c8c4f1d8e9a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d2e1f0".to_string(),
        deployed: "11 days ago".to_string(),
        deployed_full: "May 16, 2026 · 09:08 UTC".to_string(),
        size: "2.0 GB".to_string(),
        kernel: "6.13.4-200.fc42.x86_64".to_string(),
        package_count: 1338,
        signer: "fedora-42 (sigstore)".to_string(),
        pinned: true,
    });
    ds.push(MockDeployment {
        id: "d-arch".to_string(),
        state: "archived".to_string(),
        title: title.clone(),
        image: image.clone(),
        tag: "41".to_string(),
        digest: "sha256:c0d22a4e6b8d1f3a5c7e9b0d2f4a6c8e0b1d3f5a7c9e1b3d5f7a9c1e3b5d7f9a0".to_string(),
        deployed: "6 weeks ago".to_string(),
        deployed_full: "Apr 15, 2026 · 18:51 UTC".to_string(),
        size: "1.9 GB".to_string(),
        kernel: "6.12.9-100.fc41.x86_64".to_string(),
        package_count: 1297,
        signer: "fedora-41 (sigstore)".to_string(),
        pinned: false,
    });
    ds
}

fn rebuild_history_list(
    list_box: &gtk::ListBox,
    deployments: &[MockDeployment],
    expanded_id: Option<&str>,
    sender: &ComponentSender<StatusView>,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    
    for d in deployments {
        let row_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        
        let row_header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_header.set_margin_start(16);
        row_header.set_margin_end(16);
        row_header.set_margin_top(12);
        row_header.set_margin_bottom(12);
        
        let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let indicator_class = match d.state.as_str() {
            "current" => "deploy-indicator-current",
            "staged" => "deploy-indicator-staged",
            _ => "deploy-indicator-archive",
        };
        indicator.add_css_class(indicator_class);
        row_header.append(&indicator);
        
        let text_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_box.set_hexpand(true);
        
        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name_label = gtk::Label::builder()
            .label(&d.title)
            .halign(gtk::Align::Start)
            .build();
        name_label.add_css_class("heading");
        title_box.append(&name_label);
        
        if d.state == "current" {
            let pill = gtk::Label::new(Some("Booted"));
            pill.add_css_class("status-pill-ok");
            pill.add_css_class("caption");
            title_box.append(&pill);
        } else if d.state == "staged" {
            let pill = gtk::Label::new(Some("Staged"));
            pill.add_css_class("status-pill-ready");
            pill.add_css_class("caption");
            title_box.append(&pill);
        }
        if d.pinned {
            let pill = gtk::Label::new(Some("Pinned"));
            pill.add_css_class("status-pill-staged");
            pill.add_css_class("caption");
            title_box.append(&pill);
        }
        text_box.append(&title_box);
        
        let submeta_label = gtk::Label::builder()
            .label(&format!("{}:{}  ·  {}…  ·  {}", d.image, d.tag, &d.digest[0..12], d.deployed))
            .halign(gtk::Align::Start)
            .build();
        submeta_label.add_css_class("caption");
        submeta_label.add_css_class("dim-label");
        text_box.append(&submeta_label);
        row_header.append(&text_box);
        
        let actions_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        
        let pin_btn = gtk::Button::builder()
            .icon_name("pin-symbolic")
            .tooltip_text(if d.pinned { "Unpin" } else { "Pin" })
            .build();
        pin_btn.add_css_class("flat");
        if d.pinned {
            pin_btn.add_css_class("warning");
        }
        let pin_sender = sender.input_sender().clone();
        let pin_id = d.id.clone();
        pin_btn.connect_clicked(move |_| {
            pin_sender.emit(StatusViewInput::TogglePin(pin_id.clone()));
        });
        actions_box.append(&pin_btn);
        
        if d.state != "current" && d.state != "staged" {
            let rb_btn = gtk::Button::builder()
                .icon_name("edit-undo-symbolic")
                .tooltip_text("Roll back to this image")
                .build();
            rb_btn.add_css_class("flat");
            let rb_sender = sender.input_sender().clone();
            let rb_d = d.clone();
            rb_btn.connect_clicked(move |_| {
                rb_sender.emit(StatusViewInput::RollbackTo(rb_d.clone()));
            });
            actions_box.append(&rb_btn);
        }
        
        let is_expanded = expanded_id == Some(&d.id);
        let chevron_icon = if is_expanded { "go-up-symbolic" } else { "go-down-symbolic" };
        let chev_btn = gtk::Button::builder()
            .icon_name(chevron_icon)
            .build();
        chev_btn.add_css_class("flat");
        
        let toggle_sender = sender.input_sender().clone();
        let toggle_id = d.id.clone();
        let text_click_sender = sender.input_sender().clone();
        let text_click_id = d.id.clone();
        
        let gesture = gtk::GestureClick::new();
        gesture.connect_pressed(move |_, _, _, _| {
            text_click_sender.emit(StatusViewInput::TogglePin(format!("expand:{}", text_click_id)));
        });
        text_box.add_controller(gesture);
        
        chev_btn.connect_clicked(move |_| {
            toggle_sender.emit(StatusViewInput::TogglePin(format!("expand:{}", toggle_id)));
        });
        actions_box.append(&chev_btn);
        
        row_header.append(&actions_box);
        row_container.append(&row_header);
        
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(200);
        revealer.set_reveal_child(is_expanded);
        
        let detail_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        detail_box.set_margin_start(56);
        detail_box.set_margin_end(24);
        detail_box.set_margin_top(8);
        detail_box.set_margin_bottom(16);
        
        let grid = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(16)
            .build();
        
        let fields = [
            ("Image", d.image.as_str()),
            ("Tag", d.tag.as_str()),
            ("Digest", d.digest.as_str()),
            ("Deployed", d.deployed_full.as_str()),
            ("Kernel", d.kernel.as_str()),
        ];
        
        for (row_idx, &(label, val)) in fields.iter().enumerate() {
            let lbl = gtk::Label::builder()
                .label(label)
                .halign(gtk::Align::Start)
                .build();
            lbl.add_css_class("caption");
            lbl.add_css_class("dim-label");
            
            let val_lbl = gtk::Label::builder()
                .label(val)
                .halign(gtk::Align::Start)
                .build();
            val_lbl.add_css_class("caption");
            val_lbl.add_css_class("monospace");
            
            grid.attach(&lbl, 0, row_idx as i32, 1, 1);
            grid.attach(&val_lbl, 1, row_idx as i32, 1, 1);
        }
        
        let pkg_lbl = gtk::Label::builder()
            .label("Packages")
            .halign(gtk::Align::Start)
            .build();
        pkg_lbl.add_css_class("caption");
        pkg_lbl.add_css_class("dim-label");
        
        let pkg_val = gtk::Label::builder()
            .label(format!("{} installed", d.package_count))
            .halign(gtk::Align::Start)
            .build();
        pkg_val.add_css_class("caption");
        pkg_val.add_css_class("monospace");
        grid.attach(&pkg_lbl, 0, fields.len() as i32, 1, 1);
        grid.attach(&pkg_val, 1, fields.len() as i32, 1, 1);
        
        let sig_lbl = gtk::Label::builder()
            .label("Signature")
            .halign(gtk::Align::Start)
            .build();
        sig_lbl.add_css_class("caption");
        sig_lbl.add_css_class("dim-label");
        
        let sig_val = gtk::Label::builder()
            .label(format!("✓ Verified  ·  {}", d.signer))
            .halign(gtk::Align::Start)
            .build();
        sig_val.add_css_class("caption");
        sig_val.add_css_class("success");
        grid.attach(&sig_lbl, 0, (fields.len() + 1) as i32, 1, 1);
        grid.attach(&sig_val, 1, (fields.len() + 1) as i32, 1, 1);
        
        detail_box.append(&grid);
        
        let bottom_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        
        if d.state != "current" && d.state != "staged" {
            let rb_btn = gtk::Button::builder()
                .label("Roll back to this")
                .icon_name("edit-undo-symbolic")
                .build();
            rb_btn.add_css_class("suggested-action");
            rb_btn.add_css_class("pill");
            let rb_sender = sender.input_sender().clone();
            let rb_d = d.clone();
            rb_btn.connect_clicked(move |_| {
                rb_sender.emit(StatusViewInput::RollbackTo(rb_d.clone()));
            });
            bottom_actions.append(&rb_btn);
        }
        
        if d.state != "current" {
            let def_btn = gtk::Button::builder()
                .label("Set as default boot")
                .build();
            def_btn.add_css_class("pill");
            let def_sender = sender.input_sender().clone();
            let def_d = d.clone();
            def_btn.connect_clicked(move |_| {
                def_sender.emit(StatusViewInput::SetDefaultBoot(def_d.clone()));
            });
            bottom_actions.append(&def_btn);
        }
        
        let ch_btn = gtk::Button::builder()
            .label("View changelog")
            .build();
        ch_btn.add_css_class("flat");
        ch_btn.add_css_class("pill");
        let ch_sender = sender.input_sender().clone();
        let ch_tag = d.tag.clone();
        ch_btn.connect_clicked(move |_| {
            ch_sender.emit(StatusViewInput::SelectChangelogVersion(ch_tag.clone()));
        });
        bottom_actions.append(&ch_btn);
        
        detail_box.append(&bottom_actions);
        revealer.set_child(Some(&detail_box));
        row_container.append(&revealer);
        
        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        row_container.append(&sep);
        
        list_box.append(&row_container);
    }
}

fn parse_org_repo(uri: &str) -> Option<(String, String)> {
    let clean_uri = if let Some(pos) = uri.find("docker://") {
        &uri[pos + 9..]
    } else {
        uri
    };
    let parts: Vec<&str> = clean_uri.split('/').collect();
    if parts.len() >= 3 {
        let org = parts[1].to_string();
        let repo = parts[2..].join("/");
        Some((org, repo))
    } else if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

fn spawn_changelog_fetch(
    registry_uri: String,
    selected_tag: String,
    sender: ComponentSender<StatusView>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        rt.block_on(async move {
            println!("[debug] changelog: starting fetch for registry_uri={}", registry_uri);
            // 1. Fetch registry versions
            let parts: Vec<&str> = registry_uri.split('/').collect();
            if parts.len() >= 3 {
                let registry = parts[0];
                let org = parts[1];
                let image = parts[2..].join("/");
                
                let separators = ['.', '-'];
                let mut stream = selected_tag.clone();
                for sep in &separators {
                    if let Some(pos) = selected_tag.rfind(*sep) {
                        let suffix = &selected_tag[pos + 1..];
                        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                            stream = selected_tag[..pos].to_string();
                            break;
                        }
                    }
                }

                let client = crate::registry_client::RegistryClient::new(registry, org, &image, &stream);
                match client.fetch_versions(30).await {
                    Ok(versions) => {
                        println!("[debug] changelog: fetched {} registry versions", versions.len());
                        sender.input(StatusViewInput::RegistryVersionsLoaded(versions));
                    }
                    Err(e) => {
                        println!("[debug] changelog: failed to fetch registry versions: {}", e);
                    }
                }
            }

            // 2. Fetch GitHub commits
            if let Some((org, repo)) = parse_org_repo(&registry_uri) {
                let url = format!("https://api.github.com/repos/{}/{}/commits", org, repo);
                println!("[debug] changelog: fetching github commits from {}", url);
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .user_agent("Finupdate/0.1.0")
                    .build()
                    .unwrap_or_default();

                match client.get(&url).send().await {
                    Ok(resp) => {
                        #[derive(serde::Deserialize)]
                        struct GithubCommit {
                            sha: String,
                            commit: CommitDetails,
                        }
                        #[derive(serde::Deserialize)]
                        struct CommitDetails {
                            message: String,
                            author: AuthorDetails,
                        }
                        #[derive(serde::Deserialize)]
                        struct AuthorDetails {
                            name: String,
                            date: String,
                        }

                        if let Ok(commits_json) = resp.json::<Vec<GithubCommit>>().await {
                            let commits: Vec<(String, String, String)> = commits_json
                                .into_iter()
                                .map(|c| {
                                    let sha = if c.sha.len() >= 7 {
                                        c.sha[0..7].to_string()
                                    } else {
                                        c.sha
                                    };
                                    (sha, c.commit.message, c.commit.author.name)
                                })
                                .collect();
                            println!("[debug] changelog: fetched {} github commits", commits.len());
                            sender.input(StatusViewInput::GithubCommitsLoaded(commits));
                        } else {
                            println!("[debug] changelog: failed to parse github commits JSON");
                        }
                    }
                    Err(e) => {
                        println!("[debug] changelog: failed to fetch github commits: {}", e);
                    }
                }
            }

            // 3. Fetch and diff SBOMs in the background
            let booted_tag = read_selected_tag();
            let booted_ref = format!("{}:{}", registry_uri, booted_tag);
            let target_ref = format!("{}:{}", registry_uri, selected_tag);
            
            println!("[debug] sbom_diff: starting background fetch booted_ref={} target_ref={}", booted_ref, target_ref);
            if let Some(diff) = crate::sbom_diff::fetch_and_diff_sboms(booted_ref, target_ref) {
                sender.input(StatusViewInput::SbomDiffLoaded(diff));
            }
        });
    });
}
