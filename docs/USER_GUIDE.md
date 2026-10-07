# XW-P1 Emulator user guide

XW-P1 Emulator is an independent project. It is not affiliated with, endorsed
by, or supported by Casio Computer Co., Ltd.

This guide covers the Linux desktop app and the CLAP and VST3 plugins. The
emulator needs a user-supplied **XW-P1 1.11** updater
ZIP. Firmware is not included. The packages it needs and how to install it
are in the [README](../README.md#requirements).

## Start the desktop app

1. In the source directory, run `tools/install_app.sh UPDATER.zip`, giving it
   the XW-P1 1.11 Windows or Mac updater ZIP you downloaded from Casio. It
   builds and installs the app, then imports the firmware, which takes about
   three minutes. An already extracted `p1-update.bin` is also accepted.
2. Open **XW-P1 Emulator** from the application menu.
3. The sound editor opens. Press a key on the on-screen keyboard to try it.

If you ran the script without the file, the app asks for the updater on its
first start and imports it then.

The emulator reads the firmware image from the ZIP and checks that it is the
supported version. It does not run the updater or communicate with a physical
XW-P1. You can check a completed setup from a terminal:

```sh
~/.local/bin/xwp1 check
```

If the file chooser is unavailable, import from a terminal with
`~/.local/bin/xwp1 setup '/path/to/updater.zip'`, then reopen the app.

## Hear and play it

The player sends audio to a PipeWire node named **xwp1**, which PipeWire
connects to your default output. If the top bar says **Output not cabled**,
connect the node in your patchbay, or click **Listen here** to hear it
through the app's window instead.
The volume slider in the top bar changes the emulator's output level; double
click it to return to 0 dB. The two small bars beside it show output level.

You can play with the on-screen keys, or type on the computer keyboard using
the `A W S E D F ...` key pattern. Press `Z` and `X` to shift the keyboard down
or up an octave. **Hold** keeps notes pressed; **Panic** sends all notes off.

For a MIDI keyboard, click **MIDI** in the top bar and switch on its input.
That choice is remembered for later launches. MIDI software can send to
**XW-P1 Emulator In**; the emulator exposes **XW-P1 Emulator Out** for messages
it sends back.

## Find a sound and edit it

The center of the top bar shows the current tone or Performance. Click its
name to choose another, or use the arrows on either side. Changing the sound
discards live edits to the previous tone: use **Save** in the top bar to store
it in a user slot first. The lists end with the user slots (U:0-0 and so on),
under the names you gave them.

| Tab | What it shows |
| --- | --- |
| **Solo** | Six sound sources, envelopes, filters, LFOs, effects, and macros. Click an oscillator block to open its detailed pitch, filter, and amp controls. |
| **Hex** | Six layers. Choose a layer to edit its wave, key range, and modulation. |
| **Organ** | Nine drawbars, percussion, vibrato, rotary controls, and effects. Pull a drawbar down to raise that footage. |
| **PCM** | PCM tone selection and the tone's available controls. |
| **Perform** | Performance, step sequence, and mixer pages, including zones, arpeggio, and phrases. |
| **Front Panel** | The instrument's display and controls, including the native Write dialog. |

For a knob, drag upward to increase its value or downward to decrease it.
Hold **Shift** while dragging for finer movement. The mouse wheel changes it
one step; when a knob has keyboard focus, arrow keys change it one step and
Page Up/Down change it ten. Double clicking recenters controls that have a
defined center. Switches and labeled buttons respond to a click. The panel
reads values from the emulator after a sound change; wait for the loading bar
to finish before editing.

### Solo macros and morphing

The **Macros** strip at the top of Solo moves several tone parameters with one
control. The tone keeps the resulting parameter values. Use **Store A** and
**Store B** to capture two states, then move **Morph** between them. The nearby
**Save** and **Load** controls keep a named morph and macro configuration in the
app's browser storage. These configurations are separate from the
instrument's user memory. **Undo** reverses the most recent macro edit.

The macro strip can be folded with its **Macros** heading. **Learn** lets a
MIDI controller operate a macro: select the macro, then move a controller.

### Voices and parts

Click **Mono**, **Poly**, or **Multi** in the top bar to open the instance
settings. **Polyphonic** allocates up to eight instances so one keyboard can
play chords; its settings include MPE, bend range, and glide. **8-part multi**
gives each part an independent tone and a selectable MIDI receive channel.
Choose the part shown in the editor from the same popover. The **Keys** switch
routes notes through the instrument's own keyboard behavior, including zones,
arpeggio, and phrases, when enabled.

## Save work and add optional waves

The emulator stores things the way the XW-P1 does: in its user memory with
WRITE, and as files on its SD card.

**Save** in the top bar stores the tone you are editing, or the Performance
when the Perform view is open. Type a name of up to twelve characters, pick a
user slot from the list and press **Save to U:…**. It presses the instrument's
own WRITE for you and reads the slot back; what was in the slot is replaced.
The instrument is on that user slot afterwards.

The row of buttons at the top of the dialog chooses something else to store:
the **DSP** of the tone (a Hex Layer, Drawbar or PCM tone whose effect line is
DSP; a Solo Synth tone keeps its effect in the tone), the **Sequence**,
**Chain**, **Phrase** or **Arpeggio** the instrument has loaded. The
instrument goes to that screen, stores, and comes back to where it was.

All of it can also be done on the **Front Panel** exactly as on the
instrument: go to the screen, press **Write**, choose the destination with
the number keys, **Enter**, **Yes**. The display there shows all four lines of
the instrument's.

The SD card is a file, `~/.config/xwp1/card.img`, made and formatted the first
time the player starts. On the Front Panel, **Menu** on any of those screens
has **Card Save**, **Card Load** and **Clear User**; **Setting > CardUtility**
has **All Data**, **SettingData**, **Format**, **Delete** and **Rename**. The
right half of the **Save** dialog lists the card's files: download one to this
computer, upload files to the card, or delete them. The files are the
instrument's own kinds (`.ZSY` Solo Synth tone, `.ZLT` Hex Layer, `.ZDO`
Drawbar Organ, `.ZTN` and `.ZDR` PCM tones, `.ZPF` Performance, `.ZSS`
sequence, `.ZSC` chain, `.ZAR` arpeggio, `.ZPH` phrase, `.DS7` DSP, `.ZAL` all
data, `.ZST` settings). They have not yet been exchanged with a real XW-P1.

