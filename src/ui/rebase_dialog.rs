//! Rebase history dialog — lets the user rebase to any OS image from the
//! last 90 days via a calendar grid.
//!
//! Entry point: [`show_rebase_dialog`] — opens a modal `adw::Dialog`.
//!
//! ## Flow
//!
//! ```text
//! show_rebase_dialog()
//!   └── spawn background thread
//!         └── RegistryClient::detect() → fetch_versions(90)
//!               → result slot + timeout poll on UI thread
//!                    ├── Success → show calendar + details panel
//!                    └── Error   → show error page with retry
//! ```
//!
//! When the user picks a date and confirms, `bootc switch {full_ref}` is
//! run on the host via the same `flatpak-spawn --host pkexec` pattern as
//! the main update worker.

use adw::prelude::*;
use chrono::{Datelike, Local, NaiveDate};
use gtk::glib;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::registry_client::{ImageVersion, RegistryClient};
use crate::update_worker::is_flatpak;

/// Open the rebase history dialog as a child of `parent`.
pub fn show_rebase_dialog(parent: &adw::ApplicationWindow, dev_mode: bool) {
    let dialog = adw::Dialog::builder()
        .title("Rebase to Previous Version")
        .content_width(520)
        .content_height(600)
        .build();

    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar_view.add_top_bar(&header);

    // ── Stack: loading / loaded / error ────────────────────────────────
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(200)
        .build();

    // Loading page
    let loading_page = {
        let status = adw::StatusPage::builder()
            .title("Loading Version History")
            .description("Fetching available image versions…")
            .build();
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_size_request(32, 32);
        status.set_child(Some(&spinner));
        status
    };
    stack.add_named(&loading_page, Some("loading"));

    // Error page
    let retry_button = gtk::Button::builder()
        .label("Retry")
        .halign(gtk::Align::Center)
        .build();
    let error_page = adw::StatusPage::builder()
        .icon_name("network-error-symbolic")
        .title("Couldn't Load Versions")
        .description("Check your internet connection and try again.")
        .build();
    error_page.set_child(Some(&retry_button));
    stack.add_named(&error_page, Some("error"));

    // Loaded page — built dynamically once data arrives
    let loaded_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let loaded_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    loaded_scroll.set_child(Some(&loaded_box));
    stack.add_named(&loaded_scroll, Some("loaded"));

    toolbar_view.set_content(Some(&stack));
    dialog.set_child(Some(&toolbar_view));
    stack.set_visible_child_name("loading");

    let stack_for_retry = stack.clone();
    let loaded_box_for_retry = loaded_box.clone();
    let dialog_for_retry = dialog.clone();
    let parent_for_retry = parent.clone();
    let error_page_for_retry = error_page.clone();
    retry_button.connect_clicked(move |_| {
        start_version_fetch(
            stack_for_retry.clone(),
            loaded_box_for_retry.clone(),
            dialog_for_retry.clone(),
            parent_for_retry.clone(),
            error_page_for_retry.clone(),
            dev_mode,
        );
    });

    dialog.present(Some(parent));
    start_version_fetch(
        stack.clone(),
        loaded_box.clone(),
        dialog.clone(),
        parent.clone(),
        error_page.clone(),
        dev_mode,
    );
}

fn start_version_fetch(
    stack: gtk::Stack,
    loaded_box: gtk::Box,
    dialog: adw::Dialog,
    parent: adw::ApplicationWindow,
    error_page: adw::StatusPage,
    dev_mode: bool,
) {
    stack.set_visible_child_name("loading");
    error_page.set_description(Some("Check your internet connection and try again."));

    if dev_mode {
        // In dev mode, use simulated data so the dialog is functional without bootc.
        let versions = generate_mock_versions();
        glib::idle_add_local_once(move || {
            build_loaded_page(&loaded_box, &stack, &dialog, &parent, versions, dev_mode);
            stack.set_visible_child_name("loaded");
        });
        return;
    }

    let result_slot: Arc<Mutex<Option<FetchResult>>> = Arc::new(Mutex::new(None));
    spawn_fetch_thread(result_slot.clone());

    let start_time = std::time::Instant::now();
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        if let Some(result) = result_slot.lock().ok().and_then(|mut guard| guard.take()) {
            match result {
                FetchResult::Ok(versions) => {
                    build_loaded_page(&loaded_box, &stack, &dialog, &parent, versions, dev_mode);
                    stack.set_visible_child_name("loaded");
                }
                FetchResult::DetectFailed => {
                    error_page.set_description(Some(
                        "Could not detect the current image. Is bootc installed and managing this system?",
                    ));
                    stack.set_visible_child_name("error");
                }
                FetchResult::Err(_) => {
                    error_page
                        .set_description(Some("Check your internet connection and try again."));
                    stack.set_visible_child_name("error");
                }
            }
            return glib::ControlFlow::Break;
        }

        if start_time.elapsed() > std::time::Duration::from_secs(20) {
            error_page.set_description(Some("Check your internet connection and try again."));
            stack.set_visible_child_name("error");
            return glib::ControlFlow::Break;
        }

        glib::ControlFlow::Continue
    });
}

