@smoke_suite @finupdate
Feature: Finupdate smoke tests
  Validates that the finupdate Flatpak launches, exposes its main controls
  via AT-SPI, and the developer-mode simulated update path completes without
  touching the host system.

  Regression coverage for the GUI getting too far ahead of the backend — these
  scenarios fail loudly if widget wiring breaks during refactors.

  Background:
    * Start application "finupdate" via "command"
    * Wait until "Finupdate" "frame" appears in "finupdate"

  # ── Launch & main window ────────────────────────────────────────────────

  @launch
  Scenario: Application window appears with the main controls
    * Application "finupdate" is running
    * Item "Finupdate" "frame" is "showing" in "finupdate"
    * Item "Check for Updates" "push button" is "showing" in "finupdate"

  # ── Menu access ─────────────────────────────────────────────────────────

  @menu
  Scenario: Hamburger menu opens
    * Left click "Main Menu" "toggle button" in "finupdate"
    * Wait until "Preferences" "menu item" appears in "finupdate"

  @menu
  Scenario: Keyboard shortcut window opens via Ctrl+question
    * Key combo: "<Control>question"
    * Wait until "Keyboard Shortcuts" "frame" appears in "finupdate"

  # ── Preferences dialog ──────────────────────────────────────────────────

  @preferences
  Scenario: Preferences dialog opens from the menu
    * Left click "Main Menu" "toggle button" in "finupdate"
    * Left click "Preferences" "menu item" in "finupdate"
    * Wait until "Preferences" "dialog" appears in "finupdate"

  @preferences @dev_mode
  Scenario: Developer Mode toggle is reachable in Preferences
    * Left click "Main Menu" "toggle button" in "finupdate"
    * Left click "Preferences" "menu item" in "finupdate"
    * Wait until "Preferences" "dialog" appears in "finupdate"
    * Item "Developer Mode" "switch" is "showing" in "finupdate"

  # ── Dev-mode simulated update ───────────────────────────────────────────
  # These exercise the full UI state machine without root or a live system.

  @dev_mode @simulator
  Scenario: Simulated update completes (dev mode, Success scenario)
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check for Updates" "push button" in "finupdate"
    * Wait until "Updates available" appears in "finupdate" within 10 seconds
    * Activate "Install Updates" "push button" in "finupdate"
    * Wait until "Updates complete" appears in "finupdate" within 30 seconds

  @dev_mode @simulator
  Scenario: Simulated update reports up-to-date (dev mode, AlreadyUpToDate)
    * Application "finupdate" is in developer mode with scenario "AlreadyUpToDate"
    * Left click "Check for Updates" "push button" in "finupdate"
    * Wait until "Up to date" appears in "finupdate" within 10 seconds

  @dev_mode @simulator
  Scenario: Simulated update surfaces an error (dev mode, Failure)
    * Application "finupdate" is in developer mode with scenario "Failure"
    * Left click "Check for Updates" "push button" in "finupdate"
    * Activate "Install Updates" "push button" in "finupdate"
    * Wait until "failed" appears in "finupdate" within 30 seconds

  # ── Clean shutdown ──────────────────────────────────────────────────────

  @close
  Scenario: Application closes cleanly via Ctrl+Q
    * Close application "finupdate" via "shortcut"
    * Application "finupdate" is no longer running
