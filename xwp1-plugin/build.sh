#!/bin/sh
# Builds the plugin and makes its bundles in target/bundled/:
#   XW-P1 Emulator.vst3   (VST3)
#   XW-P1 Emulator.clap   (CLAP: the better choice where the host has it, MPE included)
#
#   ./build.sh             build and bundle
#   ./build.sh --install   also copy them to ~/.vst3 and ~/.clap
set -e
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
cargo build --release
so=target/release/libxwp1_plugin.so
out=target/bundled
mkdir -p "$out/XW-P1 Emulator.vst3/Contents/x86_64-linux"
cp "$so" "$out/XW-P1 Emulator.vst3/Contents/x86_64-linux/XW-P1 Emulator.so"
cp "$so" "$out/XW-P1 Emulator.clap"
echo "bundled: $here/$out"
if [ "$1" = --install ]; then
    mkdir -p "$HOME/.vst3" "$HOME/.clap"
    cp -r "$out/XW-P1 Emulator.vst3" "$HOME/.vst3/"
    cp "$out/XW-P1 Emulator.clap" "$HOME/.clap/"
    echo "installed: ~/.vst3/XW-P1 Emulator.vst3, ~/.clap/XW-P1 Emulator.clap"
fi
# A host with an older glibc must still load it (see src/compat.rs): nothing newer than 2.35, weak symbols aside.
new=$(objdump -T "$so" | grep -v ' w ' | grep -o 'GLIBC_2\.[0-9]*' | sort -uV | tail -1)
case "$new" in GLIBC_2.3[6-9]|GLIBC_2.[4-9]*) echo "warning: needs $new: it will not load in Bitwig's Flatpak (glibc 2.35)" ;; esac
