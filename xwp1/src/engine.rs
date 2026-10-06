//! The instrument as a player drives it: one instance of the machine, or
//! several behind the note allocator (`poly`), each on its own thread, with
//! the system reverb and the line output's response applied to their sum.
//! `xwp1-rt` and the plugin share this; where the sound and the MIDI come
//! from and go to is theirs.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crate::front::{self, Front};
use crate::machine::{CpuKind, Machine, OutputStage, USER_MEMORY};
use crate::poly::{self, Alloc};
use crate::reverb::{Reverb, ReverbBank};
use crate::sound::{SystemFx, WaveLibrary, WaveMorph, SAMPLE_RATE};
use crate::image;
use crate::macro_lfo;
use crate::vary;

pub const BLOCK: usize = 64; // samples between looks at MIDI IN
pub const SOLO_SYNTH_BANK: u8 = 98;
pub const HEX_LAYER_BANK: u8 = 97;
pub const DRAWBAR_BANK: u8 = 96;

/// What `Engine::start` needs to know.
pub struct Config {
    pub image: PathBuf,
    pub syx: Option<PathBuf>,
    pub program: u8,
    pub bank: u8, // bank select MSB: Solo Synth, 97 = Hex Layer, 96 = Drawbar Organ
    pub cpu: CpuKind, // the interpreter, or Unicorn as the reference
    pub fast: bool,   // Unicorn only: count instructions per block
    pub dry: bool,    // no reverb model
    pub reverb: PathBuf,
    pub block: Option<usize>, // samples per round, a multiple of 64; None = 64, or 256 when polyphonic
    pub history: Vec<Vec<u8>>, // edits to replay once the tone is selected (a saved state)
    pub user: Option<PathBuf>, // file that keeps the instrument's user memory between runs; None: it starts empty and is forgotten
    /// A host that keeps the user memory itself (a plugin, in its project): what it has, if anything. The area as
    /// the firmware leaves it then comes back in `Engine::user_written`.
    pub host_user: Option<Option<Vec<u8>>>,
}

pub fn run_seconds(m: &mut Machine, seconds: f64) {
    if !m.run((seconds * SAMPLE_RATE).ceil() as usize) {
        panic!("CPU fault: {}", m.fault.clone().unwrap_or_default());
    }
    m.out.clear();
    m.rev_out.clear();
}

/// Send a MIDI message and give the firmware time to take it, as
/// emu/session.py does: wire time on the panel link plus a settle time.
pub fn send(m: &mut Machine, data: &[u8], settle: f64) {
    m.midi_in(data);
    run_seconds(m, data.len() as f64 * 2.0 / 3125.0 + 0.01 + settle);
}

/// Splits a MIDI byte stream into whole messages (running status kept).
#[derive(Default)]
pub struct Framer {
    pub buf: Vec<u8>,
    pub status: u8,
}

impl Framer {
    pub fn push(&mut self, b: u8, mut emit: impl FnMut(&[u8])) {
        if b >= 0xF8 {
            emit(&[b]);
        } else if b == 0xF7 {
            if self.buf.first() == Some(&0xF0) {
                self.buf.push(b);
                emit(&self.buf);
            }
            self.buf.clear();
        } else if b >= 0x80 {
            self.buf.clear();
            self.buf.push(b);
            self.status = if b < 0xF0 { b } else { 0 };
        } else {
            if self.buf.is_empty() {
                if self.status == 0 {
                    return;
                }
                self.buf.push(self.status);
            }
            self.buf.push(b);
            let need = match self.buf[0] & 0xF0 {
                0xC0 | 0xD0 => 2,
                0xF0 => match self.buf[0] { 0xF0 => usize::MAX, 0xF2 => 3, _ => 2 },
                _ => 3,
            };
            if self.buf.len() == need {
                emit(&self.buf);
                self.buf.clear();
            }
        }
    }
}

/// The firmware's MIDI OUT with the bytes of two messages sorted out. When it answers a SysEx request while it
/// also sends a note or a controller, the two go out a byte about: f0 44 16 .. b1 [02] 4a [00] 1c [00] .. is the
/// answer with "b1 4a 1c" inside it (FINDINGS). A status byte says whose it is; of the data bytes that follow,
/// while both messages are open, each takes the turn the other did not have. Messages come out whole.
#[derive(Default)]
pub struct Demix {
    sysex: Option<Vec<u8>>,
    channel: Vec<u8>, // the channel message being put together
    need: usize,
    status: u8,       // running status
    channel_last: bool, // the last byte was the channel message's
    pub mixed: u64,     // messages that began inside another
}