fn spawn_fetch_thread(result_slot: Arc<Mutex<Option<FetchResult>>>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        rt.block_on(async move {
            let result = match RegistryClient::detect().await {
                None => FetchResult::DetectFailed,
                Some(client) => match client.fetch_versions(90).await {
                    Ok(versions) => FetchResult::Ok(versions),
                    Err(e) => FetchResult::Err(e.to_string()),
                },
            };
            *result_slot.lock().unwrap() = Some(result);
        });
    });
}

// ── Loaded page builder ──────────────────────────────────────────────────────

fn build_loaded_page(
    container: &gtk::Box,
    stack: &gtk::Stack,
    dialog: &adw::Dialog,
    parent: &adw::ApplicationWindow,
    versions: Vec<ImageVersion>,
    dev_mode: bool,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    // Version lookup map
    let version_map: HashMap<NaiveDate, ImageVersion> =
        versions.iter().map(|v| (v.date, v.clone())).collect();
    let version_map = Rc::new(version_map);

    // Find "current" date from the version whose full_ref matches bootc status
    // (best-effort: use the most recent version as current for now).
    let current_date: Option<NaiveDate> = versions.last().map(|v| v.date);

    // ── Selected version state ──────────────────────────────────────────
    let selected: Rc<RefCell<Option<NaiveDate>>> = Rc::new(RefCell::new(None));

    // ── Details panel (hidden until selection) ──────────────────────────
    let details_group = adw::PreferencesGroup::builder()
        .title("Selected Version")
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    details_group.set_visible(false);

    let version_row = adw::ActionRow::builder().title("Version").build();
    let kernel_row = adw::ActionRow::builder().title("Kernel").build();
    let built_row = adw::ActionRow::builder().title("Built").build();
    let commit_row = adw::ActionRow::builder().title("Commit").build();

    details_group.add(&version_row);
    details_group.add(&kernel_row);
    details_group.add(&built_row);
    details_group.add(&commit_row);

    // ── Rebase button (disabled until selection) ────────────────────────
    let rebase_btn = gtk::Button::builder()
        .label("Rebase…")
        .sensitive(false)
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(16)
        .build();
    rebase_btn.add_css_class("suggested-action");
    rebase_btn.add_css_class("pill");

    // ── Build calendar grid ─────────────────────────────────────────────
    let calendar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    calendar_box.set_margin_start(8);
    calendar_box.set_margin_end(8);
    calendar_box.set_margin_top(16);
    calendar_box.set_margin_bottom(8);

    // Current displayed month — starts at current month.
    let today = Local::now().date_naive();
    let initial_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    let displayed_month: Rc<RefCell<NaiveDate>> = Rc::new(RefCell::new(initial_month));

    // ── Month nav row ───────────────────────────────────────────────────
    let prev_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Previous month")
        .build();
    prev_btn.add_css_class("flat");
    prev_btn.add_css_class("circular");

    let next_btn = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text("Next month")
        .build();
    next_btn.add_css_class("flat");
    next_btn.add_css_class("circular");
    // Initially disabled (already on current month)
    next_btn.set_sensitive(false);

    let month_label = gtk::Label::builder()
        .hexpand(true)
        .halign(gtk::Align::Center)
        .build();
    month_label.add_css_class("title-4");

    let nav_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .margin_bottom(12)
        .build();
    nav_row.append(&prev_btn);
    nav_row.append(&month_label);
    nav_row.append(&next_btn);
    calendar_box.append(&nav_row);

    // Weekday headers
    let header_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .margin_bottom(4)
        .build();
    for day in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
        let lbl = gtk::Label::new(Some(day));
        lbl.add_css_class("caption");
        lbl.add_css_class("dim-label");
        lbl.set_hexpand(true);
        lbl.set_halign(gtk::Align::Center);
        header_row.append(&lbl);
    }
    calendar_box.append(&header_row);

    // Day grid — 7 columns × 6 rows, pre-populated
    let grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(4)
        .row_homogeneous(true)
        .column_homogeneous(true)
        .build();
    for row in 0..6i32 {
        for col in 0..7i32 {
            let btn = gtk::Button::new();
            btn.add_css_class("flat");
            btn.add_css_class("day-btn");
            grid.attach(&btn, col, row, 1, 1);
        }
    }
    calendar_box.append(&grid);

    inject_calendar_css();

    // ── Assemble container ──────────────────────────────────────────────
    container.append(&calendar_box);
    container.append(&details_group);
    container.append(&rebase_btn);

    // ── Helpers for re-drawing the grid ────────────────────────────────
    let version_map_rc = version_map.clone();
    let selected_rc = selected.clone();
    let details_group_rc = details_group.clone();
    let version_row_rc = version_row.clone();
    let kernel_row_rc = kernel_row.clone();
    let built_row_rc = built_row.clone();
    let commit_row_rc = commit_row.clone();
    let rebase_btn_rc = rebase_btn.clone();
    let month_label_rc = month_label.clone();
    let next_btn_rc = next_btn.clone();

    let redraw = Rc::new(move |grid: &gtk::Grid, displayed: NaiveDate| {
        redraw_grid(
            grid,
            displayed,
            &version_map_rc,
            current_date,
            &selected_rc,
            &details_group_rc,
            &version_row_rc,
            &kernel_row_rc,
            &built_row_rc,
            &commit_row_rc,
            &rebase_btn_rc,
            &month_label_rc,
            &next_btn_rc,
        );
    });

    // Initial draw
    redraw(&grid, *displayed_month.borrow());

    // ── Month navigation ────────────────────────────────────────────────
    {
        let grid = grid.clone();
        let displayed_month = displayed_month.clone();
        let redraw = redraw.clone();
        prev_btn.connect_clicked(move |_| {
            let current = *displayed_month.borrow();
            let prev = if current.month() == 1 {
                NaiveDate::from_ymd_opt(current.year() - 1, 12, 1).unwrap_or(current)
            } else {
                NaiveDate::from_ymd_opt(current.year(), current.month() - 1, 1).unwrap_or(current)
            };
            *displayed_month.borrow_mut() = prev;
            redraw(&grid, prev);
        });
    }

    {
        let grid = grid.clone();
        let displayed_month = displayed_month.clone();
        let redraw = redraw.clone();
        next_btn.connect_clicked(move |_| {
            let current = *displayed_month.borrow();
            let next = if current.month() == 12 {
                NaiveDate::from_ymd_opt(current.year() + 1, 1, 1).unwrap_or(current)
            } else {
                NaiveDate::from_ymd_opt(current.year(), current.month() + 1, 1).unwrap_or(current)
            };
            *displayed_month.borrow_mut() = next;
            redraw(&grid, next);
        });
    }

    // ── Rebase button click → confirm → run bootc switch ───────────────
    {
        let selected_rc = selected.clone();
        let version_map_rc = version_map.clone();
        let dialog_rc = dialog.clone();
        let parent_rc = parent.clone();
        let stack_rc = stack.clone();

        rebase_btn.connect_clicked(move |_| {
            let Some(date) = *selected_rc.borrow() else {
                return;
            };
            let Some(version) = version_map_rc.get(&date).cloned() else {
                return;
            };

            let confirm = adw::AlertDialog::builder()
                .heading("Rebase System?")
                .body(format!(
                    "Your system will be rebased to the {} build (version {}).\n\nThis requires a restart to take effect and will re-download the full image.",
                    date.format("%B %-d, %Y"),
                    version.version,
                ))
                .build();

            confirm.add_response("cancel", "_Cancel");
            confirm.add_response("rebase", "_Rebase");
            confirm.set_response_appearance("rebase", adw::ResponseAppearance::Suggested);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");

            let full_ref = version.full_ref.clone();
            let stack = stack_rc.clone();
            let dialog_close = dialog_rc.clone();

            confirm.connect_response(None, move |_, response| {
                if response == "rebase" {
                    if dev_mode {
                        run_rebase_simulated(full_ref.clone(), stack.clone(), dialog_close.clone());
                    } else {
                        run_rebase(full_ref.clone(), stack.clone(), dialog_close.clone());
                    }
                }
            });

            confirm.present(Some(&parent_rc));
        });
    }
}

