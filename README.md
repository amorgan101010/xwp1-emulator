# XW-P1 Emulator

**1.0 is withdrawn.** Saving and recalling the instrument's user data is still
being verified and corrected. This repository is a development snapshot, not
a finished release.

A Linux desktop and plugin emulator for the Casio XW-P1. It runs the instrument's firmware against a model of its sound hardware. The firmware is not included.

## Install the desktop app

From this source tree, run `tools/install_app.sh`. This builds the Rust player and app and installs them in `~/.local/share/xwp1-app`, with launcher links in `~/.local/bin`. The application menu entry and `xwp1://` editor handler use the installed copies, so the source tree can be moved afterward.

Open **XW-P1 Emulator** and choose the XW-P1 1.11 Windows or Mac updater ZIP that you downloaded from Casio. An extracted `p1-update.bin` also works. The app reads only the image inside the ZIP; it never runs the updater or writes to a physical instrument. Setup can also be run from a terminal:

```sh
~/.local/bin/xwp1 setup '/path/to/XW-P1 Updater for Win-1_11-131218.zip'
~/.local/bin/xwp1 check
```

Only the tested XW-P1 1.11 image is supported. Setup stores a private copy under `${XDG_DATA_HOME:-~/.local/share}/xwp1/firmware/` and generates preset and tone names plus editor data under `xwp1/assets/`. It can take several minutes. Repeating setup with the same updater leaves an already valid install alone. The updater ZIP stays where you put it. The emulator's `user.bin` and plugin project state survive setup.

`XWP1_DATA_HOME` overrides the user data directory. `XWP1_PANEL_DIR` overrides static panel files for development. `--image`, `--panel-dir`, and `--reverb` remain available on `xwp1-rt` for development.

The editor works without the optional wave pictures. To build them locally, run `~/.local/bin/xwp1 waves` after setup. This also builds the wave register index used to blend oscillator samples when morphing between A and B snapshots. It reports progress and keeps completed waves as checkpoints; Ctrl-C stops it, and the same command resumes. Pass a worker count such as `xwp1 waves 2` to limit memory use. The installer includes measured reverb responses from the instrument. Firmware import does not generate those recordings.

The static editor tables in `xwp1/assets/` contain SysEx addresses, value types, ranges, labels, wave names, and reviewed address maps. The control definitions were compiled from the XW-P1 MIDI implementation document, franky's CTRLR panel, and checks on the instrument. Factory preset and PCM tone names are read from the user's firmware during setup and are not in these tables.

To remove the installed app, run `tools/install_app.sh --remove`. This leaves the imported firmware and user memory in place.

## Plugin

The VST3 and CLAP plugin can be built with `xwp1-plugin/build.sh`. It uses the same imported firmware and generated editor files as the desktop app. If it opens before setup, its editor has a **Set up firmware** button. Reopen the plugin instance after setup completes.

The plugin build needs Rust and the native audio and graphics development libraries. The VST3 binding used by NIH-plug is GPLv3; this project is GPL-3.0-or-later.

## Tests

Run `tools/test.sh` from any directory. It runs the firmware-independent Rust tests for the core and plugin, builds the app's test target, checks every panel script's syntax, and runs the panel tests with Node.js. The Rust integration tests start the real HTTP/WebSocket server and check asset delivery, rejected paths, MIDI routing, audio subscriptions, status, and reconnection. When Chromium is installed, a browser test loads the real panel against a small protocol fixture and checks startup, incoming status and volume, outgoing volume, and keyboard MIDI. The browser test reports a skip if Chromium is unavailable. These tests need Rust, Node.js, and the native libraries used to build the app and plugin; no firmware or audio device is required.

The core also has slower firmware-dependent development tests marked `#[ignore]`. They are outside the firmware-independent release suite and may need local test fixtures.

The desktop app requires GTK/WebKitGTK and the player uses PipeWire and ALSA MIDI. A Linux desktop with `zenity` provides the first-launch file chooser. For headless setup, use the CLI command above.

## Release source and license

The 1.0 source release is Linux only. It contains no firmware, updater, generated preset lists, wave samples, user data, or hardware recordings other than the measured reverb responses. Use `python3 tools/release.py --check` to verify the exact public file list, then `python3 tools/release.py` to create a separate public source tree and tarball under `dist/`. The archive has a SHA-256 sidecar; the tree includes hashes for every reviewed source file. This tree is meant to be committed to a public repository with its own history, separate from private development work.

The project code is licensed under GPL-3.0-or-later; see [LICENSE](LICENSE). Bundled fonts retain the OFL licenses in `xwp1/panel/fonts/`. The Casio XW-P1 firmware is not distributed and must be supplied by the user.