impl Demix {
    pub fn push(&mut self, b: u8, out: &mut Vec<u8>) {
        match b {
            0xF8..=0xFF => out.push(b),
            0xF0 => {
                self.mixed += !self.channel.is_empty() as u64;
                (self.sysex, self.channel_last) = (Some(vec![b]), false);
            }
            0xF7 => {
                if let Some(mut whole) = self.sysex.take() {
                    whole.push(b);
                    out.extend(whole);
                }
                self.channel_last = false;
            }
            0x80..=0xF6 => {
                self.mixed += self.sysex.is_some() as u64;
                self.channel = vec![b];
                self.need = match b & 0xF0 { 0xC0 | 0xD0 => 2, 0xF0 => if b == 0xF2 { 3 } else if b == 0xF6 { 1 } else { 2 }, _ => 3 };
                self.status = if b < 0xF0 { b } else { 0 };
                self.channel_last = true;
                self.finish(out);
            }
            _ => {
                let channels = match (self.sysex.is_some(), !self.channel.is_empty()) {
                    (true, true) => !self.channel_last,
                    (true, false) => false,
                    (false, _) => true,
                };
                if channels {
                    if self.channel.is_empty() {
                        if self.status == 0 {
                            return;
                        }
                        self.channel.push(self.status);
                        self.need = if matches!(self.status & 0xF0, 0xC0 | 0xD0) { 2 } else { 3 };
                    }
                    self.channel.push(b);
                    self.finish(out);
                } else if let Some(sysex) = &mut self.sysex {
                    sysex.push(b);
                }
                self.channel_last = channels;
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        if self.channel.len() >= self.need {
            out.append(&mut self.channel);
        }
    }
}

/// Key velocity byte for a MIDI velocity: the inverse of the instrument's
/// Normal touch curve, which turns the keyboard's 0..255 into 1..127 (read
/// from the emulated firmware's MIDI OUT; within 2 units of it).
pub fn key_velocity(velocity: u8) -> u32 {
    const CURVE: [(f32, f32); 5] = [(1.0, 0.0), (10.0, 41.0), (20.0, 60.0), (65.0, 128.0), (127.0, 255.0)];
    let v = velocity.max(1) as f32;
    let i = CURVE.windows(2).position(|w| v <= w[1].0).unwrap_or(3);
    let (a, b) = (CURVE[i], CURVE[i + 1]);
    (a.1 + (b.1 - a.1) * (v - a.0) / (b.0 - a.0)).round() as u32
}

/// What an instance's thread is asked to do.
pub enum Cmd {
    Midi(Vec<u8>),       // to MIDI IN at once
    Paced(Vec<u8>),      // to MIDI IN as fast as the link takes it (`poly::pace`)
    Key(u32, bool, u32), // the key matrix
    Run(usize, Vec<f32>), // this many blocks, with this much of the instrument input; answered with a `Done`
    Peek(Vec<(u32, usize)>), // memory to read once MIDI IN has nothing waiting; answered to the pages (`Done::pages`)
    Button(u8, bool),    // a panel button, pressed or released
    Front,               // the display and LEDs, now and after every change; answered to the pages (`Done::pages`)
    Poke(u32, Vec<u8>),  // bytes for the work RAM
    Dial(i8),            // the data dial, clicks
    Control(usize, u8),  // a slider or knob (`front::control_input`) and its position
    Vary(usize, [u8; vary::KNOBS]), // voice variation: which instance this is, and the amounts (`vary`)
    WaveMorph(Option<WaveMorph>),
    MacroApply(Arc<macro_lfo::Config>, f64),
    MacroRestore(Arc<macro_lfo::Config>),
}

/// The panel reads the tone out of the instance's memory instead of asking
/// the firmware for each value (417 requests at 32 a second). Its request,
/// which never reaches the firmware: F0 7D 58 50, then per range the address
/// (five 7-bit bytes, low first) and the length (two), F7. The answer, one
/// message per range (`Engine::pages`): F0 7D 58 50, the address, two nibbles per byte, F7.
pub const PEEK: [u8; 4] = [0xF0, 0x7D, 0x58, 0x50];
/// F0 7D 58 57, an address (five 7-bit bytes, low first), then two nibbles per byte, F7 from a page: bytes for the
/// instance's work RAM (`Machine::poke`: the step sequence being edited). Not answered.
pub const POKE: [u8; 4] = [0xF0, 0x7D, 0x58, 0x57];

fn peek_ranges(msg: &[u8]) -> Vec<(u32, usize)> {
    msg[4..msg.len() - 1].chunks_exact(7).map(|c| {
        let addr = c[..5].iter().rev().fold(0u64, |a, &b| a << 7 | b as u64) as u32;
        (addr, (c[5] as usize | (c[6] as usize) << 7).min(4096))
    }).collect()
}

fn peek_answer(m: &Machine, addr: u32, len: usize) -> Vec<u8> {
    let mut out = PEEK.to_vec();
    out.extend((0..5).map(|i| (addr >> (7 * i)) as u8 & 0x7F));
    out.extend(m.mem_read(addr, len).iter().flat_map(|b| [b >> 4, b & 15]));
    out.push(0xF7);
    out
}

/// One block from an instance: dry, flat output, its reverb send, and what its MIDI OUT sent.
pub struct Done {
    pub out: Vec<f32>,
    pub rev: Vec<f32>,
    pub midi: Vec<u8>,
    pub fx: SystemFx,
    pub fiq_lost: u64,
    pub user: Option<Vec<u8>>, // the user memory, after the firmware has written to it (`Setup::keep_user`)
    pub pages: Vec<Vec<u8>>,   // whole messages for the panel pages only: memory read, display, LEDs, control positions
    pub mixed: u64,            // messages the firmware sent inside another so far (`Demix`)
    pub leds: u128,            // the instance's LEDs (`Front`)
}

pub struct Setup {
    pub image: Vec<u8>,
    pub bank: u8,
    pub program: u8,
    pub syx: Vec<Vec<u8>>,
    pub cpu: CpuKind,
    pub fast: bool,
    pub poly: bool,
    pub glide_off: bool, // polyphonic without the tones' portamento (`GLIDE_OFF` after each tone)
    pub history: Vec<Vec<u8>>, // edits since the player started, for an instance added later
    pub keep_user: bool, // report the user memory when the firmware has written to it (the first instance)
    pub wave_library: Option<Arc<WaveLibrary>>,
}

/// Part 1's "MIDI Generator Out" (Patch parameter 0xca): off, the keys no longer sound the part, while its notes
/// still go to MIDI OUT and MIDI IN still plays it (FINDINGS).
fn generator_out(on: bool) -> Vec<u8> {
    vec![0xF0, 0x44, 0x16, 0x03, 0x7F, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x4A, 1, 0, 0, 0, 0, on as u8, 0xF7]
}

/// Portamento off (controller 0x41): the firmware clears the switch of all
/// six oscillators in the edit buffer. Every instance glides from its own
/// last note, which is not what a polyphonic tone should do, so this
/// follows each tone choice unless the panel's Glide switch is on. Only a
/// tone load brings the tone's own switches back (`Engine::reload`).
pub const GLIDE_OFF: [u8; 3] = [0xB0, 0x41, 0];

impl Setup {
    /// The messages that bring a booted instance to the player's state, each with the time to let it settle.
    pub fn messages(&self) -> Vec<(Vec<u8>, f64)> {
        let mut out = vec![(vec![0xB0, 0, self.bank, 0xB0, 0x20, 0, 0xC0, self.program], 1.0)];
        for msg in &self.syx {
            // category 0x13 (the tone's effect) reloads the DSP routine
            let dsp = msg.len() > 6 && msg[0] == 0xF0 && msg[6] == 0x13;
            out.push((msg.clone(), if dsp { 0.3 } else { 0.005 }));
        }
        if self.glide_off {
            out.push((GLIDE_OFF.to_vec(), 0.005));
        }
        for msg in &self.history {
            let slow = msg[0] == 0xC0 || (msg.len() > 6 && msg[0] == 0xF0 && msg[6] == 0x13);
            out.push((msg.clone(), if slow { 0.5 } else { 0.005 }));
            if self.glide_off && msg[0] == 0xC0 {
                out.push((GLIDE_OFF.to_vec(), 0.005)); // edits made after the tone was chosen follow, and win
            }
        }
        if self.poly {
            out.push((poly::BEND_SETUP.to_vec(), 0.0));
        }
        out
    }
}

/// An instance's thread: boot, select the tone, then blocks on request.
/// The engine is made here because it cannot move between threads.
pub fn instance(setup: Setup, cmds: mpsc::Receiver<Cmd>, done: mpsc::Sender<Done>) {
    let messages = setup.messages();
    let mut m = Machine::with_cpu(setup.image, setup.cpu).expect("machine");
    if let Some(library) = setup.wave_library { m.devices().sound.set_wave_library(library); }
    (m.reverb, m.output_stage) = (None, None); // both run once, on the sum (`Mix`)
    if setup.fast {
        m.set_block_stepping().expect("block hook");
    }
    run_seconds(&mut m, 7.0); // tasks start about 5.5 s after reset
    for (msg, settle) in messages {
        send(&mut m, &msg, settle);
    }
    run_seconds(&mut m, 0.2);
    // Program changes load tones but do not leave the instrument in Tone mode.
    // Start there explicitly, after the selected tone and saved edits are ready.
    m.button(0x15, true); // TONE
    run_seconds(&mut m, 0.06);
    m.button(0x15, false);
    run_seconds(&mut m, 0.2);
    let (mut sent_from, mut queue) = (m.devices().panel.sent.len(), VecDeque::new());
    let mut peeks = Vec::new();
    let mut varied = None::<vary::Vary>;
    let mut shifts = VecDeque::new(); // the variation's own messages: sent when nothing else waits, so a note never queues behind them
    let mut demix = Demix::default();
    // the display and LEDs: followed from reset, told to the pages once one has asked
    let (mut front, mut front_from, mut front_asked, mut front_full, mut lcd_wait) = (Front::default(), 0, false, false, 0);
    let (mut controls, mut controls_changed) = ([0u8; front::CONTROLS], false);
    let mut rx_lost = 0; // bytes lost while starting are reported with the first block
    let fx = m.devices().sound.fx;
    // what the firmware has programmed or erased in flash, and for how many blocks that has not moved
    let flash_writes = |m: &mut Machine| { let f = &m.devices().flash; f.programmed + f.erased.len() as u64 };
    let (mut user_saved, mut user_seen, mut user_still) = (if setup.keep_user { 0 } else { u64::MAX }, 0, 0);
    if done.send(Done { out: Vec::new(), rev: Vec::new(), midi: Vec::new(), fx, fiq_lost: 0, user: None, pages: Vec::new(), mixed: 0, leds: 0 }).is_err() {
        return;
    }
    for cmd in cmds {
        let cmd = match (cmd, &mut varied) {
            (Cmd::Midi(mut msg), Some(v)) => { v.incoming(&mut msg, &mut shifts); Cmd::Midi(msg) }
            (Cmd::Paced(mut msg), Some(v)) => { v.incoming(&mut msg, &mut shifts); Cmd::Paced(msg) }
            (cmd, _) => cmd,
        };
        if let Cmd::Midi(ref msg) | Cmd::Paced(ref msg) = cmd {
            let mut framer = Framer::default();
            for &byte in msg {
                framer.push(byte, |whole| {
                    if let [status, note, velocity] = whole {
                        if status & 0xF0 == 0x90 && *velocity > 0 {
                            m.devices().sound.set_morph_note(*note);
                        }
                    }
                });
            }
        }
        match cmd {
            Cmd::Midi(msg) if queue.is_empty() => m.midi_in(&msg),
            Cmd::Midi(msg) => queue.push_back(msg), // behind what is still waiting (a tone being loaded again)
            Cmd::Paced(msg) => poly::pace(&mut queue, msg),
            Cmd::Vary(voice, amounts) => varied.get_or_insert_with(|| vary::Vary::new(voice)).set(amounts),
            Cmd::WaveMorph(value) => m.devices().sound.set_wave_morph(value),
            Cmd::MacroApply(config, position) => config.apply(&mut m, position),
            Cmd::MacroRestore(config) => config.restore(&mut m),
            Cmd::Key(code, release, velocity) => {
                if !release { m.devices().sound.set_morph_note((code + 36) as u8); }
                m.key(code, release, velocity)
            }
            Cmd::Peek(ranges) => peeks.extend(ranges),
            Cmd::Button(code, down) => m.button(code, down),
            Cmd::Front => (front_asked, front_full) = (true, true),
            Cmd::Poke(addr, data) => { m.poke(addr, &data); }
            Cmd::Dial(clicks) => m.dial(clicks),
            Cmd::Control(control, position) => {
                m.control(control, position);
                (controls[control], controls_changed) = (position, true);
            }
            Cmd::Run(blocks, input) => {
                m.input.extend(input);
                for _ in 0..blocks {
                    // The link carries about 4.7 of its bytes in a block: keep one message ahead, no more.
                    while m.devices().panel.waiting() < 6 {
                        let Some(msg) = queue.pop_front().or_else(|| shifts.pop_front()) else { break };
                        m.midi_in(&msg);
                    }
                    if !m.run(BLOCK) {
                        panic!("CPU fault: {}", m.fault.clone().unwrap_or_default());
                    }
                }
                let lost = m.devices().panel.rx_lost;
                if lost != rx_lost {
                    // the firmware then reads a damaged message (bug-074)
                    eprintln!("panel link: {} byte{} lost", lost - rx_lost, if lost - rx_lost == 1 { "" } else { "s" });
                    rx_lost = lost;
                }
                if let Some(v) = &mut varied {
                    let idle = queue.is_empty() && shifts.is_empty() && m.devices().panel.waiting() == 0;
                    shifts.extend(v.look(&m, blocks, idle));
                }
                let (raw, next) = m.midi_out(sent_from);
                sent_from = next;
                let mut midi = Vec::with_capacity(raw.len());
                for b in raw {
                    demix.push(b, &mut midi);
                }
                // Answers to the pages travel beside the firmware's MIDI OUT, whole, never inside its byte stream: a
                // block can end in the middle of one of its messages (a controller's third byte), and anything put
                // there breaks it and the running status after it.
                let mut pages = Vec::new();
                if !peeks.is_empty() && queue.is_empty() && m.devices().panel.waiting() == 0 {
                    for (addr, len) in peeks.drain(..) {
                        pages.push(peek_answer(&m, addr, len));
                    }
                }
                let sent = &m.devices().panel.sent;
                front.feed(&sent[front_from..]);
                front_from = sent.len();
                lcd_wait += blocks;
                if front_asked {
                    if front.leds_changed || front_full {
                        pages.push(front.leds_message());
                        front.leds_changed = false;
                    }
                    // the firmware draws a screen in many small writes: at most one picture every 16 blocks (24 ms)
                    if (front.lcd_changed && lcd_wait >= 16) || front_full {
                        pages.push(front.lcd_message());
                        (front.lcd_changed, lcd_wait) = (false, 0);
                    }
                    // a page that moved a control is told as well: another page follows it
                    if controls_changed || front_full {
                        let mut msg = front::FRONT.to_vec();
                        msg.push(2);
                        msg.extend(controls);
                        msg.push(0xF7);
                        pages.push(msg);
                        controls_changed = false;
                    }
                    front_full = false;
                }
                // a Write is many flash operations: the area is saved once they have stopped for a second
                let mut user = None;
                if user_saved != u64::MAX {
                    let writes = flash_writes(&mut m);
                    user_still = if writes == user_seen { user_still + blocks } else { 0 };
                    user_seen = writes;
                    if writes != user_saved && user_still >= (SAMPLE_RATE as usize) / BLOCK {
                        user = Some(m.mem_read(USER_MEMORY.0, USER_MEMORY.1));
                        user_saved = writes;
                    }
                }
                let block = Done { out: std::mem::take(&mut m.out), rev: std::mem::take(&mut m.rev_out), midi,
                                   fx: m.devices().sound.fx, fiq_lost: m.fiq_lost, user, pages, mixed: demix.mixed, leds: front.leds };
                if done.send(block).is_err() {
                    return;
                }
            }
        }
    }
}

/// Keep the user memory: written beside the file first, so a crash never leaves half of it.
fn save_user(file: &std::path::Path, data: &[u8]) {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let part = file.with_extension("part");
    if let Err(e) = std::fs::write(&part, data).and_then(|_| std::fs::rename(&part, file)) {
        eprintln!("user memory: {} not written ({e})", file.display());
    }
}

/// The system reverb and the line output's response, as `Machine::run`
/// applies them, here on the sum of the instances.
pub struct Mix {
    pub reverb: Option<Reverb>,
    pub bank: Option<ReverbBank>,
    pub setting: Option<(u8, u8)>,
    pub stage: [OutputStage; 2],
}

impl Mix {
    pub fn new(a: &Config) -> Mix {
        let mut mix = Mix { reverb: Some(Reverb::new(SAMPLE_RATE)), bank: None, setting: None, stage: Default::default() };
        if a.dry {
            mix.reverb = None;
        } else if let Some(bank) = ReverbBank::open(&a.reverb) {
            mix.bank = Some(bank);
            eprintln!("reverb: measured responses in {}", a.reverb.display());
        } else if let Ok(data) = std::fs::read(&a.reverb) {
            let taps: Vec<f32> = data.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            mix.reverb = Some(Reverb::from_response(&taps));
            eprintln!("reverb: fixed measured response {}", a.reverb.display());
        } else {
            eprintln!("reverb: {} not found, using the fitted model", a.reverb.display());
        }
        mix
    }

    /// `out`: left, right per sample; `rev`: the reverb send per sample.
    pub fn run(&mut self, out: &mut [f32], rev: &[f32], fx: SystemFx) {
        if let (Some(bank), true) = (&self.bank, self.reverb.is_some()) {
            // The firmware changes type and time only with the level
            // ramped to 0, so the swap is silent.
            if self.setting != Some((fx.reverb_type, fx.reverb_time)) {
                if let Some(r) = bank.reverb(fx.reverb_type, fx.reverb_time) {
                    self.reverb = Some(r);
                }
                self.setting = Some((fx.reverb_type, fx.reverb_time));
            }
            if let Some(r) = &mut self.reverb {
                r.gain = fx.reverb_level as f32 / 32.0;
            }
        }
        for (frame, &send) in out.chunks_exact_mut(2).zip(rev) {
            let wet = self.reverb.as_mut().map_or([0.0; 2], |r| r.tick(send));
            frame[0] = self.stage[0].run(frame[0] + wet[0]);
            frame[1] = self.stage[1].run(frame[1] + wet[1]);
        }
    }
}

/// How the player is set from the panel: kept in ~/.config/xwp1/poly.txt.
#[derive(Clone, Copy, PartialEq)]
pub struct Poly {
    pub voices: usize, // instances; 1 = the instrument as it is, monophonic
    /// False: the instances are a note allocator's voices. True: all eight
    /// instances are independent parts with configurable MIDI receive channels.
    pub multitimbral: bool,
    pub channels: [u8; MAX_VOICES], // zero-based receive channel for each independent part
    pub mpe: bool,     // each channel is one note's (an MPE controller); off: every channel is the same keyboard
    pub bend: f32,     // semitones of a full bend: per note with MPE, of the wheel without
    pub glide: bool,   // keep the tones' portamento when polyphonic (off: `GLIDE_OFF`)
    pub vary: [u8; vary::KNOBS], // voice variation, 0..127: glide, filters, envelopes, levels (`vary`)
}

pub const MAX_VOICES: usize = 8;
pub const DEFAULT_CHANNELS: [u8; MAX_VOICES] = [0, 1, 2, 3, 4, 5, 6, 7];

impl Poly {
    pub fn file() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/xwp1/poly.txt")
    }

    pub fn load() -> Poly {
        let mut p = Poly { voices: 1, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: true, bend: 48.0, glide: false, vary: [0; vary::KNOBS] };
        for line in std::fs::read_to_string(Self::file()).unwrap_or_default().lines() {
            p.set(line);
        }
        p
    }

    /// "voices N", "mode poly|multi", "channel PART CHANNEL", "mpe 0|1", "bend SEMITONES", "glide 0|1" or a voice variation
    /// ("vglide", "vfilter", "venv", "vlevel" 0..127); anything else changes nothing.
    pub fn set(&mut self, text: &str) {
        let mut words = text.split_whitespace();
        let (name, value) = (words.next(), words.next());
        if name == Some("channel") {
            if let (Some(part), Some(channel), None) = (value.and_then(|s| s.parse::<usize>().ok()),
                words.next().and_then(|s| s.parse::<u8>().ok()), words.next()) {
                if (1..=MAX_VOICES).contains(&part) && (1..=16).contains(&channel) {
                    self.channels[part - 1] = channel - 1;
                }
            }
            return;
        }
        match (name, value) {
            (Some("mode"), Some("multi" | "multitimbral" | "8part")) => self.multitimbral = true,
            (Some("mode"), Some("poly" | "polyphonic")) => self.multitimbral = false,
            (Some(name), Some(value)) => match value.parse::<f32>().ok().filter(|v| v.is_finite()) {
                Some(v) => match name {
                    "voices" => self.voices = (v as usize).clamp(1, MAX_VOICES),
                    "mpe" => self.mpe = v != 0.0,
                    "bend" => self.bend = v.clamp(0.0, 96.0),
                    "glide" => self.glide = v != 0.0,
                    _ => if let Some(i) = vary::NAMES.iter().position(|n| *n == name) {
                        self.vary[i] = v.clamp(0.0, 127.0) as u8;
                    }
                },
                None => {}
            },
            _ => {}
        }
    }

    pub fn save(&self) {
        let file = Self::file();
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let vary: String = vary::NAMES.iter().zip(self.vary).map(|(name, v)| format!("{name} {v}\n")).collect();
        let mode = if self.multitimbral { "multitimbral" } else { "polyphonic" };
        let channels: String = self.channels.iter().enumerate().map(|(i, ch)| format!("channel {} {}\n", i + 1, ch + 1)).collect();
        let _ = std::fs::write(file, format!("voices {}\nmode {mode}\nmpe {}\nbend {}\nglide {}\n{vary}{channels}", self.voices, self.mpe as u8, self.bend, self.glide as u8));
    }

    /// The answer to a page's "b" command: voice settings, mode, then eight receive channels.
    pub fn answer(&self) -> String {
        let v = self.vary;
        let voices = self.instances();
        let mode = if self.multitimbral { "multi" } else { "poly" };
        format!("B {voices} {} {} {} {} {} {} {} {mode} {}", self.bend, self.mpe as u8, self.glide as u8, v[0], v[1], v[2], v[3], self.channels.iter().map(|ch| (ch + 1).to_string()).collect::<Vec<_>>().join(" "))
    }

    pub fn instances(self) -> usize {
        if self.multitimbral { MAX_VOICES } else { self.voices }
    }

    pub fn polyphonic(self) -> bool {
        !self.multitimbral && self.voices > 1
    }
}

pub type Link = (mpsc::Sender<Cmd>, mpsc::Receiver<Done>);

/// The instrument as the player sees it: one instance, or several behind
/// the note allocator. System settings are taken from the first.
pub struct Engine {
    pub voices: Vec<Link>,
    wave_morph: Option<WaveMorph>,
    macro_lfo: Option<Arc<macro_lfo::Config>>,
    macro_notes: Vec<Option<u64>>, // note start time for each emulator instance
    macro_held: Vec<Option<f64>>, // sampled position for Hold, retained while a note sounds
    macro_last: Vec<Option<f64>>, // last position written, to skip repeated square/random steps
    macro_tick: u64,
    pub starting: Vec<(Link, Vec<Vec<u8>>)>, // instances still booting, and the edits made since they began
    pub alloc: Option<Alloc>,
    pub poly: Poly,
    pub glide_off: bool, // the instances running have had `GLIDE_OFF` after their tone
    pub history: Vec<Vec<u8>>, // edits (tone choice, SysEx, controllers) to bring a new instance up to date
    pub template: Box<dyn Fn(bool, Vec<Vec<u8>>) -> Setup>,
    pub mix: Mix,
    pub out: Vec<f32>,  // the last block: left, right per sample
    pub rev: Vec<f32>,
    pub midi: Vec<u8>,  // what the first instance's MIDI OUT sent during it
    pub pages: Vec<Vec<u8>>, // messages for the panel pages, not MIDI OUT: the host hands each to `Panel::midi_out` and clears the list
    pub mixed: u64,     // messages the first instance's firmware sent inside another, sorted out by `Demix`
    pub leds: Vec<u128>, // every instance's LEDs after the last round
    pub fx: SystemFx,
    pub fiq_lost: u64,
    pub block: usize, // samples per `run`
    pub fixed_block: Option<usize>,
    pub user_file: Option<PathBuf>,
    pub user_written: Option<Vec<u8>>, // the user memory after the firmware wrote to it, for a host that keeps it (`Config::host_user`)
    host_user: bool,
    // Several voices with the instrument's own keyboard (key mode): the keys go to the first instance, whose zones,
    // arpeggio and phrases make the notes; its part 1 is cut off from them (`generator_out`) and what it sends on
    // channel 1 is shared out by the allocator, tone and Performance choices going to the other instances too.
    key_mode: bool,
    varied: [u8; vary::KNOBS], // the voice variation the instances were last told
    pub peeked: Vec<(usize, Vec<u8>)>, // memory read from an instance other than the first (`Cmd::Peek` sent to it: checks)
    varied_once: bool,
    // The other way with several voices (`each`): the allocator shares the keys out and every instance gets its note
    // as a key of its own keyboard, so each runs its own arpeggio on it. The instances are kept alike: the
    // ARPEGGIO and HOLD switches follow the first instance's LEDs, the tempo buttons go to all.
    each: bool,
    switch_due: u64,          // not before this may a switch of another instance be pressed again
    local_off: bool,          // part 1 of the first instance is cut off from its keys
    local_due: Option<u64>,   // when to cut it off again: a Performance just loaded brings the switch back
    clock: u64,               // samples run
    editor_part: usize,       // the multitimbral instance the web editor is attached to
    made: Framer,             // the first instance's MIDI OUT, framed
    made_bank: u8,
    user_lock: Option<std::fs::File>, // held while this player is the one that keeps the user memory
}

impl Engine {
    pub fn start(a: &Config, poly: Poly) -> Engine {
        let mut image = image::load(&a.image).unwrap_or_else(|e| panic!("{}: {e}", a.image.display()));
        let stored = match &a.host_user {
            Some(kept) => kept.clone(),
            None => a.user.as_ref().and_then(|file| std::fs::read(file).ok()),
        };
        if let Some(saved) = stored.filter(|d| d.len() == USER_MEMORY.1) {
            let at = (USER_MEMORY.0 - image::FLASH_BASE) as usize;
            image[at..at + USER_MEMORY.1].copy_from_slice(&saved);
        }
        let mut syx = Vec::new();
        if let Some(path) = &a.syx {
            let data = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let mut framer = Framer::default();
            for b in data {
                framer.push(b, |msg| syx.push(msg.to_vec()));
            }
            eprintln!("sending {} messages from {}", syx.len(), path.display());
        }
        // The file is rewritten whole after a WRITE, so only one player may keep it: a second one (the app beside
        // `xwp1-rt`, say) starts from what is stored and keeps nothing of its own.
        let user_lock = a.user.as_deref().and_then(|file| {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let lock = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(file.with_extension("lock")).ok()?;
            lock.try_lock().ok().map(|_| lock)
        });
        if let (Some(file), None) = (&a.user, &user_lock) {
            eprintln!("user memory: {} belongs to another player while it runs; what this one stores is not kept", file.display());
        }
        let (bank, program, cpu, fast) = (a.bank, a.program, a.cpu, a.fast);
        let wave_library = std::fs::read(crate::setup::generated_dir().join("wave_morph.json")).ok()
            .and_then(|bytes| serde_json::from_slice::<WaveLibrary>(&bytes).ok()).map(Arc::new);
        let template = Box::new(move |poly, history| Setup { image: image.clone(), bank, program, syx: syx.clone(), cpu, fast, poly, glide_off: false, history, keep_user: false, wave_library: wave_library.clone() });
        let instances = poly.instances();
        let mut engine = Engine { voices: Vec::new(), wave_morph: None, macro_lfo: None, macro_notes: vec![None; instances], macro_held: vec![None; instances], macro_last: vec![None; instances], macro_tick: 0, starting: Vec::new(), alloc: None, poly, glide_off: poly.polyphonic() && !poly.glide,
                                  history: a.history.clone(), template,
                                  mix: Mix::new(a), out: Vec::new(), rev: Vec::new(), midi: Vec::new(), pages: Vec::new(), mixed: 0, leds: Vec::new(),
                                  fx: SystemFx { reverb_type: 1, reverb_time: 11, reverb_level: 32, chorus_rate: 5, chorus_level: 0 },
                                  fiq_lost: 0, block: BLOCK, fixed_block: a.block, user_file: a.user.clone().filter(|_| user_lock.is_some()), user_lock,
                                  user_written: None, host_user: a.host_user.is_some(), key_mode: false, each: false, varied: [0; vary::KNOBS], varied_once: false, peeked: Vec::new(), switch_due: 0, local_off: false, local_due: None,
                                  clock: 0, editor_part: 0, made: Framer::default(), made_bank: 0 };
        let keep = engine.user_lock.is_some() || engine.host_user;
        engine.voices = (0..instances).map(|i| engine.spawn_instance(poly.polyphonic(), i == 0 && keep)).collect();
        for (_, done) in &engine.voices {
            done.recv().expect("an instance stopped while starting");
        }
        engine.alloc = poly.polyphonic().then(|| Alloc::new(instances, poly.bend));
        engine.shape();
        engine
    }

