//! Update list component — shows per-module status during an update run.
//!
//! Displays four rows (System, Flatpak, Brew, Distrobox) as `adw::ExpanderRow`
//! widgets. Each row shows a status indicator (spinner while running, icon when
//! complete/failed). A **Nerd Mode** toggle button expands all rows to reveal
//! the raw log output attributed to each module.
//!
//! ## Module detection
//! uupd logs emit `module_name=<Name>` when a module starts. This component
//! parses incoming log lines via `ProcessLine` and transitions module state
//! accordingly. It also detects `level=ERROR` / `module_fail` to mark failures.
//!
//! ## Nerd Mode
//! When off: rows are compact, expansion is disabled (no arrow visible).
//! When on: all rows expand to show their attributed log lines in monospace text.

use adw::prelude::*;
use gtk::prelude::*;
use relm4::prelude::*;

/// The four uupd modules, in execution order.
/// Fields: (key, display name, short description)
const MODULES: &[(&str, &str, &str)] = &[
    ("system", "System", "OS image · bootc"),
    ("flatpak", "Flatpak", "Applications"),
    ("brew", "Brew", "Homebrew packages"),
    ("distrobox", "Distrobox", "Container environments"),
];

#[derive(Debug, Clone, PartialEq)]
enum ModuleStatus {
    Pending,
    Running,
    Complete,
    Failed,
}

struct ModuleEntry {
    key: &'static str,
    description: &'static str,
    row: adw::ExpanderRow,
    status_stack: gtk::Stack,
    spinner: gtk::Spinner,
    log_label: gtk::Label,
    status: ModuleStatus,
    log_text: String,
}

/// Input messages for the UpdateList component.
#[derive(Debug)]
pub enum UpdateListInput {
    /// Reset all modules to Pending (call when a new update starts).
    Reset,
    /// Process one log line from uupd — detect module transitions and attribute output.
    ProcessLine(String),
    /// Toggle Nerd Mode: expand all rows to show raw log output.
    SetNerdMode(bool),
    /// Mark all Pending/Running modules as Complete (update finished OK).
    MarkAllComplete,
    /// Mark the currently Running module as Failed.
    MarkCurrentFailed,
}

/// Component model.
pub struct UpdateList {
    modules: Vec<ModuleEntry>,
    /// Index into `modules` for the module that is currently running.
    current_module: Option<usize>,
    nerd_mode: bool,
    nerd_btn: gtk::ToggleButton,
}

#[relm4::component(pub)]
impl SimpleComponent for UpdateList {
    type Init = ();
    type Input = UpdateListInput;
    type Output = ();

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 0,
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // ── Header row ────────────────────────────────────────────────────
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.set_margin_start(12);
        header.set_margin_end(12);
        header.set_margin_top(10);
        header.set_margin_bottom(4);

        let title = gtk::Label::new(Some("Component Updates"));
        title.add_css_class("heading");
        title.set_hexpand(true);
        title.set_halign(gtk::Align::Start);

        let nerd_btn = gtk::ToggleButton::builder()
            .label("Nerd Mode")
            .icon_name("utilities-terminal-symbolic")
            .tooltip_text("Expand rows to show detailed log output per component")
            .build();
        nerd_btn.add_css_class("flat");

        let nerd_sender = sender.input_sender().clone();
        nerd_btn.connect_toggled(move |btn| {
            nerd_sender.emit(UpdateListInput::SetNerdMode(btn.is_active()));
        });

        header.append(&title);
        header.append(&nerd_btn);

