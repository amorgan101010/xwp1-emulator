# XW-P1 Emulator 1.0.0

This is the first public source release for Linux. It includes the desktop
instrument and editor, plus CLAP and VST3 plugin source.

The emulator runs the XW-P1 1.11 firmware supplied by the user. On first launch,
choose the Windows or Mac 1.11 updater ZIP. The app extracts only the firmware
image, checks its hash, and builds editor data locally. The updater program is
never run. The release does not contain Casio firmware, updater files, factory
preset data, or generated wave samples.

The desktop app has Solo Synth, Hex Layer, Drawbar Organ, and PCM editing,
MIDI input, polyphonic playback, and measured system reverb responses. Optional
wave pictures and morphing data can be generated after setup with `xwp1 waves`.

Build and installation instructions are in [README.md](README.md). This source
release is licensed under GPL-3.0-or-later. The bundled font licenses are in
`xwp1/panel/fonts/`.

Supported platform: Linux with PipeWire, ALSA MIDI, GTK, and WebKitGTK. Only
the tested XW-P1 1.11 firmware image is accepted. The browser panel and Rust
core have automated tests; plugin host compatibility should be checked in each
DAW where the plugin is used.