    pub fn spawn(&self, poly: bool) -> Link {
        self.spawn_instance(poly, false)
    }

    fn spawn_instance(&self, poly: bool, keep_user: bool) -> Link {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let mut setup = (self.template)(poly, self.history.clone());
        setup.glide_off = poly && !self.poly.glide;
        setup.keep_user = keep_user;
        std::thread::Builder::new().name("voice".into()).spawn(move || instance(setup, cmd_rx, done_tx)).expect("thread");
        if self.macro_lfo.as_ref().is_none_or(|c| c.wave.is_none()) {
            if let Some(morph) = self.wave_morph { let _ = cmd_tx.send(Cmd::WaveMorph(Some(morph))); }
        }
        (cmd_tx, done_rx)
    }

    pub fn set_wave_morph(&mut self, value: Option<WaveMorph>) {
        if self.wave_morph == value { return; }
        self.wave_morph = value;
        if self.macro_lfo.as_ref().is_some_and(|c| c.wave.is_some()) { return; }
        for (tx, _) in &self.voices { let _ = tx.send(Cmd::WaveMorph(value)); }
        for ((tx, _), _) in &self.starting { let _ = tx.send(Cmd::WaveMorph(value)); }
    }

    pub fn set_macro_lfo(&mut self, config: Option<macro_lfo::Config>) {
        let previous = self.macro_lfo.take();
        if let Some(old) = previous {
            for (tx, _) in &self.voices {
                let _ = tx.send(Cmd::MacroRestore(old.clone()));
                if old.wave.is_some() { let _ = tx.send(Cmd::WaveMorph(self.wave_morph)); }
            }
        }
        self.macro_lfo = config.filter(macro_lfo::Config::valid).map(Arc::new);
        self.macro_notes.resize(self.voices.len(), None);
        self.macro_held.resize(self.voices.len(), None);
        self.macro_last.resize(self.voices.len(), None);
        if let Some(c) = &self.macro_lfo {
            for (i, (tx, _)) in self.voices.iter().enumerate() {
                let position = self.macro_notes[i].map(|started| match c.mode.as_str() {
                    "hold" => self.macro_held[i].unwrap_or_else(|| c.position(self.clock)),
                    "trig" | "one" | "half" => c.position(self.clock.saturating_sub(started)),
                    _ => c.position(self.clock),
                });
                let _ = if let Some(position) = position { tx.send(Cmd::MacroApply(c.clone(), position)) }
                    else { tx.send(Cmd::MacroRestore(c.clone())) };
                self.macro_last[i] = position;
            }
        } else {
            self.macro_notes.fill(None);
            self.macro_held.fill(None);
            self.macro_last.fill(None);
        }
    }

