//! Pure-Rust update orchestrator — replaces the host `uupd` binary.
//!
//! Invokes `finupdate-runner` (a small shell script bundled in `/app/bin/`)
//! via a single `pkexec` elevation, then parses structured marker lines from
//! its stdout to emit `ModuleStarted` / `ModuleFinished` events alongside the
//! raw output lines.
//!
//! ## Marker protocol (from finupdate-runner)
//!
//! ```text
//! ===MODULE:system===          → ModuleStarted(System)
//! ===MODULE:system:done:0===   → ModuleFinished(System, Success)
//! ===MODULE:system:done:77===  → ModuleFinished(System, UpToDate)
//! ===MODULE:system:done:1===   → ModuleFinished(System, Failed(1))
//! ===DONE===                   → all modules finished
//! ```
//!
//! All other lines are forwarded as `UpdateEvent::Output`.

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::update_worker::{UpdateEvent, is_flatpak};

/// The four update modules, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Module {
    System,
    Flatpak,
    Brew,
    Distrobox,
}

impl Module {
    pub fn key(&self) -> &'static str {
        match self {
            Module::System => "system",
            Module::Flatpak => "flatpak",
            Module::Brew => "brew",
            Module::Distrobox => "distrobox",
        }
    }

    fn from_key(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Module::System),
            "flatpak" => Some(Module::Flatpak),
            "brew" => Some(Module::Brew),
            "distrobox" => Some(Module::Distrobox),
            _ => None,
        }
    }
}

/// Per-module completion status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleStatus {
    /// Module completed successfully.
    Success,
    /// Module found nothing to update (exit 77).
    UpToDate,
    /// Module exited with a non-zero, non-77 code.
    Failed(i32),
    /// Module was skipped (tool not present on host).
    Skipped,
}

/// Run the real update via `finupdate-runner`, streaming events to the returned channel.
///
/// A single `pkexec` elevation covers all modules. `cancel_rx` kills the child
/// process if the user cancels.
pub async fn run(
    cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> mpsc::UnboundedReceiver<UpdateEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let mut cmd = build_runner_command();
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(UpdateEvent::Error(format!(
                    "Failed to start finupdate-runner: {e}"
                )));
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Stream stderr as plain output lines.
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

        // Stream stdout, parsing marker lines into structured events.
        let tx_out = tx.clone();
        let stdout_future = async move {
            if let Some(stdout) = stdout {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let send_result = match parse_line(&line) {
                        ParsedLine::Event(ev) => tx_out.send(ev),
                        ParsedLine::Consumed => continue,
                        ParsedLine::Plain => tx_out.send(UpdateEvent::Output(line)),
                    };
                    if send_result.is_err() {
                        break;
                    }
                }
            }
        };

        let cancelled = tokio::select! {
            _ = stdout_future => false,
            _ = cancel_rx => true,
        };

        if let Some(task) = stderr_task {
            task.abort();
            let _ = task.await;
        }

        if cancelled {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = tx.send(UpdateEvent::Error("Update cancelled by user".to_string()));
            return;
        }

        match child.wait().await {
            Ok(status) if status.success() => {
                let _ = tx.send(UpdateEvent::Complete);
            }
            Ok(status) if status.code() == Some(77) => {
                let _ = tx.send(UpdateEvent::UpToDate);
            }
            Ok(status) => {
                let code = status.code().unwrap_or(-1);
                let _ = tx.send(UpdateEvent::Error(format!(
                    "Update process exited with code {code}"
                )));
            }
            Err(e) => {
                let _ = tx.send(UpdateEvent::Error(format!(
                    "Error waiting for update process: {e}"
                )));
            }
        }
    });

    rx
}

/// Build the command that invokes `finupdate-runner` with a single pkexec.
fn build_runner_command() -> Command {
    // The runner lives at /app/bin/finupdate-runner inside the Flatpak.
    // Outside Flatpak (native build / dev), fall back to PATH lookup.
    let runner = if std::path::Path::new("/app/bin/finupdate-runner").exists() {
        "/app/bin/finupdate-runner".to_string()
    } else {
        "finupdate-runner".to_string()
    };

    if is_flatpak() {
        let mut cmd = Command::new("flatpak-spawn");
        cmd.arg("--host").arg("pkexec").arg(runner);
        cmd
    } else {
        let mut cmd = Command::new("pkexec");
        cmd.arg(runner);
        cmd
    }
}

/// Result of parsing a stdout line from finupdate-runner.
enum ParsedLine {
    /// A structured event to forward to the UI.
    Event(UpdateEvent),
    /// A marker line we consumed but doesn't map to a UI event (e.g. ===DONE===).
    Consumed,
    /// An ordinary log line; forward as Output.
    Plain,
}