        // ── Module rows ───────────────────────────────────────────────────
        let list_box = gtk::ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::None);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(12);
        list_box.set_margin_end(12);
        list_box.set_margin_bottom(8);

        let mut modules = Vec::with_capacity(MODULES.len());

        for &(key, name, description) in MODULES {
            let row = adw::ExpanderRow::builder()
                .title(name)
                .subtitle(description)
                // Hide expand arrow until Nerd Mode is enabled.
                .enable_expansion(false)
                .build();

            // Status indicator: stack switches between visual states.
            let status_stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(150)
                .valign(gtk::Align::Center)
                .build();

            let pending_icon = gtk::Image::from_icon_name("media-playback-pause-symbolic");
            pending_icon.add_css_class("dim-label");

            let spinner = gtk::Spinner::new();
            spinner.set_size_request(16, 16);

            let complete_icon = gtk::Image::from_icon_name("emblem-ok-symbolic");
            complete_icon.add_css_class("success");

            let failed_icon = gtk::Image::from_icon_name("dialog-error-symbolic");
            failed_icon.add_css_class("error");

            status_stack.add_named(&pending_icon, Some("pending"));
            status_stack.add_named(&spinner, Some("running"));
            status_stack.add_named(&complete_icon, Some("complete"));
            status_stack.add_named(&failed_icon, Some("failed"));
            status_stack.set_visible_child_name("pending");

            row.add_suffix(&status_stack);

            // Log content widget — shown when the row is expanded (Nerd Mode).
            let log_label = gtk::Label::new(Some("No output yet."));
            log_label.add_css_class("monospace");
            log_label.add_css_class("caption");
            log_label.add_css_class("dim-label");
            log_label.set_xalign(0.0);
            log_label.set_wrap(true);
            log_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            log_label.set_selectable(true);
            log_label.set_margin_start(12);
            log_label.set_margin_end(12);
            log_label.set_margin_top(6);
            log_label.set_margin_bottom(6);

            row.add_row(&log_label);
            list_box.append(&row);

            modules.push(ModuleEntry {
                key,
                description,
                row,
                status_stack,
                spinner,
                log_label,
                status: ModuleStatus::Pending,
                log_text: String::new(),
            });
        }

        root.append(&header);
        root.append(&list_box);

        let model = UpdateList {
            modules,
            current_module: None,
            nerd_mode: false,
            nerd_btn,
        };

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            UpdateListInput::Reset => {
                self.current_module = None;
                self.nerd_mode = false;
                self.nerd_btn.set_active(false);
                for i in 0..self.modules.len() {
                    self.modules[i].log_text.clear();
                    self.modules[i].log_label.set_text("No output yet.");
                    self.modules[i].row.set_enable_expansion(false);
                    self.modules[i].row.set_expanded(false);
                    self.apply_status(i, ModuleStatus::Pending);
                }
            }

            UpdateListInput::ProcessLine(line) => {
                // Check if this line announces a module starting.
                if let Some(idx) = detect_module_start(&line) {
                    // Mark the previously running module complete before switching.
                    if let Some(prev) = self.current_module {
                        if self.modules[prev].status == ModuleStatus::Running {
                            self.apply_status(prev, ModuleStatus::Complete);
                        }
                    }
                    self.current_module = Some(idx);
                    self.apply_status(idx, ModuleStatus::Running);
                }

                // Attribute the line to whatever module is currently running.
                if let Some(idx) = self.current_module {
                    if !self.modules[idx].log_text.is_empty() {
                        self.modules[idx].log_text.push('\n');
                    }
                    self.modules[idx].log_text.push_str(&line);
                    let text = self.modules[idx].log_text.clone();
                    self.modules[idx].log_label.set_text(&text);
                }

                // Detect failure signals in the log.
                if line.contains("level=ERROR") || line.contains("module_fail") {
                    if let Some(idx) = self.current_module {
                        self.apply_status(idx, ModuleStatus::Failed);
                    }
                }
            }

            UpdateListInput::SetNerdMode(enabled) => {
                self.nerd_mode = enabled;
                for entry in &self.modules {
                    entry.row.set_enable_expansion(enabled);
                    entry.row.set_expanded(enabled);
                }
            }

            UpdateListInput::MarkAllComplete => {
                for i in 0..self.modules.len() {
                    if matches!(
                        self.modules[i].status,
                        ModuleStatus::Running | ModuleStatus::Pending
                    ) {
                        self.apply_status(i, ModuleStatus::Complete);
                    }
                }
                self.current_module = None;
            }

            UpdateListInput::MarkCurrentFailed => {
                if let Some(idx) = self.current_module {
                    if self.modules[idx].status == ModuleStatus::Running {
                        self.apply_status(idx, ModuleStatus::Failed);
                    }
                }
            }
        }
    }
}

impl UpdateList {
    /// Apply a status transition to one module entry, updating all visual indicators.
    fn apply_status(&mut self, idx: usize, status: ModuleStatus) {
        let entry = &mut self.modules[idx];

        let stack_page = match &status {
            ModuleStatus::Pending => "pending",
            ModuleStatus::Running => "running",
            ModuleStatus::Complete => "complete",
            ModuleStatus::Failed => "failed",
        };
        let subtitle = match &status {
            ModuleStatus::Pending => entry.description,
            ModuleStatus::Running => "Updating…",
            ModuleStatus::Complete => "Complete",
            ModuleStatus::Failed => "Failed",
        };

        entry.status_stack.set_visible_child_name(stack_page);
        entry.row.set_subtitle(subtitle);

        if status == ModuleStatus::Running {
            entry.spinner.start();
        } else {
            entry.spinner.stop();
        }

        entry.status = status;
    }
}

/// Parse a log line from uupd and return the index into MODULES if it announces
/// a module starting. uupd emits `module_name=System` etc. when a module begins.
fn detect_module_start(line: &str) -> Option<usize> {
    // Match against the capitalized display names uupd uses in its structured log.
    const PATTERNS: &[(&str, &str)] = &[
        ("System", "system"),
        ("Flatpak", "flatpak"),
        ("Brew", "brew"),
        ("Distrobox", "distrobox"),
    ];

    for &(display, key) in PATTERNS {
        let needle = format!("module_name={}", display);
        if line.contains(needle.as_str()) {
            return MODULES.iter().position(|&(k, _, _)| k == key);
        }
    }
    None
}