    fn macro_event(&mut self, voice: usize, msg: &[u8]) {
        if voice >= self.voices.len() { return; }
        match msg {
            [status, _, velocity] if status & 0xF0 == 0x90 && *velocity > 0 => {
                self.macro_notes.resize(self.voices.len(), None);
                self.macro_held.resize(self.voices.len(), None);
                self.macro_last.resize(self.voices.len(), None);
                self.macro_notes[voice] = Some(self.clock);
                if let Some(c) = &self.macro_lfo {
                    let at = if matches!(c.mode.as_str(), "trig" | "one" | "half") { 0 } else { self.clock };
                    let position = c.position(at);
                    self.macro_held[voice] = Some(position);
                    self.macro_last[voice] = Some(position);
                    let _ = self.voices[voice].0.send(Cmd::MacroApply(c.clone(), position));
                }
            }
            [status, ..] if status & 0xF0 == 0x80 || (status & 0xF0 == 0x90 && msg.get(2) == Some(&0)) => {
                if voice < self.macro_notes.len() {
                    self.macro_notes[voice] = None;
                    self.macro_held[voice] = None;
                    self.macro_last[voice] = None;
                }
            }
            _ => {}
        }
    }

    /// Follow the panel's settings. More voices take some seconds to boot
    /// and join when they are ready; the sound goes on meanwhile.
    pub fn set(&mut self, poly: Poly) {
        let old = self.poly;
        let instances = poly.instances();
        if poly.glide != old.glide || poly.multitimbral != old.multitimbral {
            self.starting.clear(); // booted for the other setting
        }
        if poly.multitimbral != old.multitimbral {
            // In either direction, start every added instance from the first
            // one's history. The other parts are intentionally independent
            // while multitimbral, so they cannot become polyphonic voices.
            self.voices.truncate(1);
        }
        if poly.multitimbral && old.multitimbral {
            for part in 0..self.voices.len().min(MAX_VOICES) {
                if poly.channels[part] != old.channels[part] {
                    let _ = self.voices[part].0.send(Cmd::Midi(vec![0xB0, 120, 0]));
                }
            }
        }
        self.poly = poly;
        self.starting.truncate(instances.saturating_sub(self.voices.len()));
        self.voices.truncate(instances);
        self.macro_notes.resize(instances, None);
        self.macro_held.resize(instances, None);
        self.macro_last.resize(instances, None);
        while self.voices.len() + self.starting.len() < instances {
            let link = self.spawn(poly.polyphonic());
            self.starting.push((link, Vec::new()));
        }
        self.shape();
    }

    /// Make the allocator, the bend ranges and the round length fit the instances running.
    pub fn shape(&mut self) {
        let n = self.voices.len();
        let glide_off = self.poly.polyphonic() && !self.poly.glide;
        if glide_off != self.glide_off {
            self.glide_off = glide_off;
            if glide_off {
                self.cut_glide();
            } else {
                self.reload();
            }
        }
        let first = &self.voices[0].0;
        match (&mut self.alloc, self.poly.polyphonic()) {
            (Some(alloc), true) => alloc.resize(n),
            (None, true) => {
                let _ = first.send(Cmd::Midi(vec![0xB0, 123, 0]));
                let _ = first.send(Cmd::Midi(poly::BEND_SETUP.to_vec()));
                self.alloc = Some(Alloc::new(n, self.poly.bend));
            }
            (Some(_), false) => {
                // back to the instrument as it is: its own bend range (2 semitones) and nothing left sounding
                self.alloc = None;
                for msg in [vec![0xB0, 123, 0], vec![0xE0, 0, 0x40], vec![0xD0, 0], vec![0xB0, 0x65, 0, 0xB0, 0x64, 0, 0xB0, 0x06, 2, 0xB0, 0x65, 0x7F, 0xB0, 0x64, 0x7F]] {
                    let _ = first.send(Cmd::Paced(msg));
                }
            }
            (None, false) => {}
        }
        if let Some(alloc) = &mut self.alloc {
            (alloc.member_range, alloc.master_range) = if self.poly.mpe { (self.poly.bend, 2.0) } else { (self.poly.bend, self.poly.bend) };
        }
        self.block = self.fixed_block.unwrap_or(if n > 1 { 4 * BLOCK } else { BLOCK });
        // voice variation: every instance but the first, once any amount has been set (never told: untouched)
        if self.poly.polyphonic() && self.poly.vary != self.varied {
            self.varied_once = true;
        }
        if self.poly.polyphonic() && self.varied_once {
            self.varied = self.poly.vary;
            for (i, (cmds, _)) in self.voices.iter().enumerate().skip(1) {
                let _ = cmds.send(Cmd::Vary(i, self.varied));
            }
        }
        self.sync_local();
    }

    /// `GLIDE_OFF` for the instances running, then the portamento switches
    /// set by hand since the tone was chosen: the state a new instance gets
    /// (`Setup::messages`), without loading the tone again.
    fn cut_glide(&self) {
        let since = self.history.iter().rposition(|m| m[0] == 0xC0).map_or(0, |i| i + 1);
        // controller 0x41, or a Solo Synth oscillator's parameter 4 (category 9)
        let switch = |m: &&Vec<u8>| matches!(m[..], [0xB0, 0x41, _]) || (m.len() > 24 && m[..6] == [0xF0, 0x44, 0x16, 0x03, 0x7F, 1] && m[6] == 9 && m[18] == 4 && m[19] == 0);
        for (cmds, _) in &self.voices {
            let _ = cmds.send(Cmd::Paced(GLIDE_OFF.to_vec()));
            for msg in self.history[since..].iter().filter(switch) {
                let _ = cmds.send(Cmd::Paced(msg.clone()));
            }
        }
    }

    /// Load the tone again in every instance and repeat the edits: the only
    /// way back to the tone's own portamento switches. It takes as long as
    /// the edits take to send (a patch of 400 values: about 4 s).
    fn reload(&self) {
        let mut setup = (self.template)(self.poly.polyphonic(), self.history.clone());
        (setup.image, setup.glide_off) = (Vec::new(), self.glide_off);
        for (cmds, _) in &self.voices {
            let _ = cmds.send(Cmd::Paced(vec![0xB0, 123, 0]));
            for (msg, _) in setup.messages() {
                let _ = cmds.send(Cmd::Paced(msg));
            }
        }
    }

    /// The bend range as the controller last set it itself (RPN 0), if it differs from the panel's.
    pub fn bend_set_by_controller(&mut self) -> Option<f32> {
        let alloc = self.alloc.as_ref()?;
        let range = if self.poly.mpe { alloc.member_range } else { alloc.master_range };
        (range != self.poly.bend).then(|| {
            self.poly.bend = range;
            self.shape();
            range
        })
    }

