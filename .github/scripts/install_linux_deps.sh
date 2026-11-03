#!/usr/bin/env bash
# Native dependencies for the Ubuntu 24.04 Linux validation job.
# gpui-pre-linux links xkbcommon/XCB; its font-kit backend requires
# Fontconfig/FreeType. native-tls uses OpenSSL. Headless GPUI tests do not
# need an X server, but fonts and the Mesa Vulkan driver support rendering.
set -euo pipefail

apt_command=(apt-get)
if [[ "${EUID}" -ne 0 ]]; then
  apt_command=(sudo apt-get)
fi
"${apt_command[@]}" update
"${apt_command[@]}" install -y --no-install-recommends \
  build-essential pkg-config libssl-dev \
  libfontconfig1-dev libfreetype6-dev fonts-dejavu-core \
  libxcb1-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libxcb-randr0-dev libxcb-xinput-dev \
  libwayland-dev libvulkan-dev mesa-vulkan-drivers libegl1-mesa-dev
