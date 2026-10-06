#!/bin/sh
# Install relocatable binaries and static panel files. Firmware remains in the user's data root.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
prefix=${XWP1_PREFIX:-$data/xwp1-app}
apps=$data/applications
icons=$data/icons/hicolor/scalable/apps
links=${XWP1_BIN_HOME:-$HOME/.local/bin}

if [ "${1:-}" = --remove ]; then
    rm -f "$apps/xwp1.desktop" "$icons/xwp1.svg" "$links/xwp1" "$links/xwp1-app"
    rm -rf "$prefix"
    update-desktop-database "$apps" 2>/dev/null || true
    exit 0
fi

(cd "$root/xwp1" && cargo build --release --bin xwp1-rt)
(cd "$root/xwp1-app" && cargo build --release)
mkdir -p "$prefix/bin" "$prefix/share/xwp1/panel/fonts" "$apps" "$icons" "$links"
cp "$root/xwp1/target/release/xwp1-rt" "$prefix/bin/.xwp1-rt.new"
mv -f "$prefix/bin/.xwp1-rt.new" "$prefix/bin/xwp1-rt"
cp "$root/xwp1-app/target/release/xwp1-app" "$prefix/bin/.xwp1-app.new"
mv -f "$prefix/bin/.xwp1-app.new" "$prefix/bin/xwp1-app"
for pattern in '*.html' '*.js' '*.css' 'dsp.json'; do
    for file in "$root"/xwp1/panel/$pattern; do
        [ -f "$file" ] && cp "$file" "$prefix/share/xwp1/panel/"
    done
done
cp "$root"/xwp1/panel/fonts/* "$prefix/share/xwp1/panel/fonts/"
cp "$root/xwp1-app/assets/xwp1.svg" "$icons/xwp1.svg"
# the measured reverb responses (recordings of the hardware; they ship with the app): into the data root, where the player looks
if ls "$root"/data/reverb/*.f32 >/dev/null 2>&1; then
    mkdir -p "${XWP1_DATA_HOME:-$data/xwp1}/reverb"
    cp "$root"/data/reverb/*.f32 "${XWP1_DATA_HOME:-$data/xwp1}/reverb/"
fi
ln -sfn "$prefix/bin/xwp1-rt" "$links/xwp1"
ln -sfn "$prefix/bin/xwp1-app" "$links/xwp1-app"
cat > "$apps/xwp1.desktop" <<END
[Desktop Entry]
Type=Application
Name=XW-P1 Emulator
GenericName=Synthesizer
Comment=Casio XW-P1 emulator and sound editor
Exec=$prefix/bin/xwp1-app %u
Icon=xwp1
Terminal=false
Categories=AudioVideo;Audio;Midi;Music;
Keywords=synth;casio;xw-p1;midi;
StartupWMClass=xwp1-app
MimeType=x-scheme-handler/xwp1;
END
if [ "${XWP1_SKIP_DESKTOP_INTEGRATION:-0}" != 1 ]; then
    update-desktop-database "$apps" 2>/dev/null || true
    xdg-mime default xwp1.desktop x-scheme-handler/xwp1
    gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
fi
echo "installed: $prefix (run $links/xwp1 setup UPDATER.zip if needed)"
