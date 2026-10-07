# XW-P1 Emulator

An emulator of the Casio XW-P1 synthesizer for Linux: a desktop app with a full sound editor, and a VST3 / CLAP plugin. It runs the instrument's own firmware against a model of its sound hardware, so the Solo Synth, Hex Layer, Drawbar Organ and PCM tones, the Performances, step sequencer, arpeggios and phrases are the instrument's, not imitations.

**The firmware is not included.** You supply Casio's XW-P1 1.11 updater (a free download from Casio's support site) and the emulator reads the firmware image out of it.

This is an independent project, unaffiliated with and not endorsed or supported by Casio Computer Co., Ltd.

## Status: 0.9, a pre-release

- Works: all four tone engines with their editors, polyphonic Solo Synth (up to eight voices, MPE), eight-part multi mode, Performances, the step sequencer, the measured system reverb, the instrument's front panel, and saving (WRITE to user slots, and the instrument's own file kinds on an SD card image).
- Not yet checked against a real XW-P1: files saved here have not been loaded on the instrument, nor the other way round.
- The desktop app and every plugin instance share one SD card image, and nothing yet stops two of them writing it at the same moment: do Card Save from one at a time. Saving from inside the plugin uses the same code as the app but has not been tested separately.
- Linux only, built from source. The plugin is developed against Bitwig Studio; other hosts are untried.
- Only the firmware of updater 1.11 is accepted.

## Requirements

A Linux desktop with PipeWire, and these packages. The build itself needs Rust 1.89 or newer; if your distribution's is older, install it from [rustup.rs](https://rustup.rs).

| | Debian / Ubuntu | Arch | Fedora |
| --- | --- | --- | --- |
| Build | `build-essential pkg-config libasound2-dev libdbus-1-dev libwebkit2gtk-4.1-dev`, and Rust from rustup (the packaged one is too old in 24.04) | `base-devel rust alsa-lib dbus webkit2gtk-4.1` | `cargo gcc pkgconf-pkg-config alsa-lib-devel dbus-devel webkit2gtk4.1-devel` |
| Run | `pipewire-bin zenity xdg-utils` | `pipewire-audio zenity xdg-utils` | `pipewire-utils zenity xdg-utils` |
| Plugin, in addition | `libx11-xcb-dev` | nothing | `libX11-devel` |

`tools/install_app.sh --check` (add `--plugin` if you want it) says what is missing without building anything. The lists were checked by building in bare Ubuntu 24.04 and Fedora containers; the app is developed and used on Arch.

## Install

```sh
tools/install_app.sh '/path/to/XW-P1 Updater for Win-1_11-131218.zip'
```

That is the whole installation: it checks the requirements, builds (three to five minutes the first time), installs the app for your user, and imports the firmware (about three minutes: it starts the instrument and reads every tone name from it). Then open **XW-P1 Emulator** from the application menu.

- Either of Casio's 1.11 updater ZIPs works (Windows or Mac), and so does an extracted `p1-update.bin`. Only the image inside is read; the updater program is never run and nothing is sent to a physical instrument. The ZIP stays where it is.
- Without the file argument the script only installs, and the app asks for the updater on its first start. `xwp1 setup UPDATER.zip` does the same from a terminal, and `xwp1 check` reports whether an import is complete.
- Add `--plugin` to build the VST3 and CLAP plugin too and copy it to `~/.vst3` and `~/.clap`. It uses the same imported firmware.
- Optional: `xwp1 waves` draws the picture of every wave for the editor and builds the index used when morphing between different samples (about four minutes; it can be interrupted and resumed, and `xwp1 waves 2` uses two workers instead of four). The editor works without them.

Where things go:

| | |
| --- | --- |
| `~/.local/share/xwp1-app/` | the app: binaries and editor pages; also the editor's own storage (saved macro and morph sets) |
| `~/.local/bin/xwp1`, `xwp1-app` | links to the player and the app |
| `~/.local/share/xwp1/` | your imported firmware, the data built from it, the reverb responses |
| `~/.config/xwp1/` | your user memory (`user.bin`), SD card image (`card.img`) and settings |

`tools/install_app.sh --remove` removes the binaries, editor pages, menu entry and plugin, and nothing you made or imported. Run the install script again after updating the source; an import that is already complete is left as it is.

How to play it, edit sounds and save them is in the [user guide](docs/USER_GUIDE.md).

## Development

`tools/test.sh` runs the tests that need neither firmware nor an audio device: the Rust tests of the core, app and plugin (including the real HTTP / WebSocket server), a syntax check of every editor script, and the editor's tests under Node.js (one of them in headless Chromium when it is installed, skipped otherwise). The core has slower tests marked `#[ignore]` that need an imported firmware.

- `xwp1/` is the emulator core and the player `xwp1-rt` (`xwp1 --help` lists its options), `xwp1-app/` the desktop window, `xwp1-plugin/` the plugin, `xwp1/panel/` the editor (plain JavaScript, no build step).
- To run from the source tree: `XWP1_PANEL_DIR=xwp1/panel xwp1/target/release/xwp1-rt`. `XWP1_DATA_HOME` moves the data directory; `--image`, `--panel-dir` and `--reverb` name single pieces.
- The CPU is an ARM7TDMI interpreter (`xwp1/src/arm.rs`). `cargo build --release --features unicorn` adds Unicorn as a reference core (`--cpu unicorn`) and builds `xwp1-lockstep`, which runs both side by side and compares them after every sample; that needs cmake and libclang, and nothing else uses it.
- The tables in `xwp1/assets/` are the instrument's control definitions (SysEx addresses, value types, ranges, labels, wave names), compiled from Casio's MIDI implementation document, franky's CTRLR panel, and checks on the instrument. Preset and tone names are not in them: they are read from your firmware during setup.
- `tools/release.py` exports the files listed in `release-manifest.txt` as a source release.

## License

GPL-3.0-or-later; see [LICENSE](LICENSE). (The VST3 binding used by NIH-plug is GPLv3.) The bundled fonts keep their OFL licenses in `xwp1/panel/fonts/`. The measured reverb responses in `data/reverb/` are recordings of the instrument made for this project. The Casio XW-P1 firmware is not distributed and must be supplied by the user.
