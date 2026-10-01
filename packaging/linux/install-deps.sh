#!/usr/bin/env sh
# Build dependencies for gpui on Debian/Ubuntu.
set -eu
SUDO=""
[ "$(id -u)" -ne 0 ] && SUDO="sudo"
$SUDO apt-get update
$SUDO apt-get install -y --no-install-recommends \
  build-essential pkg-config \
  libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libx11-dev libx11-xcb-dev libxcb1-dev \
  libfontconfig1-dev libfreetype6-dev \
  libvulkan-dev libvulkan1