// ── Grid redraw ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn redraw_grid(
    grid: &gtk::Grid,
    displayed: NaiveDate,
    versions: &HashMap<NaiveDate, ImageVersion>,
    current_date: Option<NaiveDate>,
    selected: &Rc<RefCell<Option<NaiveDate>>>,
    details_group: &adw::PreferencesGroup,
    version_row: &adw::ActionRow,
    kernel_row: &adw::ActionRow,
    built_row: &adw::ActionRow,
    commit_row: &adw::ActionRow,
    rebase_btn: &gtk::Button,
    month_label: &gtk::Label,
    next_btn: &gtk::Button,
) {
    let today = Local::now().date_naive();

    // Update label
    month_label.set_label(&displayed.format("%B %Y").to_string());

    // Disable next if we're on current month
    let current_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    next_btn.set_sensitive(displayed < current_month);

    let days_in_month = days_in_month(displayed);
    // ISO weekday Mon=0, Sun=6
    let first_weekday = displayed.weekday().num_days_from_monday() as i32;
    let selected_date = *selected.borrow();

    let mut slot = 0i32;
    for row in 0..6i32 {
        for col in 0..7i32 {
            let btn = grid
                .child_at(col, row)
                .and_then(|w| w.downcast::<gtk::Button>().ok());
            let Some(btn) = btn else {
                slot += 1;
                continue;
            };

            let day_num = slot - first_weekday + 1;

            if day_num < 1 || day_num > days_in_month as i32 {
                btn.set_label("");
                btn.set_visible(false);
                btn.set_sensitive(false);
            } else {
                btn.set_visible(true);
                btn.set_label(&day_num.to_string());

                let date =
                    NaiveDate::from_ymd_opt(displayed.year(), displayed.month(), day_num as u32);

                // Clear state classes
                for cls in ["day-available", "day-current", "day-selected", "day-today"] {
                    btn.remove_css_class(cls);
                }

                if let Some(d) = date {
                    let is_available = versions.contains_key(&d);
                    let is_current = current_date == Some(d);
                    let is_selected = selected_date == Some(d);
                    let is_today = d == today;
                    let is_future = d > today;

                    btn.set_sensitive(is_available && !is_future);

                    if is_today {
                        btn.add_css_class("day-today");
                    }
                    if is_available {
                        btn.add_css_class("day-available");
                    }
                    if is_current {
                        btn.add_css_class("day-current");
                    }
                    if is_selected {
                        btn.add_css_class("day-selected");
                    }

                    if is_available && !is_future {
                        // Wire click — disconnect any existing handler first
                        if let Some(hid) =
                            unsafe { btn.steal_data::<glib::SignalHandlerId>("day-handler") }
                        {
                            btn.disconnect(hid);
                        }

                        let selected_inner = selected.clone();
                        let grid_inner = grid.clone();
                        let displayed_inner = displayed;
                        let versions_inner = versions.clone();
                        let current_date_inner = current_date;
                        let details_group_inner = details_group.clone();
                        let version_row_inner = version_row.clone();
                        let kernel_row_inner = kernel_row.clone();
                        let built_row_inner = built_row.clone();
                        let commit_row_inner = commit_row.clone();
                        let rebase_btn_inner = rebase_btn.clone();
                        let month_label_inner = month_label.clone();
                        let next_btn_inner = next_btn.clone();

                        let hid = btn.connect_clicked(move |_| {
                            // Toggle or set selection
                            let prev = *selected_inner.borrow();
                            if prev == Some(d) {
                                *selected_inner.borrow_mut() = None;
                            } else {
                                *selected_inner.borrow_mut() = Some(d);
                            }

                            // Redraw to update selection highlight
                            redraw_grid(
                                &grid_inner,
                                displayed_inner,
                                &versions_inner,
                                current_date_inner,
                                &selected_inner,
                                &details_group_inner,
                                &version_row_inner,
                                &kernel_row_inner,
                                &built_row_inner,
                                &commit_row_inner,
                                &rebase_btn_inner,
                                &month_label_inner,
                                &next_btn_inner,
                            );

                            // Update details panel
                            if let Some(sel_date) = *selected_inner.borrow() {
                                if let Some(v) = versions_inner.get(&sel_date) {
                                    update_details(
                                        &details_group_inner,
                                        &version_row_inner,
                                        &kernel_row_inner,
                                        &built_row_inner,
                                        &commit_row_inner,
                                        &rebase_btn_inner,
                                        v,
                                        &sel_date,
                                        current_date_inner,
                                    );
                                }
                            } else {
                                details_group_inner.set_visible(false);
                                rebase_btn_inner.set_sensitive(false);
                            }
                        });

                        unsafe { btn.set_data("day-handler", hid) };
                    }
                } else {
                    btn.set_sensitive(false);
                }
            }
            slot += 1;
        }
    }
}

