#!/bin/sh
# Build and install the desktop app (relocatable binaries and static panel files). Firmware remains in the
# user's data root.
#
#   tools/install_app.sh [--plugin] [UPDATER.zip]   build and install; with a file, import the firmware too
#   tools/install_app.sh --check [--plugin]         only say what is missing on this system
#   tools/install_app.sh --remove                   remove the app and plugin (firmware and user data stay)
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
prefix=${XWP1_PREFIX:-$data/xwp1-app}
apps=$data/applications
icons=$data/icons/hicolor/scalable/apps
links=${XWP1_BIN_HOME:-$HOME/.local/bin}

plugin=0 check=0 remove=0 updater=
for arg in "$@"; do
    case "$arg" in
        --plugin) plugin=1 ;;
        --check) check=1 ;;
        --remove) remove=1 ;;
        -h|--help) sed -n '2,7s/^# \{0,1\}//p' "$0"; exit 0 ;;
        -*) echo "unknown option $arg (see --help)" >&2; exit 2 ;;
        *) updater=$arg ;;
    esac
done

if [ $remove = 1 ]; then
    rm -f "$apps/xwp1.desktop" "$icons/xwp1.svg" "$links/xwp1" "$links/xwp1-app"
    # only what this script put there: the web view keeps the editor's own storage (saved macro sets) in the same directory
    rm -rf "$prefix/bin" "$prefix/share" "$HOME/.vst3/XW-P1 Emulator.vst3" "$HOME/.clap/XW-P1 Emulator.clap"
    rmdir "$prefix" 2>/dev/null || true
    update-desktop-database "$apps" 2>/dev/null || true
    echo "removed. Still there: the firmware in ${XWP1_DATA_HOME:-$data/xwp1}, user memory and card image in $HOME/.config/xwp1, the editor's saved macro sets in $prefix"
    exit 0
fi
if [ -n "$updater" ] && [ ! -f "$updater" ]; then
    echo "no such file: $updater" >&2
    exit 2
fi

# What the build and the app need, checked before the long compile. Package lists: README.md, "Requirements".
missing= advice=
have() { command -v "$1" >/dev/null 2>&1; }
want() { missing="$missing
  $1"; }
have cargo || want "cargo: Rust 1.89 or newer (https://rustup.rs)"
have cc || want "cc: a C compiler and linker"
if have pkg-config; then
    modules="alsa dbus-1 gtk+-3.0 webkit2gtk-4.1"
    [ $plugin = 1 ] && modules="$modules x11 x11-xcb xcb gl"
    for module in $modules; do
        pkg-config --exists "$module" || want "$module: development files (pkg-config module)"
    done
else
    want "pkg-config"
fi
have pw-cat || want "pw-cat: PipeWire's command-line player (the emulator's audio output)"
if have cargo; then
    rust=$(cargo --version | sed 's/^cargo 1\.\([0-9]*\).*/\1/')
    case "$rust" in ''|*[!0-9]*) ;; *) [ "$rust" -ge 89 ] || want "cargo $(cargo --version | cut -d' ' -f2) is too old: Rust 1.89 or newer (https://rustup.rs)" ;; esac
fi
have zenity || advice="$advice
  zenity: without it there is no file chooser on first start (pass the updater ZIP to this script instead)"
have xdg-mime || advice="$advice
  xdg-utils: without it the plugin's Open editor button cannot open the app"
if [ -n "$missing" ]; then
    echo "missing:$missing" >&2
    [ -n "$advice" ] && echo "optional:$advice" >&2
    echo "The package names for Debian / Ubuntu, Fedora and Arch are in README.md under Requirements." >&2
    exit 1
fi
[ -n "$advice" ] && echo "optional, not found:$advice"
if [ $check = 1 ]; then
    echo "everything needed is here"
    exit 0
fi

(cd "$root/xwp1" && cargo build --release --locked --bin xwp1-rt)
(cd "$root/xwp1-app" && cargo build --release --locked)
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
    xdg-mime default xwp1.desktop x-scheme-handler/xwp1 2>/dev/null || true
    gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
fi
echo "installed: $prefix"

if [ $plugin = 1 ]; then
    "$root/xwp1-plugin/build.sh" --install
fi
if [ -n "$updater" ]; then
    echo "importing the firmware (about three minutes)..."
    "$prefix/bin/xwp1-rt" setup "$updater"
elif ! "$prefix/bin/xwp1-rt" check >/dev/null 2>&1; then
    echo "next: open XW-P1 Emulator and choose Casio's updater ZIP, or run: $links/xwp1 setup UPDATER.zip"
fi
case ":$PATH:" in *":$links:"*) ;; *) echo "note: $links is not in your PATH; the commands in the guide are $links/xwp1 ..." ;; esac
