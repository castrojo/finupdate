"""
Custom step definitions for the finupdate smoke suite.

`qecore.common_steps` covers the low-level lifecycle and most UI actions; the
custom steps here cover finupdate-specific affordances:

- Activating Developer Mode + selecting a simulation scenario before triggering
  an update (so the same scenarios can drive Success / AlreadyUpToDate /
  Failure without root or a live system).
- "Activate" — a convenience that waits-for + clicks, since several buttons
  appear after async work.
- Time-bounded waits with a friendlier error message than `assert child is None`.
"""
import json
import os
import time
from pathlib import Path

from behave import step
from qecore.common_steps import *  # noqa: F401,F403


# ── Helpers ────────────────────────────────────────────────────────────────

def _settings_path() -> Path:
    """Settings live inside the Flatpak sandbox's per-app config dir."""
    # gtk::glib::user_config_dir() inside the sandbox resolves to
    # ~/.var/app/<app-id>/config — at least on Linux/Flatpak.
    base = Path.home() / ".var/app/org.projectbluefin.Finupdate.Devel/config"
    return base / "finupdate/settings.json"


def _write_settings(**overrides) -> None:
    """Pre-write settings.json before launching the app so dev_mode + scenario
    are in effect from process start. The app reads this file on construction.
    """
    path = _settings_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    current = {}
    if path.exists():
        try:
            current = json.loads(path.read_text())
        except json.JSONDecodeError:
            current = {}
    current.update(overrides)
    path.write_text(json.dumps(current, indent=2))


# ── Dev-mode scenario setup ───────────────────────────────────────────────

@step('Application "{app_id}" is in developer mode with scenario "{scenario}"')
def set_dev_scenario(context, app_id, scenario):
    """Set Settings::dev_mode = true and the simulation scenario before launch.

    The app reads ~/.var/app/<app-id>/config/finupdate/settings.json on
    startup; writing it here is the most reliable way to gate the simulator
    path without driving the Preferences dialog every scenario.
    """
    assert app_id == "finupdate", f"Unknown app_id: {app_id}"
    assert scenario in ("Success", "AlreadyUpToDate", "Failure"), \
        f"Unknown scenario: {scenario}"

    # The app must not be running when we touch settings; otherwise it may
    # overwrite our changes when closing. qecore tracks this via context.
    if context.finupdate.instance:
        context.execute_steps('* Close application "finupdate" via "shortcut"')

    _write_settings(dev_mode=True, sim_scenario=scenario)

    # Re-launch with new settings in effect.
    context.execute_steps('\n'.join([
        '* Start application "finupdate" via "command"',
        '* Wait until window "Finupdate" appears in "finupdate"',
    ]))


# ── Custom window detection ───────────────────────────────────────────────

@step('Wait until window "{name}" appears in "{app_id}"')
def wait_for_window(context, name, app_id):
    """Wait for the application window to appear by name, regardless of role.

    GTK4 windows may report as 'filler' or other roles, not 'frame'.
    """
    app = getattr(context, app_id)
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            # Look for any widget with this name, any role
            matches = app.instance.findChildren(
                lambda n: n.name == name
            )
            if matches:
                return
        except Exception:
            pass
        time.sleep(0.5)
    assert False, f"Window {name!r} did not appear in {app_id}"


# ── Friendlier wait-and-click ─────────────────────────────────────────────

@step('Activate "{name}" "{role}" in "{app_id}"')
def activate_widget(context, name, role, app_id):
    """Wait until the named widget appears, then click it.

    Useful for buttons that appear partway through an async flow (e.g. the
    "Install Updates" button shown only after the check completes).
    """
    context.execute_steps('\n'.join([
        f'* Wait until "{name}" "{role}" appears in "{app_id}"',
        f'* Left click "{name}" "{role}" in "{app_id}"',
    ]))


# ── Bounded waits ─────────────────────────────────────────────────────────

@step('Wait until "{text}" appears in "{app_id}" within {seconds:d} seconds')
def wait_for_text(context, text, app_id, seconds):
    """Poll the AT-SPI tree for any node whose name or description contains
    `text`. Replaces noisy `findChild` regex with a bounded retry loop.

    Useful for matching status-page strings ("Ready to install", "Update
    Complete", "Up to Date", "Update Failed") which may appear in any role.
    """
    app = getattr(context, app_id)
    deadline = time.time() + seconds
    needle = text.lower()
    while time.time() < deadline:
        try:
            matches = app.instance.findChildren(
                lambda n: needle in ((n.name or "") + (n.description or "")).lower()
            )
            if matches:
                return
        except Exception:
            pass  # AT-SPI tree may transiently not be queryable
        time.sleep(0.5)

    # Debug: dump what IS in the tree
    found_texts = []
    try:
        def collect(n):
            name = n.name or ""
            desc = n.description or ""
            if name or desc:
                found_texts.append(f"[{n.roleName}] name={name!r} desc={desc!r}")
        app.instance.findChildren(lambda n: (collect(n), False)[1])
    except Exception:
        pass
    found_summary = "\n      ".join(found_texts[:30]) if found_texts else "(nothing with name/desc found)"
    assert False, f"Timed out after {seconds}s waiting for {text!r} in {app_id}.\n    Visible text nodes:\n      {found_summary}"


# ── Real mode setup ───────────────────────────────────────────────────────

@step('Application "{app_id}" is in real mode (developer mode disabled)')
def disable_dev_mode(context, app_id):
    """Disable developer mode to test real update paths.

    The Devel build defaults to dev_mode=true for safety during development.
    This step disables it so we can test the real update flow with actual
    system checks (Flatpak, Homebrew, Distrobox, bootc).
    """
    assert app_id == "finupdate", f"Unknown app_id: {app_id}"

    # Close the app if running
    if context.finupdate.instance:
        context.execute_steps('* Close application "finupdate" via "shortcut"')

    _write_settings(dev_mode=False)

    # Re-launch with real mode in effect
    context.execute_steps('\n'.join([
        '* Start application "finupdate" via "command"',
        '* Wait until window "Finupdate" appears in "finupdate"',
    ]))


# ── Diagnostics ───────────────────────────────────────────────────────────

@step('Dump AT-SPI tree of "{app_id}" to artifact')
def dump_tree(context, app_id):
    """Useful when developing new selectors. Captures the full a11y tree."""
    app = getattr(context, app_id)
    try:
        tree = app.instance.dump()
    except AttributeError:
        # Fallback: build a simple textual tree.
        out = []
        def walk(node, depth=0):
            try:
                out.append("  " * depth + f"[{node.roleName}] {node.name!r}")
                for child in node.children:
                    walk(child, depth + 1)
            except Exception:
                pass
        walk(app.instance)
        tree = "\n".join(out)
    target = Path(os.environ.get("DOGTAIL_ARTIFACTS", "/tmp")) / f"{app_id}-tree.txt"
    target.write_text(tree)
    print(f"Tree dumped to {target}")