The desktop player keeps the user memory in `~/.config/xwp1/user.bin`. The
plugin keeps it in the host's project; the card image is shared by the player
and every plugin instance. Copy `user.bin` and `card.img` to back them up, or
save **All Data** to the card and download the `.ZAL` file. Start the player
with `--no-card` for an empty slot, or `--card FILE` for another image (the
whole card as a file; one read from a real card has not been tried).

Wave pictures are optional. After setup, run `~/.local/bin/xwp1 waves` to
build them from your imported firmware. This also builds the wave index used
for Solo morphing between different samples. The command reports progress;
you can stop it with Ctrl-C and run it again to resume. `xwp1 waves 2` limits
the work to two workers. The measured reverb responses are already installed
with the desktop app.

## Use the plugin

Install with `tools/install_app.sh --plugin` (with the desktop app, which the
plugin opens as its editor), or build the plugin alone with
`xwp1-plugin/build.sh --install`. Either puts the CLAP and VST3 bundles in
`~/.clap` and `~/.vst3`. Rescan plugins in your host if needed. The
plugin uses the same imported firmware and generated editor files. If it opens
before setup, use its **Set up firmware** button, then reopen the plugin
instance. The plugin keeps its instrument state in the host project.

## If something is wrong

| Symptom | Check |
| --- | --- |
| Setup rejects an updater | The image must be the tested XW-P1 1.11 version. Choose the updater ZIP itself or its `p1-update.bin`; an updater program alone is not accepted. |
| Notes play but you hear nothing | Connect the **xwp1** PipeWire node to an output, or click **Listen here**. Check the top-bar volume and **Panic** if notes are stuck. |
| Your MIDI keyboard does nothing | Open **MIDI** and switch on the device. Check its connection and the chosen mode or part's receive channel. |
| The editor says it is reconnecting | Confirm the player is still running. Close and reopen the app if the player exited. |
| Wave pictures are absent | The editor works without them; run `xwp1 waves` after firmware setup if you want them. |
| A plugin asks for firmware | Finish setup in the desktop app or with `xwp1 setup`, then reopen the plugin instance. |

The project is licensed under GPL-3.0-or-later. The firmware remains the
user's responsibility and is never part of this release.
