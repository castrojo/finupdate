toolbox := "finupdate"
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

# Build and install the Flatpak (full integration build)
flatpak:
    flatpak run org.flatpak.Builder \
        --user --install --force-clean \
        _flatpak {{ manifest }}

# Run the installed Flatpak
run:
    flatpak run {{ app_id }}

# Build Flatpak then run it
flatpak-run: flatpak run

# Clean Flatpak build artifacts
clean-flatpak:
    rm -rf _flatpak .flatpak-builder

# Create the toolbox and install build deps (one-time setup)
setup:
    toolbox create {{ toolbox }} || true
    toolbox run --container {{ toolbox }} sudo dnf install -y \
        cargo rust \
        gtk4-devel libadwaita-devel \
        meson ninja-build \
        pkg-config