fn update_details(
    group: &adw::PreferencesGroup,
    version_row: &adw::ActionRow,
    kernel_row: &adw::ActionRow,
    built_row: &adw::ActionRow,
    commit_row: &adw::ActionRow,
    rebase_btn: &gtk::Button,
    v: &ImageVersion,
    date: &NaiveDate,
    current_date: Option<NaiveDate>,
) {
    version_row.set_subtitle(&v.version);
    kernel_row.set_subtitle(&v.kernel);
    built_row.set_subtitle(&v.created.format("%b %-d, %Y · %H:%M UTC").to_string());
    commit_row.set_subtitle(if v.revision.is_empty() {
        "—"
    } else {
        &v.revision
    });

    group.set_visible(true);

    let is_current = current_date == Some(*date);
    if is_current {
        rebase_btn.set_label("Currently Installed");
        rebase_btn.set_sensitive(false);
    } else {
        rebase_btn.set_label(&format!("Rebase to {}…", date.format("%b %-d")));
        rebase_btn.set_sensitive(true);
    }
}

// ── Rebase worker ────────────────────────────────────────────────────────────

fn run_rebase(full_ref: String, stack: gtk::Stack, dialog: adw::Dialog) {
    // Show a simple progress page while rebasing.
    let progress_page = adw::StatusPage::builder()
        .title("Rebasing…")
        .description("Switching to the selected image.\nThis may take a few minutes.")
        .build();
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    progress_page.set_child(Some(&spinner));
    stack.add_named(&progress_page, Some("rebasing"));
    stack.set_visible_child_name("rebasing");

    let result_slot: Arc<Mutex<Option<Result<(), String>>>> = Arc::new(Mutex::new(None));
    let result_bg = result_slot.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        rt.block_on(async move {
            let result = run_bootc_switch(&full_ref).await;
            *result_bg.lock().unwrap() = Some(result);
        });
    });

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some(result) = result_slot.lock().ok().and_then(|mut g| g.take()) else {
            return glib::ControlFlow::Continue;
        };
        match result {
            Ok(()) => {
                // Show success page — user needs to reboot.
                let done_page = adw::StatusPage::builder()
                    .title("Rebase Complete")
                    .description("Restart your system to boot into the selected version.")
                    .icon_name("object-select-symbolic")
                    .build();
                let close_btn = gtk::Button::builder()
                    .label("Close")
                    .halign(gtk::Align::Center)
                    .build();
                close_btn.add_css_class("suggested-action");
                close_btn.add_css_class("pill");
                let dialog_close = dialog.clone();
                close_btn.connect_clicked(move |_| {
                    dialog_close.close();
                });
                done_page.set_child(Some(&close_btn));
                stack.add_named(&done_page, Some("done"));
                stack.set_visible_child_name("done");
            }
            Err(msg) => {
                let fail_page = adw::StatusPage::builder()
                    .title("Rebase Failed")
                    .description(msg)
                    .icon_name("dialog-error-symbolic")
                    .build();
                let close_btn = gtk::Button::builder()
                    .label("Close")
                    .halign(gtk::Align::Center)
                    .build();
                close_btn.add_css_class("pill");
                let dialog_close = dialog.clone();
                close_btn.connect_clicked(move |_| {
                    dialog_close.close();
                });
                fail_page.set_child(Some(&close_btn));
                stack.add_named(&fail_page, Some("fail"));
                stack.set_visible_child_name("fail");
            }
        }
        glib::ControlFlow::Break
    });
}

