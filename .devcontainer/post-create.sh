#!/bin/bash
set -euo pipefail

# Install GTK4 and libadwaita development libraries needed for building
# GNOME/Adwaita apps with gtk4-rs and libadwaita-rs bindings.
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
  libgtk-4-dev \
  libadwaita-1-dev \
  pkg-config \
  build-essential

# Clean apt cache to keep image small
sudo rm -rf /var/lib/apt/lists/*

echo "✓ Dev dependencies installed. Ready to build finpilot."
