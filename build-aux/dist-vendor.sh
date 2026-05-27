#!/bin/bash
# dist-vendor.sh — packages Cargo dependencies into a dist tarball.
# Used by `meson dist` to produce self-contained source archives
# that can be built offline (required for Flatpak reproducible builds).

set -euo pipefail

export DIST="$1"
export SOURCE="$2"

cd "$SOURCE"
mkdir -p "$DIST"/.cargo
cargo vendor | sed 's/^directory = ".*"/directory = "vendor"/' > "$DIST"/.cargo/config.toml
cp -r vendor "$DIST"/vendor
