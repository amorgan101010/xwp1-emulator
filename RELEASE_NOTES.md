# XW-P1 Emulator 0.9.0

A pre-release, for Linux, built from source. It replaces the 1.0.0 source
snapshot, which was withdrawn: that one could not save, and was harder to
install than it should have been.

## New since the withdrawn snapshot

- Saving, the way the XW-P1 does it. WRITE stores tones, Performances, DSPs,
  step sequences, chains, phrases and arpeggios in the instrument's user
  memory, which the app keeps between runs; the instrument's Card Save and
  Card Load work on an SD card image, in the instrument's own file kinds.
  The editor's **Save** dialog does both without the front panel, and can
  download and upload the card's files.
- One command installs: `tools/install_app.sh UPDATER.zip` checks what the
  build needs before it starts, builds, installs, and imports the firmware.
  `--plugin` adds the VST3 and CLAP plugin, `--check` only reports what is
  missing, `--remove` removes app and plugin.
- A shorter list of requirements, given per distribution in the README. The
  build no longer needs cmake or libclang (the reference CPU core they were
  for is now an optional feature for development).
- `xwp1 --help` and `xwp1 --version`; plain messages instead of crashes for a
  wrong option or a missing PipeWire tool.

## What it is

The emulator runs the XW-P1 1.11 firmware supplied by the user: Solo Synth,
Hex Layer, Drawbar Organ and PCM tones with an editor for each, polyphonic
Solo Synth, eight-part multi mode, Performances, the step sequencer, measured
system reverb responses, and the instrument's front panel. Casio's firmware,
updater, factory preset data and wave samples are not part of the release.

## Known limits

- Files saved here have not yet been exchanged with a real XW-P1.
- The app and every plugin instance share one SD card image, with nothing to
  stop two writing it at once: do Card Save from one at a time. Saving from
  inside the plugin has not been tested separately from the app.
- Arpeggios, chains and phrases can be stored but have no editor page yet;
  they are made on the Front Panel view with the instrument's own menus.
- The plugin is developed against Bitwig Studio; other hosts are untried.
- Only the firmware of updater 1.11 is accepted.

Installation is in [README.md](README.md), use in the
[user guide](docs/USER_GUIDE.md). GPL-3.0-or-later. This is an independent
project, unaffiliated with and not endorsed or supported by Casio Computer
Co., Ltd.