async fn run_bootc_switch(full_ref: &str) -> Result<(), String> {
    let status = if is_flatpak() {
        tokio::process::Command::new("flatpak-spawn")
            .args(["--host", "pkexec", "bootc", "switch", full_ref])
            .status()
            .await
    } else {
        tokio::process::Command::new("pkexec")
            .args(["bootc", "switch", full_ref])
            .status()
            .await
    };

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "bootc switch exited with code {}",
            s.code().unwrap_or(-1)
        )),
        Err(e) => Err(format!("Failed to run bootc switch: {}", e)),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Simulated rebase for dev mode — shows the progress UI then succeeds after a delay.
fn run_rebase_simulated(full_ref: String, stack: gtk::Stack, dialog: adw::Dialog) {
    tracing::warn!(
        "Rebase suppressed — developer mode is active. \
         Would have called `bootc switch {}`.",
        full_ref
    );

    let progress_page = adw::StatusPage::builder()
        .title("Rebasing… (simulated)")
        .description("Developer mode — no actual changes are being made.")
        .build();
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    progress_page.set_child(Some(&spinner));
    stack.add_named(&progress_page, Some("rebasing"));
    stack.set_visible_child_name("rebasing");

    // Simulate a short delay then show success.
    glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
        let done_page = adw::StatusPage::builder()
            .title("Rebase Complete (simulated)")
            .description(
                "Developer mode — no changes were made.\nIn production, a restart would be needed.",
            )
            .icon_name("object-select-symbolic")
            .build();
        let close_btn = gtk::Button::builder()
            .label("Close")
            .halign(gtk::Align::Center)
            .build();
        close_btn.add_css_class("suggested-action");
        close_btn.add_css_class("pill");
        let dialog_close = dialog.clone();
        close_btn.connect_clicked(move |_| {
            dialog_close.close();
        });
        done_page.set_child(Some(&close_btn));
        stack.add_named(&done_page, Some("done"));
        stack.set_visible_child_name("done");
    });
}

