toolbox := "finupdate"
toolbox_image := "registry.fedoraproject.org/fedora-toolbox:43"
manifest := "build-aux/org.projectbluefin.Finupdate.Devel.json"
app_id := "org.projectbluefin.Finupdate.Devel"

# Show available recipes
default:
    @just --list

# Check the code compiles (fast, inside toolbox)
check:
    toolbox run --container {{ toolbox }} cargo check

# Build a debug binary (inside toolbox)
build:
    toolbox run --container {{ toolbox }} cargo build

# Run clippy lints (inside toolbox)
lint:
    toolbox run --container {{ toolbox }} cargo clippy -- -D warnings

# Run unit tests inside the toolbox
test:
    toolbox run --container {{ toolbox }} cargo test --all-targets

# Build and install the Flatpak (full integration build)
flatpak:
    flatpak run org.flatpak.Builder \
        --user --install --force-clean \
        _flatpak {{ manifest }}

# Run the installed Flatpak
run:
    flatpak run {{ app_id }}

# Refresh GNOME dock/launcher after a Flatpak install
dock:
    update-desktop-database ~/.local/share/applications 2>/dev/null || true
    gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor 2>/dev/null || true
    @echo "Dock launcher refreshed — you may need to re-pin the app"

# Build Flatpak, install it, then refresh the dock
flatpak-run: flatpak dock run

# Clean Flatpak build artifacts
clean-flatpak:
    rm -rf _flatpak .flatpak-builder

# Create the toolbox and install build + GUI-test deps (one-time setup).
# Uses fedora-toolbox (which ships dnf) rather than the sealed Bluefin Dakota
# image, which has no package manager.
setup:
    toolbox create -y --image {{ toolbox_image }} {{ toolbox }} || true
    toolbox run --container {{ toolbox }} sudo dnf install -y \
        cargo rust \
        gtk4-devel libadwaita-devel pango-devel cairo-devel openssl-devel \
        meson ninja-build pkg-config \
        python3-pip python3-dogtail python3-behave python3-pytest \
        gnome-ponytail-daemon python3-uinput

# Drop and recreate the toolbox from scratch
reset-toolbox:
    toolbox rm -f {{ toolbox }} || true
    just setup

# Run dogtail/behave GUI tests against the *currently installed* Flatpak,
# inside the current GNOME Wayland session. Requires:
#   - The Devel Flatpak is installed (`just flatpak` first).
#   - You're running an active GNOME session (or `qecore-headless` — see gui-test-headless).
#   - org.gnome.desktop.interface toolkit-accessibility is true.
gui-test suite="smoke" tags="":
    cd tests/{{ suite }} && behave features/ {{ if tags != "" { "--tags " + tags } else { "" } }}

# Run the GUI tests inside an isolated headless Wayland session via
# qecore-headless. This is what CI uses; DO NOT run on developer machines.
# Use `just gui-test` instead to test against your actual GNOME session.
_gui-test-headless suite="smoke" tags="":
    qecore-headless --session-type wayland --session-desktop gnome \
        "bash -lc 'cd tests/{{ suite }} && behave features/ {{ if tags != "" { "--tags " + tags } else { "" } }}'"

# Dump the current AT-SPI tree of a running finupdate to /tmp/finupdate-tree.txt
# Useful for writing new dogtail selectors. Run `just run` first, then this.
atspi-dump:
    python3 -c "from dogtail.tree import root; \
        import sys; \
        app = root.application('finupdate'); \
        def walk(n, d=0): \
            print('  '*d + f'[{n.roleName}] {n.name!r}'); \
            for c in n.children: walk(c, d+1); \
        walk(app)" > /tmp/finupdate-tree.txt
    @echo "AT-SPI tree dumped to /tmp/finupdate-tree.txt"
