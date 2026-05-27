//! Async subprocess worker for invoking `uupd`.
//!
//! ## About uupd
//!
//! `uupd` (Universal Update) is the system update daemon for Universal Blue / Bluefin.
//! It coordinates updates across 4 independent modules:
//!
//! | Module | What it updates | Backend |
//! |--------|----------------|---------|
//! | System | The OS image (bootc/rpm-ostree) | `bootc upgrade` or `rpm-ostree upgrade` |
//! | Flatpak | User + system Flatpak apps | `flatpak update` |
//! | Brew | Homebrew packages in /home/linuxbrew | `brew upgrade` |
//! | Distrobox | Container-based distro environments | `distrobox upgrade` |
//!
//! ### Key behaviors:
//! - **Requires root** — must be run as `sudo uupd` (or via polkit/pkexec)
//! - **Lock file** — only one instance can run at a time
//! - **Exit codes** — 0 = success, non-zero = at least one module failed
//! - **Logging** — uses Go's `slog`; default `info` level shows module progress
//! - **Progress** — emits OSC terminal progress sequences (disabled with `--json`)
//! - **Config** — reads `/etc/uupd/config.json` for module enable/disable
//! - **Systemd** — normally runs via `uupd.timer` daily at 04:00
//! - **update-check** — exits 0 if update available, 77 if not (useful for pre-checking)
//!
//! ### Useful flags for GUI integration:
//! - `--verbose` / `-v` — shows command output from each module (more log lines)
//! - `--dry-run` / `-n` — simulates without making changes
//! - `--force` / `-f` — skips update-check, forces system module to run
//! - `--json` — structured JSON log output (parseable but noisy)
//! - `--disable-module-<name>` — skip a specific module
//! - `--log-level debug` — very verbose, shows all subprocess output
//!
//! ### Output format (default info level):
//! ```text
//! time=... level=INFO msg="Hardware checks passed"
//! time=... level=INFO msg="System" module_name=System
//! time=... level=INFO msg="Flatpak" module_name=Flatpak
//! time=... level=INFO msg="Updates Completed Successfully"
//! ```
//!
//! On failure:
//! ```text
//! time=... level=ERROR msg="module_fail" module=System cli="bootc upgrade"
//! time=... level=ERROR msg="Updates finished with errors!"
//! ```
//!
//! ## Pattern: Subprocess streaming with Flatpak sandbox awareness
//!
//! - Detect if running inside a Flatpak sandbox (via /.flatpak-info)
//! - If sandboxed, use `flatpak-spawn --host` to run commands on the host
//! - Spawn the process with stdout/stderr piped
//! - Read lines asynchronously via tokio
//! - Send structured events back through an mpsc channel
//! - The caller (relm4 component) receives events and updates UI state
//!
//! This decoupling means the UI never blocks on I/O, and the worker
//! is testable in isolation (you can substitute a mock process).

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Events emitted by the update worker as the subprocess runs.
#[derive(Debug, Clone)]
pub enum UpdateEvent {
    /// A line of stdout/stderr output from uupd.
    Output(String),
    /// The process exited successfully (exit code 0).
    Complete,
    /// The process exited with code 77 — no update was needed.
    /// `uupd update-check` uses this code to signal "already up to date".
    UpToDate,
    /// The process failed — includes a human-readable error description.
    Error(String),
}

/// What the simulated update run should end with.
#[derive(Debug, Clone, Copy)]
pub enum SimulationScenario {
    /// All four modules succeed, update completes normally.
    Success,
    /// System is already current — ends with UpToDate.
    AlreadyUpToDate,
    /// System module fails — ends with an Error.
    Failure,
}

/// Detect if we're running inside a Flatpak sandbox.
/// The canonical check is the existence of `/.flatpak-info`.
pub fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Manages spawning and streaming output from the `uupd` process.
/// Handles Flatpak sandbox transparency automatically.
pub struct UpdateWorker {
    /// The command to invoke. Defaults to "uupd" but can be overridden for testing.
    command: String,
    /// Arguments to pass. Default: empty (uupd runs a full update with no args).
    args: Vec<String>,
}