    /// Keep what a later instance needs to reach the same state: only the last of each setting.
    pub fn remember(&mut self, msg: &[u8]) {
        let key = |m: &[u8]| -> Option<Vec<u8>> {
            match m {
                [0xF0, _, _, _, _, 0, ..] => None, // a request
                [0xF0, _, _, _, _, 1, ..] if m.len() > 24 => Some(m[..24].to_vec()), // a parameter: up to its address
                [0xF0, ..] => Some(m.to_vec()),
                [0xC0, _] => Some(vec![0xC0]),
                [0xB0, cc, _] if matches!(cc, 6 | 38 | 96..=101) => Some(Vec::new()), // data entry: order is everything
                [0xB0, cc, _] if !matches!(cc, 1 | 64 | 120..=127) => Some(vec![0xB0, *cc]),
                _ => None,
            }
        };
        let Some(k) = key(msg) else { return };
        if !k.is_empty() {
            self.history.retain(|old| key(old).as_ref() != Some(&k));
        }
        self.history.push(msg.to_vec());
        for (_, missed) in &mut self.starting {
            missed.push(msg.to_vec());
            if self.poly.polyphonic() && !self.poly.glide && msg[0] == 0xC0 {
                missed.push(GLIDE_OFF.to_vec());
            }
        }
    }

    /// Route channel messages to every part assigned that channel. System
    /// real-time messages reach every part; panel SysEx reaches the editor part.
    fn play_multitimbral(&mut self, mut msg: Vec<u8>) {
        let Some(&status) = msg.first() else { return };
        if status >= 0xF8 {
            for (cmds, _) in &self.voices {
                let _ = cmds.send(Cmd::Midi(msg.clone()));
            }
        } else if status < 0xF0 {
            let channel = status & 0x0F;
            msg[0] &= 0xF0;
            for part in 0..self.voices.len().min(MAX_VOICES) {
                if self.poly.channels[part] != channel { continue; }
                if part == 0 { self.remember(&msg); }
                self.macro_event(part, &msg);
                let _ = self.voices[part].0.send(Cmd::Midi(msg.clone()));
            }
        } else {
            let part = self.editor_part();
            if part == 0 { self.remember(&msg); }
            self.macro_event(part, &msg);
            if let Some((cmds, _)) = self.voices.get(part) {
                let _ = cmds.send(Cmd::Midi(msg));
            }
        }
    }

    /// The part the editor talks to. It is deliberately a panel-only setting:
    /// Receive channels are configured separately for each part.
    fn editor_part(&self) -> usize {
        if self.poly.multitimbral { self.editor_part.min(self.voices.len().saturating_sub(1)) } else { 0 }
    }

    fn panel_voices(&self) -> &[Link] {
        if self.poly.multitimbral {
            let part = self.editor_part();
            &self.voices[part..=part]
        } else {
            &self.voices
        }
    }

    /// Incoming MIDI, one or several messages. Polyphonic: the allocator
    /// shares it out. Otherwise, with `keys`, notes on channel 1 within the
    /// 61 keys (C2..C7) go to the key matrix (code = note - 36, flag =
    /// release), so the instrument's zones, arpeggio and phrases apply;
    /// everything else is MIDI IN.
    pub fn play(&mut self, msg: &[u8], keys: bool) {
        // a page sends several messages in one piece (bank select and program change)
        let (mut framer, mut whole) = (Framer::default(), Vec::new());
        for &b in msg {
            framer.push(b, |m| whole.push(m.to_vec()));
        }
        for mut msg in whole {
            // The panel sends this private message before it reads a part.
            // Keeping it in the MIDI stream makes the choice ordered with all
            // of the parameter requests and writes around it.
            if let [0xF0, 0x7D, 0x58, 0x54, part, 0xF7] = msg[..] {
                self.editor_part = (part as usize).min(MAX_VOICES - 1);
                continue;
            }
            if msg.starts_with(&[0xF0, 0x7D, 0x58, 0x4D]) {
                if msg.last() == Some(&0xF7) {
                    if msg.len() == 5 { self.set_macro_lfo(None); }
                    else if let Ok(config) = serde_json::from_slice::<macro_lfo::Config>(&msg[4..msg.len() - 1]) {
                        if config.valid() { self.set_macro_lfo(Some(config)); }
                    }
                }
                continue;
            }
            if msg.starts_with(&PEEK) {
                let _ = self.voices[self.editor_part()].0.send(Cmd::Peek(peek_ranges(&msg)));
                continue;
            }
            if msg.starts_with(&POKE) {
                if msg.len() >= 12 && msg.len() % 2 == 0 {
                    let addr = msg[4..9].iter().rev().fold(0u64, |a, &b| a << 7 | b as u64) as u32;
                    let data: Vec<u8> = msg[9..msg.len() - 1].chunks_exact(2).map(|n| n[0] << 4 | n[1] & 15).collect();
                    for (cmds, _) in self.panel_voices() {
                        let _ = cmds.send(Cmd::Poke(addr, data.clone()));
                    }
                }
                continue;
            }
            // the front panel is every instance's (buttons, dial, sliders and knobs, the step grid's writes): they all
            // stay in the same state; the display and LEDs shown are the first one's. Transport is the first one's
            // alone (START/STOP 0x09, the phrase's PLAY/STOP 0x21, REC 0x0A), or a sequence plays once per voice.
            if msg.starts_with(&front::BUTTON) {
                if msg.len() == 7 {
                    let transport = matches!(msg[4], 0x09 | 0x21 | 0x0A);
                    let targets = self.panel_voices();
                    for (cmds, _) in &targets[..if transport { 1 } else { targets.len() }] {
                        let _ = cmds.send(Cmd::Button(msg[4], msg[5] != 0));
                    }
                }
                continue;
            }
            if msg.starts_with(&front::FRONT) {
                let _ = self.panel_voices()[0].0.send(Cmd::Front);
                continue;
            }
            if msg.starts_with(&front::DIAL) {
                if msg.len() == 6 {
                    for (cmds, _) in self.panel_voices() {
                        let _ = cmds.send(Cmd::Dial(((msg[4] << 1) as i8) >> 1));
                    }
                }
                continue;
            }
            if msg.starts_with(&front::CONTROL) {
                if msg.len() == 7 && (msg[4] as usize) < front::CONTROLS {
                    for (cmds, _) in self.panel_voices() {
                        let _ = cmds.send(Cmd::Control(msg[4] as usize, msg[5]));
                    }
                }
                continue;
            }
            if self.poly.multitimbral {
                self.play_multitimbral(msg);
                continue;
            }
            self.remember(&msg);
            // The instrument's own keys (code = note - 36), with one voice or several (`key_mode`). One voice: channel 1,
            // the other channels being the other parts'. Several: every channel is the one keyboard (an MPE
            // controller's notes are on channels 2 and up), so its zones, arpeggio and phrases apply to all of it;
            // what a finger does to its own note (bend, pressure) has no note to go to then.
            let note = matches!(msg[0] & 0xF0, 0x80 | 0x90) && (msg[0] & 0x0F == 0 || self.alloc.is_some());
            if keys && note && msg.len() == 3 && (36..=96).contains(&msg[1]) && self.each && self.alloc.is_some() {
                // a keyboard per voice: the allocator's note is a key of that instance (its bend and pressure go on
                // as MIDI, which the part takes all the same)
                if !self.poly.mpe {
                    msg[0] &= 0xF0;
                }
                for (voice, out) in self.alloc.as_mut().unwrap().feed(&msg) {
                    self.macro_event(voice, &out);
                    let key = |n: u8| (36..=96).contains(&n).then(|| n as u32 - 36);
                    let cmd = match out[..] {
                        [0x90, n, v] if v > 0 && key(n).is_some() => Cmd::Key(key(n).unwrap(), false, key_velocity(v)),
                        [0x90 | 0x80, n, _] if key(n).is_some() => Cmd::Key(key(n).unwrap(), true, 0),
                        _ => Cmd::Paced(out),
                    };
                    let _ = self.voices[voice].0.send(cmd);
                }
            } else if keys && note && msg.len() == 3 && (36..=96).contains(&msg[1]) {
                let release = msg[0] & 0xF0 == 0x80 || msg[2] == 0;
                self.macro_event(0, &msg);
                let _ = self.voices[0].0.send(Cmd::Key(msg[1] as u32 - 36, release, if release { 0 } else { key_velocity(msg[2]) }));
            } else if let Some(alloc) = &mut self.alloc {
                if !self.poly.mpe && msg[0] < 0xF0 {
                    msg[0] &= 0xF0; // one keyboard, whatever its channels
                }
                for (voice, msg) in alloc.feed(&msg) {
                    self.macro_event(voice, &msg);
                    let _ = self.voices[voice].0.send(Cmd::Paced(msg));
                }
                if self.glide_off && msg[0] == 0xC0 {
                    for (cmds, _) in &self.voices {
                        let _ = cmds.send(Cmd::Paced(GLIDE_OFF.to_vec()));
                    }
                }
            } else {
                self.macro_event(0, &msg);
                let _ = self.voices[0].0.send(Cmd::Midi(msg));
            }
        }
    }

    /// Whether notes play the instrument's own keyboard, and with several voices whether each voice has its own
    /// (`each`: an arpeggio per voice) or all share the first one's (the host says so every round).
    pub fn key_mode(&mut self, on: bool, each: bool) {
        let on = on && !self.poly.multitimbral;
        let each = each && on;
        if (on, each) != (self.key_mode, self.each) {
            if let Some(alloc) = &mut self.alloc {
                // nothing left sounding under the old routing
                for (voice, msg) in alloc.feed(&[0xB0, 123, 0]) {
                    let _ = self.voices[voice].0.send(Cmd::Paced(msg));
                }
            }
            (self.key_mode, self.each) = (on, each);
            self.sync_local();
        }
    }

    fn sync_local(&mut self) {
        let want = self.key_mode && !self.each && self.alloc.is_some();
        if want != self.local_off {
            self.local_off = want;
            self.local_due = None;
            let first = &self.voices[0].0;
            let _ = first.send(Cmd::Paced(vec![0xB0, 123, 0]));
            let _ = first.send(Cmd::Paced(generator_out(!want)));
        }
    }

    /// A message the first instance sent on MIDI OUT while its part 1 is cut off from the keys.
    fn generated(&mut self, msg: Vec<u8>) {
        if msg[0] >= 0xF0 || msg[0] & 0x0F != 0 {
            return; // the other zones' channels play inside the first instance
        }
        match msg[0] & 0xF0 {
            0x80 | 0x90 if self.local_off => {
                if let Some(alloc) = &mut self.alloc {
                    for (voice, msg) in alloc.feed(&msg) {
                        self.macro_event(voice, &msg);
                        let _ = self.voices[voice].0.send(Cmd::Paced(msg));
                    }
                }
            }
            0xB0 | 0xC0 => {
                // a knob or slider, or a tone or Performance chosen on the panel: the other instances follow
                if msg[0] == 0xB0 && msg[1] == 0 {
                    self.made_bank = msg[2];
                }
                self.remember(&msg);
                for (cmds, _) in &self.voices[1..] {
                    let _ = cmds.send(Cmd::Paced(msg.clone()));
                }
                if msg[0] == 0xC0 {
                    for (cmds, _) in &self.voices {
                        if self.glide_off {
                            let _ = cmds.send(Cmd::Paced(GLIDE_OFF.to_vec()));
                        }
                        let _ = cmds.send(Cmd::Paced(poly::BEND_SETUP.to_vec()));
                    }
                    if matches!(self.made_bank, 0x70 | 0x71) {
                        self.local_due = Some(self.clock + SAMPLE_RATE as u64);
                    }
                }
            }
            _ => {}
        }
    }