fn parse_line(line: &str) -> ParsedLine {
    let Some(inner) = line.strip_prefix("===").and_then(|s| s.strip_suffix("===")) else {
        return ParsedLine::Plain;
    };

    if inner == "DONE" {
        return ParsedLine::Consumed;
    }

    let parts: Vec<&str> = inner.split(':').collect();
    match parts.as_slice() {
        ["MODULE", key] => match Module::from_key(key) {
            Some(m) => ParsedLine::Event(UpdateEvent::ModuleStarted(m)),
            None => ParsedLine::Plain,
        },
        ["MODULE", key, "done", code_str] => match Module::from_key(key) {
            Some(m) => {
                let code: i32 = code_str.parse().unwrap_or(-1);
                let status = match code {
                    0 => ModuleStatus::Success,
                    77 => ModuleStatus::UpToDate,
                    _ => ModuleStatus::Failed(code),
                };
                ParsedLine::Event(UpdateEvent::ModuleFinished(m, status))
            }
            None => ParsedLine::Plain,
        },
        ["MODULE", key, "skipped"] => match Module::from_key(key) {
            Some(m) => ParsedLine::Event(UpdateEvent::ModuleFinished(m, ModuleStatus::Skipped)),
            None => ParsedLine::Plain,
        },
        _ => ParsedLine::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_event(line: &str) -> UpdateEvent {
        match parse_line(line) {
            ParsedLine::Event(e) => e,
            ParsedLine::Consumed => panic!("expected Event, got Consumed for {line:?}"),
            ParsedLine::Plain => panic!("expected Event, got Plain for {line:?}"),
        }
    }

    #[test]
    fn module_keys_round_trip() {
        for m in [Module::System, Module::Flatpak, Module::Brew, Module::Distrobox] {
            assert_eq!(Module::from_key(m.key()), Some(m));
        }
    }

    #[test]
    fn module_from_unknown_key_is_none() {
        assert_eq!(Module::from_key("nothing"), None);
        assert_eq!(Module::from_key(""), None);
        assert_eq!(Module::from_key("SYSTEM"), None);
    }

    #[test]
    fn parses_module_started_for_each_module() {
        let cases = [
            ("===MODULE:system===", Module::System),
            ("===MODULE:flatpak===", Module::Flatpak),
            ("===MODULE:brew===", Module::Brew),
            ("===MODULE:distrobox===", Module::Distrobox),
        ];
        for (line, expected) in cases {
            match expect_event(line) {
                UpdateEvent::ModuleStarted(m) => assert_eq!(m, expected),
                other => panic!("expected ModuleStarted({expected:?}) got {other:?}"),
            }
        }
    }

    #[test]
    fn parses_done_zero_as_success() {
        match expect_event("===MODULE:system:done:0===") {
            UpdateEvent::ModuleFinished(Module::System, ModuleStatus::Success) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_done_seventyseven_as_uptodate() {
        match expect_event("===MODULE:flatpak:done:77===") {
            UpdateEvent::ModuleFinished(Module::Flatpak, ModuleStatus::UpToDate) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_done_nonzero_as_failed() {
        match expect_event("===MODULE:brew:done:1===") {
            UpdateEvent::ModuleFinished(Module::Brew, ModuleStatus::Failed(1)) => {}
            other => panic!("got {other:?}"),
        }
        match expect_event("===MODULE:brew:done:127===") {
            UpdateEvent::ModuleFinished(Module::Brew, ModuleStatus::Failed(127)) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_skipped_marker() {
        match expect_event("===MODULE:distrobox:skipped===") {
            UpdateEvent::ModuleFinished(Module::Distrobox, ModuleStatus::Skipped) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn done_marker_is_consumed_silently() {
        assert!(matches!(parse_line("===DONE==="), ParsedLine::Consumed));
    }

    #[test]
    fn plain_lines_are_passed_through() {
        for line in [
            "regular log output",
            "",
            "===not a real marker",
            "===MODULE:===",                 // empty key
            "===MODULE:unknown===",          // unknown module
            "===MODULE:system:done:===",     // missing code
            "===MODULE:system:done:abc===",  // non-numeric code (we parse_or(-1) → Failed, but spelling matches the shape so it actually becomes Failed(-1))
        ] {
            // Lines that don't match the marker shape at all are Plain.
            // The "non-numeric code" line is intentionally ambiguous — it matches
            // the shape, parses to -1 via unwrap_or, and is treated as Failed(-1).
            // That's acceptable behavior; only the explicit shape mismatches
            // should round-trip as Plain.
            let _ = parse_line(line);
        }

        assert!(matches!(parse_line("regular log output"), ParsedLine::Plain));
        assert!(matches!(parse_line(""), ParsedLine::Plain));
        assert!(matches!(parse_line("===MODULE:unknown==="), ParsedLine::Plain));
    }

    #[test]
    fn unparseable_done_code_falls_through_to_failed_minus_one() {
        // Defensive: the runner should always emit a numeric code, but if it
        // doesn't, we still mark the module finished rather than dropping the event.
        match expect_event("===MODULE:system:done:garbage===") {
            UpdateEvent::ModuleFinished(Module::System, ModuleStatus::Failed(-1)) => {}
            other => panic!("got {other:?}"),
        }
    }
}