impl UpdateWorker {
    /// Create a new worker with default uupd invocation.
    pub fn new() -> Self {
        Self {
            command: "uupd".to_string(),
            args: vec![],
        }
    }

    /// Create a worker with a custom command (useful for testing/mocking).
    #[allow(dead_code)]
    pub fn with_command(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
        }
    }

    /// Build the actual Command, wrapping with `flatpak-spawn --host` if sandboxed.
    fn build_command(&self) -> Command {
        if is_flatpak() {
            // Inside Flatpak: use flatpak-spawn to escape the sandbox and
            // run the command on the host system.
            let mut cmd = Command::new("flatpak-spawn");
            cmd.arg("--host");
            cmd.arg(&self.command);
            cmd.args(&self.args);
            cmd
        } else {
            // Running natively: invoke the command directly.
            let mut cmd = Command::new(&self.command);
            cmd.args(&self.args);
            cmd
        }
    }

    /// Spawn the subprocess and return a receiver for streaming events.
    ///
    /// `cancel_rx` is a oneshot channel. When the sender fires, the subprocess
    /// is killed with SIGKILL and the channel closes. This ensures the real
    /// process is always terminated, not just abandoned.
    ///
    /// # Design decision: mpsc over callbacks
    /// Using a channel decouples the worker from relm4 entirely.
    /// This means the worker can be unit-tested with a simple receiver
    /// without needing a running GTK main loop.
    pub async fn run(
        &mut self,
        cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> mpsc::UnboundedReceiver<UpdateEvent> {
        let (tx, rx) = mpsc::unbounded_channel();

        let command_display = if is_flatpak() {
            format!("flatpak-spawn --host {}", self.command)
        } else {
            self.command.clone()
        };

        let mut cmd = self.build_command();
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        tokio::spawn(async move {
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(UpdateEvent::Error(format!(
                        "Failed to start '{}': {}",
                        command_display, e
                    )));
                    return;
                }
            };

            // Stream stdout and stderr line-by-line, merging both into Output events.
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            let tx_err = tx.clone();
            let stderr_task = stderr.map(|s| {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(s).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if tx_err.send(UpdateEvent::Output(line)).is_err() {
                            break;
                        }
                    }
                })
            });

            let tx_out = tx.clone();
            let stdout_future = async move {
                if let Some(stdout) = stdout {
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if tx_out.send(UpdateEvent::Output(line)).is_err() {
                            break;
                        }
                    }
                }
            };

            // Race between reading all output and receiving a cancel signal.
            let cancelled = tokio::select! {
                _ = stdout_future => false,
                _ = cancel_rx => true,
            };

            // Always clean up the stderr reader task.
            if let Some(task) = stderr_task {
                task.abort();
                let _ = task.await;
            }

            if cancelled {
                // Kill the subprocess so it doesn't linger in the background.
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = tx.send(UpdateEvent::Error("Update cancelled by user".to_string()));
                return;
            }

            // Wait for the child to exit and check status.
            match child.wait().await {
                Ok(status) if status.success() => {
                    let _ = tx.send(UpdateEvent::Complete);
                }
                Ok(status) if status.code() == Some(77) => {
                    // Exit code 77 means "nothing to update" (used by uupd update-check
                    // and potentially by the main uupd command when image is current).
                    let _ = tx.send(UpdateEvent::UpToDate);
                }
                Ok(status) => {
                    let code = status.code().unwrap_or(-1);
                    let _ = tx.send(UpdateEvent::Error(format!(
                        "Update process exited with code {}",
                        code
                    )));
                }
                Err(e) => {
                    let _ = tx.send(UpdateEvent::Error(format!(
                        "Error waiting for update process: {}",
                        e
                    )));
                }
            }
        });

        rx
    }
}

/// Run a scripted simulation of a uupd update run without touching the real system.
///
/// Emits realistic-looking log lines for all four modules with short delays,
/// then ends according to `scenario`. Respects `cancel_rx` — if cancelled,
/// emits an error event and returns.
///
/// This is the backend for Developer Mode: allows walking through the entire
/// UI flow (progress, per-module status, completion/failure screens) without
/// root privileges or a live uupd installation.
pub async fn run_simulated(
    scenario: SimulationScenario,
    cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> mpsc::UnboundedReceiver<UpdateEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let tx2 = tx.clone();
        let do_sim = async move { simulate_update(&tx2, scenario).await };

        tokio::select! {
            _ = do_sim => {}
            _ = cancel_rx => {
                let _ = tx.send(UpdateEvent::Error("Update cancelled by user".to_string()));
            }
        }
    });

    rx
}

