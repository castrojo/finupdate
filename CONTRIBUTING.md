# Contributing to Finupdate

Thank you for your interest in contributing to Finupdate and the Bluefin utility app ecosystem!

## Development Setup

### Prerequisites

You need **one** of these environments:

1. **Devcontainer** (recommended for contributors without GTK on host):
   - VS Code + Dev Containers extension
   - Podman or Docker
   - The repo includes a devcontainer config with all GTK/Rust deps

2. **Native** (Fedora/Bluefin):
   ```bash
   sudo dnf install gtk4-devel libadwaita-devel meson cargo rust
   ```

3. **Flatpak only** (build + test without any native Rust):
   ```bash
   flatpak install flathub org.gnome.Sdk//50 org.gnome.Platform//50
   flatpak install flathub org.freedesktop.Sdk.Extension.rust-stable//25.08
   flatpak install flathub org.flatpak.Builder
   ```

### Quick Build & Test Cycle

```bash
# Fast iteration (cargo):
cargo build && ./target/debug/finupdate

# Full integration test (Flatpak):
flatpak run org.flatpak.Builder --user --install --force-clean _flatpak \
  build-aux/org.projectbluefin.Finupdate.Devel.json
flatpak run org.projectbluefin.Finupdate.Devel
```

### Important: Flatpak + Container Conflict

If you use both a devcontainer AND Flatpak builds, the container's `target/` directory has different permissions. **Clean it before Flatpak builds:**

```bash
podman exec -w /workspaces/finupdate <container-id> rm -rf target
rm -rf _flatpak .flatpak-builder
```

## Architecture Overview

Read [PATTERNS.md](PATTERNS.md) for the full architecture. Key principles:

1. **One component per file** — each relm4 component lives in its own `.rs` file
2. **Messages over callbacks** — all state changes happen through the relm4 message system
3. **Separate worker thread** — async I/O (subprocess, network) runs on tokio in a background thread
4. **State machine at app level** — the `AppState` enum drives all UI transitions

## Code Style

### Rust
- Follow standard `rustfmt` formatting (default config)
- Use `tracing` macros (`tracing::info!`, `tracing::error!`) not `println!`
- All public items need doc comments (`///`)
- Module-level doc comments (`//!`) explain the *pattern*, not just what the code does
- Prefer explicit error messages over `.unwrap()` in production paths

### GTK/Adwaita
- **No hardcoded pixel values** for spacing — use CSS classes (`margin-12`, etc.) or Adwaita defaults
- **No custom colors** — rely on Adwaita style classes (`suggested-action`, `destructive-action`, `dim-label`)
- **Symbolic icons only** — always use `name-symbolic` suffix
- **AdwStatusPage for states** — idle, error, success, and empty states
- **AdwToast for transient feedback** — "copied to clipboard", "update complete"

### Accessibility
- Every icon-only button must have `set_tooltip_text` AND `set_accessible_label`
- Keyboard navigation must work for all interactive elements
- Test in both light and dark mode

## Making Changes

### Adding a New Feature

1. Identify which component owns the feature (app.rs, status_view.rs, or new component)
2. Add message variants to the appropriate `Input`/`Output` enum
3. Implement the handler in `update()`
4. Update the view! macro or init() if new widgets are needed
5. Update PATTERNS.md if the feature introduces a new pattern

### Adding a New Component

```bash
# Create the file:
touch src/ui/my_component.rs

# Add to src/ui/mod.rs:
pub mod my_component;
```

Then implement using the `#[relm4::component(pub)]` macro. See `log_view.rs` for the simplest example or `status_view.rs` for a complex one.

### Modifying the Flatpak Manifest

The manifest is at `build-aux/org.projectbluefin.Finupdate.Devel.json`. Key things:

- **New D-Bus permissions**: add to `finish-args` → `--talk-name=...`
- **New system access**: add to `finish-args` → `--filesystem=...`
- **SDK version changes**: update both `runtime-version` and `sdk-extensions` version

### Data Files

All data files use Meson templates (`.in` suffix) with placeholder substitution:
- `@APP_ID@` → the full application ID (includes `.Devel` suffix in dev builds)
- `@ICON@` → the icon name (matches APP_ID)

If you add a new data file, register it in `data/meson.build`.

## Commit Messages

Follow conventional commits:
```
feat: add feature description
fix: what was broken and how it's fixed
build: build system changes
docs: documentation only
refactor: code changes that don't add features or fix bugs
```

Include the Co-authored-by trailer for AI-assisted commits.

## Before Submitting a PR

Run through the HIG compliance checklist in [PATTERNS.md](PATTERNS.md#6-hig-compliance-checklist).

Quick sanity checks:
- [ ] `cargo build` compiles cleanly with no warnings
- [ ] `cargo clippy` passes (if available)
- [ ] App launches and the new feature works visually
- [ ] Dark mode looks correct
- [ ] Keyboard navigation works
- [ ] No hardcoded colors or pixel values

## File Map

| File | Purpose |
|------|---------|
| `Cargo.toml` | Rust dependencies and metadata |
| `meson.build` | Top-level Meson build (deps, subdirs) |
| `meson_options.txt` | Build profile option (development/release) |
| `src/main.rs` | Entry point (logging, app launch) |
| `src/config.rs` | Build-time constants |
| `src/config.rs.in` | Meson template for config.rs |
| `src/app.rs` | Main window component + state machine |
| `src/update_worker.rs` | Async subprocess worker (tokio) |
| `src/ui/mod.rs` | UI module declarations |
| `src/ui/status_view.rs` | State-driven content area (Stack) |
| `src/ui/log_view.rs` | Scrollable log output |
| `src/meson.build` | Cargo build integration |
| `data/meson.build` | Install desktop/metainfo/icons |
| `data/*.desktop.in` | Desktop entry template |
| `data/*.metainfo.xml.in` | AppStream metadata template |
| `data/icons/*.svg` | App icons (regular + symbolic) |
| `build-aux/*.json` | Flatpak manifests (Devel + release) |
| `build-aux/dist-vendor.sh` | Vendor cargo deps for `meson dist` |
| `PATTERNS.md` | Reusable patterns for Bluefin apps |
| `CONTRIBUTING.md` | This file |

## Getting Help

- Open an issue at https://github.com/castrojo/finupdate/issues
- Reference the [GNOME Developer Documentation](https://developer.gnome.org/)
- Reference the [relm4 book](https://relm4.org/book/stable/)
- Reference the [gtk4-rs docs](https://gtk-rs.org/gtk4-rs/stable/latest/docs/gtk4/)