    /// Run every instance for `block` samples and mix them into `out`.
    /// Waking the threads costs a few tenths of a millisecond each time,
    /// which is why several instances are run in longer stretches.
    pub fn run(&mut self, input: &[f32]) {
        let before = self.voices.len();
        let mut i = 0;
        while i < self.starting.len() {
            match self.starting[i].0 .1.try_recv() {
                Ok(_) => {
                    let (link, missed) = self.starting.remove(i);
                    for msg in missed {
                        let _ = link.0.send(Cmd::Paced(msg));
                    }
                    self.voices.push(link);
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => drop(self.starting.remove(i)),
            }
        }
        if self.voices.len() != before {
            self.shape();
        }
        if let Some(c) = &self.macro_lfo {
            if c.mode != "hold" && self.clock.saturating_sub(self.macro_tick) >= (SAMPLE_RATE / 60.0) as u64 {
                for (i, started) in self.macro_notes.iter().enumerate() {
                    if let Some(started) = started {
                        if let Some((tx, _)) = self.voices.get(i) {
                            let at = if matches!(c.mode.as_str(), "trig" | "one" | "half") {
                                self.clock.saturating_sub(*started)
                            } else { self.clock };
                            let position = c.position(at);
                            if self.macro_last.get(i).copied().flatten().is_none_or(|last| (last - position).abs() >= 0.1) {
                                let _ = tx.send(Cmd::MacroApply(c.clone(), position));
                                self.macro_last[i] = Some(position);
                            }
                        }
                    }
                }
                self.macro_tick = self.clock;
            }
        }
        for (cmds, _) in &self.voices {
            let _ = cmds.send(Cmd::Run(self.block / BLOCK, input.to_vec()));
        }
        let selected_part = self.editor_part();
        let mut leds = Vec::with_capacity(self.voices.len());
        for (i, (_, done)) in self.voices.iter().enumerate() {
            let block = done.recv().unwrap_or_else(|_| panic!("instance {i} stopped"));
            if i == 0 {
                match (block.user, &self.user_file) {
                    (Some(data), _) if self.host_user => self.user_written = Some(data),
                    (Some(data), Some(file)) => {
                        // off the audio thread: it is a megabyte to disk
                        let file = file.clone();
                        std::thread::spawn(move || save_user(&file, &data));
                    }
                    _ => {}
                }
                leds.clear();
            }
            if i == selected_part {
                self.pages.extend(block.pages);
                self.mixed = block.mixed;
            } else if i != 0 {
                self.peeked.extend(block.pages.into_iter().map(|answer| (i, answer)));
            }
            leds.push(block.leds);
            if i == 0 {
                (self.out, self.rev, self.midi, self.fx, self.fiq_lost) = (block.out, block.rev, block.midi, block.fx, block.fiq_lost);
            } else {
                if self.poly.polyphonic() && self.varied[3] != 0 {
                    let gain = vary::gain(i, self.varied);
                    self.out.iter_mut().zip(&block.out).for_each(|(sum, x)| *sum += gain * x);
                    self.rev.iter_mut().zip(&block.rev).for_each(|(sum, x)| *sum += gain * x);
                } else {
                    self.out.iter_mut().zip(&block.out).for_each(|(sum, x)| *sum += x);
                    self.rev.iter_mut().zip(&block.rev).for_each(|(sum, x)| *sum += x);
                    self.fiq_lost += block.fiq_lost;
                }
                if i == selected_part {
                    self.midi = block.midi;
                    self.fx = block.fx;
                }
            }
        }
        self.clock += self.block as u64;
        self.leds = leds.clone();
        if !self.key_mode && (self.alloc.is_some() || (self.poly.multitimbral && selected_part == 0)) {
            // a tone chosen on the first instance's panel (the others chose it too: the buttons are theirs as well)
            // is kept for a voice that joins later. In multitimbral mode this
            // is just part 1, which is what the panel edits.
            let mut made = Vec::new();
            for &b in &self.midi {
                self.made.push(b, |m| made.push(m.to_vec()));
            }
            for msg in made {
                if matches!(msg[..], [0xC0, _] | [0xB0, 0 | 0x20, _]) {
                    self.remember(&msg);
                }
            }
        }
        if self.key_mode && self.alloc.is_some() {
            // what the first instance sends: its notes for the allocator when it is cut off from its keys, and in
            // either way of sharing the keys the tones, Performances and controllers chosen on its panel
            let mut made = Vec::new();
            for &b in &self.midi {
                self.made.push(b, |m| made.push(m.to_vec()));
            }
            for msg in made {
                self.generated(msg);
            }
            if self.local_off && self.local_due.is_some_and(|due| self.clock >= due) {
                self.local_due = None;
                let _ = self.voices[0].0.send(Cmd::Paced(generator_out(false)));
            }
            if self.each && self.clock >= self.switch_due {
                // ARPEGGIO (LED 34, button 0x28) and HOLD (LED 40, button 0x30) as on the first instance, and its mode
                // (PERFORM LED 37, TONE 6, STEP SEQ 5: one of them is lit), or the others keep their zones in Tone mode
                for (i, &other) in leds.iter().enumerate().skip(1) {
                    for (led, button, toggle) in [(34, 0x28, true), (40, 0x30, true), (37, 0x0E, false), (6, 0x15, false), (5, 0x04, false)] {
                        let differ = (other ^ leds[0]) >> led & 1 != 0;
                        if differ && (toggle || leds[0] >> led & 1 != 0) {
                            let _ = self.voices[i].0.send(Cmd::Button(button, true));
                            let _ = self.voices[i].0.send(Cmd::Button(button, false));
                            self.switch_due = self.clock + (0.8 * SAMPLE_RATE) as u64;
                        }
                    }
                }
            }
        }
        self.mix.run(&mut self.out, &self.rev, self.fx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multitimbral_mode_always_has_eight_independent_parts() {
        let mut p = Poly { voices: 3, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: true, bend: 48.0, glide: false, vary: [0; vary::KNOBS] };
        assert_eq!(p.instances(), 3);
        assert!(p.polyphonic());
        p.set("mode multi");
        assert_eq!(p.instances(), MAX_VOICES);
        assert!(!p.polyphonic());
        assert!(p.answer().contains(" multi 1 2 3 4 5 6 7 8"));
        p.set("channel 2 16");
        p.set("channel 3 16");
        assert_eq!(p.channels[1..3], [15, 15]);
        assert!(p.answer().contains(" multi 1 16 16 4 5 6 7 8"));
        p.set("channel 2 17");
        p.set("channel 9 1");
        assert_eq!(p.channels[1], 15);
        p.set("mode poly");
        assert_eq!(p.instances(), 3, "switching back keeps the selected polyphonic voice count");
        assert!(p.polyphonic());
    }

    #[test]
    #[ignore] // requires installed firmware; run explicitly for the per-voice path
    fn macro_hold_samples_each_voice_independently() {
        let config = Config { image: crate::setup::image_path(), syx: None, program: 0, bank: SOLO_SYNTH_BANK,
            cpu: CpuKind::Native, fast: true, dry: true, reverb: PathBuf::new(), block: None,
            history: Vec::new(), user: None, host_user: None };
        let mut e = Engine::start(&config, Poly { voices: 2, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: false, bend: 2.0, glide: false, vary: [0; vary::KNOBS] });
        let c = macro_lfo::Config { mode: "hold".into(), shape: "saw".into(), rate: 1.0,
            depth: 100.0, center: 50.0, points: (0..6).map(|i| [42, i, 0, 0, 127, 64, 0]).collect(), wave: None };
        let mut message = vec![0xF0, 0x7D, 0x58, 0x4D];
        message.extend(serde_json::to_vec(&c).unwrap());
        message.push(0xF7);
        e.play(&message, false);
        assert!(e.macro_lfo.is_some());
        e.play(&[0xF0, 0x7D, 0x58, 0x4C, 0xF7], false); // front-panel state request is a different command
        e.run(&[]);
        assert!(e.pages.iter().any(|m| m.starts_with(&front::FRONT)), "front-panel request must still work");
        e.play(&[0x90, 60, 100], false);
        let mut first_energy = 0.0;
        for _ in 0..(SAMPLE_RATE as usize / (2 * e.block)) {
            e.run(&[]);
            first_energy += e.out.iter().map(|x| x * x).sum::<f32>();
        }
        e.play(&[0x90, 64, 100], false);
        let mut second_energy = 0.0;
        for _ in 0..(SAMPLE_RATE as usize / (4 * e.block)) {
            e.run(&[]);
            second_energy += e.out.iter().map(|x| x * x).sum::<f32>();
        }
        let addr = 536796389;
        for (tx, _) in &e.voices { let _ = tx.send(Cmd::Peek(vec![(addr, 1)])); }
        for _ in 0..4 { e.run(&[]); }
        let first = e.pages.iter().find(|m| m.starts_with(&PEEK) && m.len() >= 12).unwrap();
        let second = e.peeked.iter().find(|(_, m)| m.starts_with(&PEEK) && m.len() >= 12).unwrap();
        let a = first[9] << 4 | first[10];
        let b = second.1[9] << 4 | second.1[10];
        eprintln!("macro test: {a} {b}, energy {first_energy} {second_energy}");
        assert!(a < 10 && (55..=75).contains(&b), "held macro values: {a}, {b}");
        assert!(second_energy > first_energy * 10.0, "second held voice should be much louder");
    }

    #[test]
    #[ignore]
    fn macro_free_changes_a_sounding_voice() {
        let config = Config { image: crate::setup::image_path(), syx: None, program: 0, bank: SOLO_SYNTH_BANK,
            cpu: CpuKind::Native, fast: true, dry: true, reverb: PathBuf::new(), block: None,
            history: Vec::new(), user: None, host_user: None };
        let mut e = Engine::start(&config, Poly { voices: 1, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: false, bend: 2.0, glide: false, vary: [0; vary::KNOBS] });
        let c = macro_lfo::Config { mode: "free".into(), shape: "square".into(), rate: 1.0,
            depth: 100.0, center: 50.0, points: (0..6).map(|i| [42, i, 0, 0, 127, 64, 0]).collect(), wave: None };
        e.set_macro_lfo(Some(c));
        e.play(&[0x90, 60, 100], false);
        let mut energies = [0.0f32; 4];
        for n in 0..(SAMPLE_RATE as usize / e.block) {
            e.run(&[]);
            energies[(n * e.block * 4 / SAMPLE_RATE as usize).min(3)] += e.out.iter().map(|x| x * x).sum::<f32>();
        }
        eprintln!("macro free energies: {energies:?}");
        assert!(energies[0] > energies[2] * 3.0);
    }

    #[test]
    fn mixed_bytes_are_sorted_out() {
        let run = |bytes: &[u8]| {
            let (mut d, mut out) = (Demix::default(), Vec::new());
            for &b in bytes {
                d.push(b, &mut out);
            }
            out
        };
        // as captured: a knob's controller on two channels inside an answer
        assert_eq!(run(&[0xF0, 0x44, 0x03, 0x1C, 0x7F, 0x01, 0xB1, 0x02, 0x4A, 0x00, 0x1C, 0x00, 0xB2, 0x00, 0x4A, 0x00, 0x1C, 0x05, 0xF7]),
                   [0xB1, 0x4A, 0x1C, 0xB2, 0x4A, 0x1C, 0xF0, 0x44, 0x03, 0x1C, 0x7F, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x05, 0xF7]);
        // an answer beginning inside a controller message
        assert_eq!(run(&[0xB0, 0x13, 0xF0, 0x25, 0x44, 0x16, 0xF7]), [0xB0, 0x13, 0x25, 0xF0, 0x44, 0x16, 0xF7]);
        // nothing mixed: as it came, running status and real-time bytes included
        assert_eq!(run(&[0x90, 60, 100, 62, 0, 0xF8, 0xF0, 1, 2, 0xF7, 0xC0, 5, 0xE0, 0, 64]),
                   [0x90, 60, 100, 0x90, 62, 0, 0xF8, 0xF0, 1, 2, 0xF7, 0xC0, 5, 0xE0, 0, 64]);
    }

    /// Needs the firmware: `cargo test --release --lib no_notes_hang -- --ignored` in xwp1/.
    /// Several voices, the instrument's own keys, and a page reading values all the while (a tab of the Perform
    /// view filling in): the answers' bytes get mixed with the notes' on the first instance's MIDI OUT, and no
    /// note made up of them may reach the allocator and stay.
    #[test]
    #[ignore]
    fn no_notes_hang_while_a_page_reads() {
        let config = Config { image: "../firmware/win/XW-P1 Updater/p1-update.bin".into(), syx: None, program: 0, bank: SOLO_SYNTH_BANK,
                              cpu: CpuKind::Native, fast: true, dry: true, reverb: PathBuf::new(), block: None, history: Vec::new(),
                              user: None, host_user: None };
        let mut e = Engine::start(&config, Poly { voices: 3, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: false, bend: 2.0, glide: false, vary: [0; vary::KNOBS] });
        e.key_mode(true, false);
        let request = |pid: u8, part: u8| vec![0xF0, 0x44, 0x16, 0x03, 0x7F, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, part, 0, pid, 0, 0, 0, 0, 0, 0xF7];
        let blocks = |seconds: f64, e: &Engine| (seconds * SAMPLE_RATE / e.block as f64) as usize;
        for _ in 0..blocks(0.5, &e) {
            e.run(&[]);
        }
        let (mut answers, mut framer, mut most, mut asked, mut odd) = (0, Framer::default(), 0, 0, Vec::new());
        for round in 0..blocks(6.0, &e) {
            // about 33 requests a second, as a page with three under way gets through; a key every eighth round
            if round % 5 == 0 {
                e.play(&request(0x68 + (round % 9) as u8, (round % 16) as u8), true);
                asked += 1;
            }
            if round % 8 == 0 {
                let note = [72, 76, 79][round / 8 % 3];
                e.play(&[if round / 8 % 6 < 3 { 0x90 } else { 0x80 }, note, 100], true);
            }
            if round == 10 {
                // the phrase and the step sequencer start, as when the fault was reported: the phrase's notes go
                // to MIDI OUT from another task than the answers, and that is where the bytes get mixed
                for code in [0x21, 0x09] {
                    for down in [1, 0] {
                        let mut msg = front::BUTTON.to_vec();
                        msg.extend([code, down, 0xF7]);
                        e.play(&msg, true);
                        e.run(&[]);
                    }
                }
            }
            e.run(&[]);
            for b in std::mem::take(&mut e.midi) {
                framer.push(b, |m| match m[0] {
                    // an answer is whole: 26 or 27 bytes with Casio's header; anything else on MIDI OUT here is a note
                    0xF0 if (m.len() == 26 || m.len() == 27) && m[1..6] == [0x44, 0x16, 0x03, 0x7F, 1] => answers += 1,
                    0x80..=0x9F if m[1] >= 36 && (m[0] & 0xF0 == 0x80 || m[2] == 0 || m[2] >= 20) => {}
                    _ => odd.push(m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")),
                });
            }
            most = most.max(e.alloc.as_ref().unwrap().held());
        }
        for note in [72, 76, 79] {
            e.play(&[0x80, note, 0], true);
        }
        for code in [0x21, 0x09] { // both stopped
            for down in [1, 0] {
                let mut msg = front::BUTTON.to_vec();
                msg.extend([code, down, 0xF7]);
                e.play(&msg, true);
                e.run(&[]);
            }
        }
        for _ in 0..blocks(2.0, &e) {
            e.run(&[]);
        }
        assert!(odd.is_empty(), "{} messages that are neither an answer nor a note, first {:?}", odd.len(), &odd[..odd.len().min(6)]);
        assert!(answers + 2 >= asked, "{answers} whole answers to {asked} requests");
        eprintln!("{answers} answers, {} messages inside another, at most {most} notes held", e.mixed);
        assert!(e.mixed > 0, "nothing was mixed: the test did not meet the fault");
        assert!((1..=3).contains(&most), "{most} notes held at once with three voices");
        assert_eq!(e.alloc.as_ref().unwrap().held(), 0, "notes left sounding after every key was released");
        // a controller or program change made up of mixed bytes would have gone to the other voices, and into this
        let made_up: Vec<String> = e.history.iter().map(|m| m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")).collect();
        assert!(made_up.is_empty(), "the other voices were sent {made_up:?}");
    }

    /// Needs the firmware: `cargo test --release --lib a_host_keeps -- --ignored` in xwp1/.
    /// A host that keeps the user memory itself (the plugin) gets the area after the first-boot
    /// format and after a WRITE, and an instrument started from it finds nothing to format.
    #[test]
    #[ignore]
    fn a_host_keeps_what_write_stores() {
        let config = |kept: Option<Vec<u8>>| Config {
            image: "../firmware/win/XW-P1 Updater/p1-update.bin".into(), syx: None, program: 0, bank: SOLO_SYNTH_BANK, cpu: CpuKind::Native,
            fast: true, dry: true, reverb: PathBuf::new(), block: None, history: Vec::new(), user: None, host_user: Some(kept) };
        let poly = Poly { voices: 1, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: false, bend: 2.0, glide: false, vary: [0; vary::KNOBS] };
        let run = |e: &mut Engine, seconds: f64| {
            for _ in 0..(seconds * SAMPLE_RATE / BLOCK as f64) as usize {
                e.run(&[]);
                e.pages.clear();
            }
        };
        let press = |e: &mut Engine, code: u8| {
            for down in [1, 0] {
                let mut msg = front::BUTTON.to_vec();
                msg.extend([code, down, 0xF7]);
                e.play(&msg, false);
                run(e, if down == 1 { 0.05 } else { 0.8 });
            }
        };
        let mut first = Engine::start(&config(None), poly);
        run(&mut first, 2.5);
        let formatted = first.user_written.take().expect("the formatted area was not reported");
        assert_eq!(formatted.len(), USER_MEMORY.1);
        assert!(formatted.iter().filter(|&&b| b != 0xFF).count() > 100_000, "the area was not formatted");
        for code in [0x0C, 0x1A, 0x33] { // WRITE, ENTER, YES: the Performance goes to user 0-0
            press(&mut first, code);
        }
        run(&mut first, 2.5);
        let written = first.user_written.take().expect("WRITE was not reported");
        let changed = formatted.iter().zip(&written).filter(|(a, b)| a != b).count();
        assert!((1..2000).contains(&changed), "{changed} bytes changed by one WRITE");
        drop(first);
        let mut second = Engine::start(&config(Some(written)), poly);
        run(&mut second, 3.0);
        assert!(second.user_written.is_none(), "an instrument started from the kept area wrote to it again");
    }

    /// Needs the firmware: `cargo test --release --lib keys_are_polyphonic -- --ignored` in xwp1/.
    /// Voice variation: the first instance keeps the tone's values, the others hold theirs shifted, whatever way
    /// the values got there (the setting itself, an edit, a tone chosen over MIDI or on the front panel, a voice
    /// that joined later), and come back when the amounts are 0.
    #[test]
    #[ignore]
    fn voices_vary_and_the_first_does_not() {
        let config = Config { image: "../firmware/win/XW-P1 Updater/p1-update.bin".into(), syx: None, program: 0, bank: SOLO_SYNTH_BANK,
                              cpu: CpuKind::Native, fast: true, dry: true, reverb: PathBuf::new(), block: None, history: Vec::new(),
                              user: None, host_user: None };
        let mut poly = Poly { voices: 3, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: false, bend: 2.0, glide: false, vary: [0; vary::KNOBS] };
        let mut e = Engine::start(&config, poly);
        let run = |e: &mut Engine, seconds: f64| {
            for _ in 0..(seconds * SAMPLE_RATE / e.block as f64) as usize {
                e.run(&[]);
                e.pages.clear();
            }
        };
        // Total Filter cutoff (parameter 73) and oscillator 2's amp envelope release (53): their memory cells
        let cells: serde_json::Value = serde_json::from_str(include_str!("../assets/mem.json")).unwrap();
        let cell = |pid: u64, inst: u64| cells["cells"].as_array().unwrap().iter().find(|r| r[0] == 9 && r[1] == pid && r[2] == inst).unwrap()[4].as_u64().unwrap() as u32;
        let (cutoff, release, glide) = (cell(73, 0), cell(53, 1), cell(5, 1));
        let read = |e: &mut Engine, addr: u32| -> Vec<u8> {
            let n = e.voices.len();
            let mut ask = PEEK.to_vec();
            ask.extend((0..5).map(|i| (addr >> (7 * i)) as u8 & 0x7F));
            ask.extend([1, 0, 0xF7]);
            e.play(&ask, false);
            for (cmds, _) in &e.voices[1..] {
                let _ = cmds.send(Cmd::Peek(vec![(addr, 1)]));
            }
            e.peeked.clear();
            let mut values = vec![None; n];
            for _ in 0..40 {
                e.run(&[]);
                for (i, answer) in e.peeked.drain(..).chain(e.pages.drain(..).filter(|p| p.starts_with(&PEEK) && p.len() == 12).map(|p| (0, p))) {
                    values[i] = Some(answer[9] << 4 | answer[10]);
                }
                if values.iter().all(Option::is_some) {
                    break;
                }
            }
            values.into_iter().map(|v| v.expect("an instance did not answer")).collect()
        };
        let shifted = |voice: usize, pid: u32, base: u8, span: f32| (base as f32 + vary::offset(voice, pid) * span).round().clamp(0.0, 127.0) as u8;
        let check = |e: &mut Engine, what: &str| -> (u8, u8) {
            let (c, r) = (read(e, cutoff), read(e, release));
            for i in 1..c.len() {
                assert_eq!(c[i], shifted(i, 73, c[0], 16.0), "{what}: cutoff of the voices {c:?}");
                assert_eq!(r[i], shifted(i, 53, r[0], 3.0 + 0.3 * r[0] as f32), "{what}: release of the voices {r:?}");
            }
            (c[0], r[0])
        };
        run(&mut e, 0.5);
        let (c, r) = (read(&mut e, cutoff), read(&mut e, release));
        assert!(c.iter().all(|v| *v == c[0]) && r.iter().all(|v| *v == r[0]), "before any variation: {c:?} {r:?}");
        let tone = (c[0], r[0]);

        // set while somebody plays (a note every 30 ms, the wheel moving): the shift must not wait for silence
        poly.vary = [127, 127, 127, 0];
        e.set(poly);
        for round in 0..(1.5 * SAMPLE_RATE / e.block as f64) as usize {
            if round % 5 == 0 {
                e.play(&[if round % 10 == 0 { 0x90 } else { 0x80 }, 60 + (round / 10 % 12) as u8, 100], false);
                e.play(&[0xB0, 1, (round % 128) as u8], false);
            }
            e.run(&[]);
            e.pages.clear();
        }
        assert_eq!(check(&mut e, "the setting, while notes are played"), tone);
        e.play(&[0xB0, 123, 0], false);
        // portamento time is bits 1..7 of its cell
        let g: Vec<u8> = read(&mut e, glide).iter().map(|b| b >> 1 & 127).collect();
        for i in 1..g.len() {
            assert_eq!(g[i], shifted(i, 5, g[0], 3.0 + 0.3 * g[0] as f32), "glide time of the voices {g:?}");
        }
        assert!((1..3).any(|i| shifted(i, 73, tone.0, 16.0) != tone.0), "no voice's cutoff moves at all");

        // an edit goes to every instance alike, and is shifted on its way in
        e.play(&[0xF0, 0x44, 0x16, 0x03, 0x7F, 1, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 73, 0, 0, 0, 0, 0, 80, 0xF7], false);
        run(&mut e, 0.3);
        assert_eq!(check(&mut e, "an edit").0, 80);

        // another tone over MIDI
        e.play(&[0xB0, 0, SOLO_SYNTH_BANK, 0xB0, 0x20, 0, 0xC0, 5], false);
        run(&mut e, 3.0);
        let five = check(&mut e, "a program change");
        // a tone chosen with the dial on the front panel
        let mut dial = front::DIAL.to_vec();
        dial.extend([1, 0xF7]);
        e.play(&dial, false);
        run(&mut e, 3.0);
        let six = check(&mut e, "the dial");
        assert!(five != tone || six != five, "the tones chosen hold the same two values: nothing was tested");

        // a voice that joins later: it is on the tone the dial chose, and shifted
        poly.voices = 4;
        e.set(poly);
        for _ in 0..4000 {
            if e.voices.len() == 4 {
                break;
            }
            e.run(&[]);
            e.pages.clear();
        }
        assert_eq!(e.voices.len(), 4, "the fourth voice did not join");
        run(&mut e, 3.0);
        check(&mut e, "a voice added");

        // amounts back to 0: every instance holds the tone's values again
        poly.vary = [0; vary::KNOBS];
        e.set(poly);
        run(&mut e, 1.5);
        let (c, r) = (read(&mut e, cutoff), read(&mut e, release));
        assert!(c.iter().all(|v| *v == c[0]) && r.iter().all(|v| *v == r[0]), "variation off: {c:?} {r:?}");
    }

    /// Key mode with several voices: three keys of zone 1 (the Solo Synth, one note at a time in an instance)
    /// sound three notes, also after a Performance was chosen on the panel and the first one chosen again.
    #[test]
    #[ignore]
    fn keys_are_polyphonic_with_several_voices() {
        let chord = |voices: usize, change: bool| {
            let config = Config { image: "../firmware/win/XW-P1 Updater/p1-update.bin".into(), syx: None, program: 0, bank: SOLO_SYNTH_BANK,
                                  cpu: CpuKind::Native, fast: true, dry: true, reverb: PathBuf::new(), block: None, history: Vec::new(),
                                  user: None, host_user: None };
            let mut e = Engine::start(&config, Poly { voices, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: false, bend: 2.0, glide: false, vary: [0; vary::KNOBS] });
            e.key_mode(true, false);
            let mut energy = 0.0f64;
            let mut run = |e: &mut Engine, seconds: f64, listen: bool| {
                for _ in 0..(seconds * SAMPLE_RATE / e.block as f64) as usize {
                    e.run(&[]);
                    if listen {
                        energy += e.out.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
                    }
                }
            };
            run(&mut e, 0.5, false);
            if change {
                for code in [0x26, 0x36] { // number keys 5 and 0
                    for down in [1, 0] {
                        let mut msg = front::BUTTON.to_vec();
                        msg.extend([code, down, 0xF7]);
                        e.play(&msg, true);
                        run(&mut e, if down == 1 { 0.1 } else { 2.0 }, false);
                    }
                }
            }
            for note in [72, 76, 79] {
                e.play(&[0x90, note, 100], true);
            }
            run(&mut e, 0.3, false);
            run(&mut e, 1.2, true);
            10.0 * energy.log10()
        };
        // an MPE controller: its notes are on channels 2 and up, and they are the instrument's keys all the same.
        // With the arpeggio on and zone 1 taking part in it, one held key makes a run of notes for the voices.
        let config = Config { image: "../firmware/win/XW-P1 Updater/p1-update.bin".into(), syx: None, program: 0, bank: SOLO_SYNTH_BANK,
                              cpu: CpuKind::Native, fast: true, dry: true, reverb: PathBuf::new(), block: None, history: Vec::new(),
                              user: None, host_user: None };
        let mut e = Engine::start(&config, Poly { voices: 3, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: true, bend: 48.0, glide: false, vary: [0; vary::KNOBS] });
        e.key_mode(true, false);
        let run = |e: &mut Engine, seconds: f64| -> usize {
            let (mut framer, mut notes) = (Framer::default(), 0);
            for _ in 0..(seconds * SAMPLE_RATE / e.block as f64) as usize {
                e.run(&[]);
                for b in std::mem::take(&mut e.midi) {
                    framer.push(b, |m| notes += (m[0] == 0x90 && m[2] > 0) as usize);
                }
            }
            notes
        };
        run(&mut e, 0.5);
        e.play(&[0x93, 72, 100], true);
        assert_eq!(run(&mut e, 0.4), 1, "a note on channel 4 did not play the instrument's key");
        e.play(&[0x83, 72, 0], true);
        // zone 1 takes part in the arpeggio, and the arpeggio's keys reach up to it (Performance 0: up to note 59)
        e.play(&[0xF0, 0x44, 0x16, 0x03, 0x7F, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x46, 1, 0, 0, 0, 0, 1, 0xF7], true);
        e.play(&[0xF0, 0x44, 0x16, 0x03, 0x7F, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x28, 1, 0, 0, 0, 0, 127, 0xF7], true);
        run(&mut e, 0.3);
        for down in [1, 0] {
            let mut msg = front::BUTTON.to_vec();
            msg.extend([0x28, down, 0xF7]);
            e.play(&msg, true);
            run(&mut e, 0.3);
        }
        e.play(&[0x92, 72, 100], true);
        let arpeggio = run(&mut e, 1.5);
        assert!(arpeggio >= 4, "{arpeggio} notes from one held key with the arpeggio on");
        drop(e);

        // A keyboard per voice: every voice runs its own arpeggio on the note it was given. The ARPEGGIO switch
        // pressed on the first instance is pressed on the others (their LED 34 follows).
        let mut e = Engine::start(&config, Poly { voices: 3, multitimbral: false, channels: DEFAULT_CHANNELS, mpe: false, bend: 2.0, glide: false, vary: [0; vary::KNOBS] });
        e.key_mode(true, true);
        run(&mut e, 0.5);
        e.play(&[0xF0, 0x44, 0x16, 0x03, 0x7F, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x46, 1, 0, 0, 0, 0, 1, 0xF7], true);
        e.play(&[0xF0, 0x44, 0x16, 0x03, 0x7F, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x28, 1, 0, 0, 0, 0, 127, 0xF7], true);
        for down in [1, 0] {
            let mut msg = front::BUTTON.to_vec();
            msg.extend([0x28, down, 0xF7]);
            e.play(&msg, true);
            run(&mut e, 0.3);
        }
        run(&mut e, 2.5);
        assert!(e.leds.len() == 3 && e.leds.iter().all(|l| l >> 34 & 1 == 1), "ARPEGGIO LEDs of the voices: {:?}", e.leds.iter().map(|l| l >> 34 & 1).collect::<Vec<_>>());
        let energy = |e: &mut Engine, keys: &[u8]| {
            for &k in keys {
                e.play(&[0x90, k, 100], true);
            }
            let (mut sum, mut framer, mut notes) = (0.0f64, Framer::default(), 0);
            for _ in 0..(1.5 * SAMPLE_RATE / e.block as f64) as usize {
                e.run(&[]);
                sum += e.out.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
                for b in std::mem::take(&mut e.midi) {
                    framer.push(b, |m| notes += (m[0] == 0x90 && m[2] > 0) as usize);
                }
            }
            for &k in keys {
                e.play(&[0x80, k, 0], true);
            }
            for _ in 0..(1.0 * SAMPLE_RATE / e.block as f64) as usize {
                e.run(&[]);
            }
            (10.0 * sum.log10(), notes)
        };
        let (single, run_of_one) = energy(&mut e, &[72]);
        assert!(run_of_one >= 4, "{run_of_one} notes from the first voice's own arpeggio on one key");
        let (chord_each, _) = energy(&mut e, &[72, 76, 79]);
        assert!(chord_each > single + 2.5, "three keys, an arpeggio each: {chord_each:.1} dB, one key {single:.1} dB");
        // TONE pressed on the first instance puts every voice in Tone mode (no zones under the other voices' keys),
        // and PERFORM brings them all back
        for (button, lit, dark) in [(0x15, 6, 37), (0x0E, 37, 6)] {
            for down in [1, 0] {
                let mut msg = front::BUTTON.to_vec();
                msg.extend([button, down, 0xF7]);
                e.play(&msg, true);
                run(&mut e, 0.3);
            }
            run(&mut e, 2.5);
            assert!(e.leds.iter().all(|l| l >> lit & 1 == 1 && l >> dark & 1 == 0), "mode LEDs {lit} / {dark} of the voices: {:?}", e.leds.iter().map(|l| (l >> lit & 1, l >> dark & 1)).collect::<Vec<_>>());
        }
        drop(e);

        let (one, three, after) = (chord(1, false), chord(3, false), chord(3, true));
        assert!(three > one + 3.0, "three voices {three:.1} dB, one voice {one:.1} dB");
        assert!((after - three).abs() < 2.0, "after a Performance change {after:.1} dB, before {three:.1} dB");
    }
}