/// Internal simulation: emits fake log lines for all four uupd modules.
async fn simulate_update(tx: &mpsc::UnboundedSender<UpdateEvent>, scenario: SimulationScenario) {
    use tokio::time::{sleep, Duration};

    // Helper: emit a log line and pause briefly.
    macro_rules! line {
        ($msg:expr) => {{
            if tx.send(UpdateEvent::Output($msg.to_string())).is_err() {
                return;
            }
            sleep(Duration::from_millis(300)).await;
        }};
        ($msg:expr, $delay:expr) => {{
            if tx.send(UpdateEvent::Output($msg.to_string())).is_err() {
                return;
            }
            sleep(Duration::from_millis($delay)).await;
        }};
    }

    let ts = "2026-05-26T04:00:00Z";

    line!(format!("time={ts} level=INFO msg=\"[DEV MODE] Starting finupdate simulation\""));
    line!(format!("time={ts} level=INFO msg=\"Hardware checks passed\""), 500);

    // ── System module ────────────────────────────────────────────────────
    line!(format!("time={ts} level=INFO msg=\"System\" module_name=System"));
    line!(format!("time={ts} level=INFO msg=\"Checking for OS image updates...\""), 600);

    if matches!(scenario, SimulationScenario::AlreadyUpToDate) {
        line!(format!("time={ts} level=INFO msg=\"Image is already up to date\""), 400);
        let _ = tx.send(UpdateEvent::UpToDate);
        return;
    }

    if matches!(scenario, SimulationScenario::Failure) {
        line!(format!("time={ts} level=ERROR msg=\"module_fail\" module=System cli=\"bootc upgrade\""), 400);
        let _ = tx.send(UpdateEvent::Error(
            "[DEV MODE] Simulated system module failure".to_string(),
        ));
        return;
    }

    line!(format!("time={ts} level=INFO msg=\"bootc: Fetching image manifest...\""), 500);
    line!(format!("time={ts} level=INFO msg=\"bootc: Pulling 3 new layers (42.1 MB)\""), 800);
    line!(format!("time={ts} level=INFO msg=\"bootc: Staging complete — reboot to apply\""), 400);

    // ── Flatpak module ───────────────────────────────────────────────────
    line!(format!("time={ts} level=INFO msg=\"Flatpak\" module_name=Flatpak"));
    line!(format!("time={ts} level=INFO msg=\"Checking for Flatpak updates...\""), 500);
    line!(format!("time={ts} level=INFO msg=\"Updated: org.mozilla.firefox (130.0.1 → 131.0)\""), 400);
    line!(format!("time={ts} level=INFO msg=\"Updated: com.spotify.Client (1.2.45 → 1.2.46)\""), 400);
    line!(format!("time={ts} level=INFO msg=\"Flatpak: 2 apps updated\""), 300);

    // ── Brew module ──────────────────────────────────────────────────────
    line!(format!("time={ts} level=INFO msg=\"Brew\" module_name=Brew"));
    line!(format!("time={ts} level=INFO msg=\"Upgrading Homebrew packages...\""), 500);
    line!(format!("time={ts} level=INFO msg=\"Already up-to-date: neovim, fzf, ripgrep\""), 300);

    // ── Distrobox module ─────────────────────────────────────────────────
    line!(format!("time={ts} level=INFO msg=\"Distrobox\" module_name=Distrobox"));
    line!(format!("time={ts} level=INFO msg=\"Upgrading Distrobox containers...\""), 500);
    line!(format!("time={ts} level=INFO msg=\"ubuntu-22: updated 5 packages\""), 400);

    sleep(Duration::from_millis(300)).await;
    line!(format!("time={ts} level=INFO msg=\"Updates Completed Successfully\""), 200);

    let _ = tx.send(UpdateEvent::Complete);
}