/// Generate mock image versions for the last 30 days (dev mode).
fn generate_mock_versions() -> Vec<ImageVersion> {
    use chrono::{Duration, Utc};
    use crate::registry_client::RegistryClient;

    let (registry, org, image) = if let Some(client) = RegistryClient::detect_from_os_release() {
        (client.registry().to_string(), client.org().to_string(), client.image().to_string())
    } else {
        ("ghcr.io".to_string(), "projectbluefin".to_string(), "dakota".to_string())
    };

    let today = Utc::now().date_naive();
    let mut versions = Vec::new();

    // Generate a version every 2-3 days over the last 60 days.
    let mut day_offset = 2i64;
    let mut build_num = 1u32;
    while day_offset <= 60 {
        let date = today - Duration::days(day_offset);
        let tag = format!("latest-{}", date.format("%Y%m%d"));
        versions.push(ImageVersion {
            date,
            full_ref: format!("{}/{}/{}:{}", registry, org, image, tag),
            version: tag,
            kernel: format!("6.12.{}-200.fc42.x86_64", build_num),
            revision: format!("{:08x}", 0xdeadbe00 + build_num),
            created: date.and_hms_opt(4, 30, 0).unwrap().and_utc(),
        });
        day_offset += if build_num % 3 == 0 { 3 } else { 2 };
        build_num += 1;
    }

    versions.sort_by_key(|v| v.date);
    versions
}

fn days_in_month(date: NaiveDate) -> u32 {
    let next = if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)
    };
    next.unwrap_or(date)
        .signed_duration_since(
            NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date),
        )
        .num_days() as u32
}

fn inject_calendar_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        r#"
        .day-btn {
            min-width: 36px;
            min-height: 36px;
            padding: 0;
            border-radius: 18px;
            font-size: 0.85em;
        }
        .day-btn:not(:sensitive) { opacity: 0.3; }
        .day-available           { color: @accent_color; font-weight: bold; }
        .day-current             { background-color: @accent_bg_color; color: @accent_fg_color; }
        .day-selected:not(.day-current) {
            outline: 2px solid @accent_color;
            outline-offset: -2px;
        }
        .day-today label { text-decoration: underline; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// ── Message type for background fetch ────────────────────────────────────────

enum FetchResult {
    Ok(Vec<ImageVersion>),
    Err(String),
    DetectFailed,
}
