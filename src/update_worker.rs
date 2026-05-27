//! Async subprocess worker for invoking `uupd`.
//!
//! Pattern: Subprocess streaming with Flatpak sandbox awareness
//! - Detect if running inside a Flatpak sandbox (via /.flatpak-info)
//! - If sandboxed, use `flatpak-spawn --host` to run commands on the host
//! - Spawn the process with stdout piped
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
    /// The process failed — includes a human-readable error description.
    Error(String),
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
    /// The caller should poll `rx.recv()` in a loop until it gets
    /// `Complete` or `Error`, at which point the channel closes.
    ///
    /// # Design decision: mpsc over callbacks
    /// Using a channel decouples the worker from relm4 entirely.
    /// This means the worker can be unit-tested with a simple receiver
    /// without needing a running GTK main loop.
    pub async fn run(&mut self) -> mpsc::UnboundedReceiver<UpdateEvent> {
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
            let child = cmd.spawn();

            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(UpdateEvent::Error(format!(
                        "Failed to start '{}': {}",
                        command_display, e
                    )));
                    return;
                }
            };

            // Stream stdout line-by-line.
            // We merge stderr into the output stream so the user sees all feedback.
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            // Spawn a task to read stderr and forward it too.
            let tx_stderr = tx.clone();
            let stderr_handle = stderr.map(|stderr| {
                tokio::spawn(async move {
                    let reader = BufReader::new(stderr);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if tx_stderr.send(UpdateEvent::Output(line)).is_err() {
                            break;
                        }
                    }
                })
            });

            // Read stdout on the current task.
            if let Some(stdout) = stdout {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx.send(UpdateEvent::Output(line)).is_err() {
                        break;
                    }
                }
            }

            // Wait for stderr task to finish.
            if let Some(handle) = stderr_handle {
                let _ = handle.await;
            }

            // Wait for the child to exit and check status.
            match child.wait().await {
                Ok(status) if status.success() => {
                    let _ = tx.send(UpdateEvent::Complete);
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
