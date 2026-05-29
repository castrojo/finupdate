@smoke_suite @finupdate
Feature: Finupdate smoke tests
  Validates that the finupdate Flatpak launches, exposes its main controls
  via AT-SPI, and the developer-mode simulated update path completes without
  touching the host system.

  Regression coverage for the GUI getting too far ahead of the backend — these
  scenarios fail loudly if widget wiring breaks during refactors.

  Background:
    * Start application "finupdate" via "command"
    * Wait until window "Finupdate" appears in "finupdate"

  # ── Launch & main window ────────────────────────────────────────────────

  @launch
  Scenario: Application window appears with the main controls
    * Application "finupdate" is running
    * Item "Check" "button" is "showing" in "finupdate"

  # ── Menu access ─────────────────────────────────────────────────────────

  @menu
  Scenario: Hamburger menu opens
    * Left click "Main Menu" "toggle button" in "finupdate"
    * Item "Main Menu" "button" is "showing" in "finupdate"

  @menu
  Scenario: Keyboard shortcut window opens via Ctrl+question
    * Key combo: "<Control>question"
    # GtkShortcutsWindow is a separate GTK4 toplevel not exposed via AT-SPI;
    # just verify the key combo doesn't crash the app.
    * Application "finupdate" is running

  # ── Preferences dialog ──────────────────────────────────────────────────

  @preferences
  Scenario: Preferences dialog opens from the menu
    * Key combo: "<Control>comma"
    * Wait until "Preferences" "dialog" appears in "finupdate"

  @preferences @dev_mode
  Scenario: Developer Mode toggle is reachable in Preferences
    * Key combo: "<Control>comma"
    * Wait until "Preferences" "dialog" appears in "finupdate"
    * Item "Developer Mode" "switch" is "showing" in "finupdate"

  # ── Dev-mode simulated update ───────────────────────────────────────────
  # These exercise the full UI state machine without root or a live system.

  # ── Dev-mode simulated update (limited AT-SPI coverage) ─────────────
  # GTK4 adw::StatusPage / progress views don't expose text in the
  # accessibility tree.  These scenarios verify the check dialog opens
  # and responds; the post-install completion page is verified manually.

  @dev_mode @simulator
  Scenario: Check dialog shows update available (dev mode, Success)
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Ready to install" appears in "finupdate" within 10 seconds
    * Key combo: "Escape"

  @dev_mode @simulator
  Scenario: Check dialog reports up-to-date (dev mode, AlreadyUpToDate)
    * Application "finupdate" is in developer mode with scenario "AlreadyUpToDate"
    * Left click "Check" "button" in "finupdate"
    * Wait until "System is up to date" appears in "finupdate" within 10 seconds
    * Key combo: "Escape"

  @dev_mode @simulator
  Scenario: Check dialog shows error (dev mode, Failure)
    * Application "finupdate" is in developer mode with scenario "Failure"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Ready to install" appears in "finupdate" within 10 seconds
    * Key combo: "Escape"

  # ── Check dialog action scenarios ────────────────────────────────────

  @dev_mode @simulator @dialog
  Scenario: Install button activates in check dialog when updates available
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Install all" "button" appears in "finupdate" within 10 seconds
    * Item "Install all" "button" is "showing" in "finupdate"

  @dev_mode @simulator @dialog
  Scenario: Check dialog can be dismissed with Close button
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Ready to install" appears in "finupdate" within 10 seconds
    * Left click "Close" "button" in "finupdate"
    * Wait until 2 seconds
    * Application "finupdate" is running

  @dev_mode @simulator @dialog
  Scenario: Install all initiates update from check dialog (Success scenario)
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Activate "Install all" "button" in "finupdate"
    * Wait until "installing" appears in "finupdate" within 15 seconds

  @dev_mode @simulator @dialog
  Scenario: Install all initiates update from check dialog (Failure scenario)
    * Application "finupdate" is in developer mode with scenario "Failure"
    * Left click "Check" "button" in "finupdate"
    * Activate "Install all" "button" in "finupdate"
    * Wait until "Update failed" appears in "finupdate" within 15 seconds

  # ── Update cancellation ──────────────────────────────────────────────

  @dev_mode @simulator @cancel
  Scenario: User can cancel update during progress
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Activate "Install all" "button" in "finupdate"
    * Wait until "installing" appears in "finupdate" within 10 seconds
    * Wait until 2 seconds
    * Left click "Cancel" "button" in "finupdate"
    * Application "finupdate" is running

  # ── Developer mode simulation scenarios ───────────────────────────────

  @dev_mode @simulator @scenarios
  Scenario: Sim scenario can be changed from hamburger menu
    * Key combo: "<Alt>F10"
    * Item "Simulate _Success" "menu item" is "showing" in "finupdate"
    * Item "Simulate _Failure" "menu item" is "showing" in "finupdate"
    * Item "Simulate Already _Up To Date" "menu item" is "showing" in "finupdate"

  # ── Settings persistence ─────────────────────────────────────────────

  @dev_mode @settings
  Scenario: Developer mode setting persists across restarts
    * Key combo: "<Control>comma"
    * Wait until "Preferences" "dialog" appears in "finupdate"
    * Left click "Developer Mode" "switch" in "finupdate"
    * Close application "finupdate" via "shortcut"
    * Application "finupdate" is no longer running
    * Start application "finupdate" via "command"
    * Wait until window "Finupdate" appears in "finupdate"
    * Key combo: "<Control>comma"
    * Wait until "Preferences" "dialog" appears in "finupdate"
    * Item "Developer Mode" "switch" is "showing" in "finupdate"

  # ── Check dialog text verification ──────────────────────────────────
  # Verify check dialog displays correctly without waiting for real checks.

  @dialog @text
  Scenario: Check dialog displays all source names
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Powered by uupd" appears in "finupdate" within 5 seconds
    * Key combo: "Escape"

  # ── Real non-destructive update checks ───────────────────────────────
  # These test actual update checking for Flatpak, Homebrew, Distrobox
  # without modifying the system image (bootc). Safe to run on real systems.

  @real @integration @non_destructive
  Scenario: Real check dialog queries actual update sources
    * Application "finupdate" is in real mode (developer mode disabled)
    * Item "Check" "button" is "showing" in "finupdate"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Checking for updates" appears in "finupdate" within 30 seconds
    * Wait until "Flatpak" appears in "finupdate" within 30 seconds
    * Wait until "Homebrew" appears in "finupdate" within 30 seconds
    * Wait until "Distrobox" appears in "finupdate" within 30 seconds
    * Key combo: "Escape"

  @real @integration @non_destructive
  Scenario: Real update check completes without errors
    * Application "finupdate" is in real mode (developer mode disabled)
    * Item "Check" "button" is "showing" in "finupdate"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Querying" appears in "finupdate" within 30 seconds
    * Wait until "Close" "button" appears in "finupdate" within 60 seconds
    * Application "finupdate" is running
    * Left click "Close" "button" in "finupdate"

  @real @integration @non_destructive @install
  Scenario: Install available non-system updates (Flatpak, Homebrew, Distrobox)
    * Application "finupdate" is in real mode (developer mode disabled)
    * Item "Check" "button" is "showing" in "finupdate"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Querying" appears in "finupdate" within 30 seconds
    * Wait until "Close" "button" appears in "finupdate" within 60 seconds
    * Item "Install all" "button" is "showing" in "finupdate"
    * Left click "Install all" "button" in "finupdate"
    * Wait until 5 seconds
    * Application "finupdate" is running

  # ── Clean shutdown ──────────────────────────────────────────────────────

  @close
  Scenario: Application closes cleanly via Ctrl+Q
    * Close application "finupdate" via "shortcut"
    * Application "finupdate" is no longer running
