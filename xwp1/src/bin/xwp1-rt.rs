//! Real-time XW-P1 Solo Synth: boots the firmware, selects a Solo Synth
//! tone, then plays live. Audio goes to PipeWire through `pw-cat` (which
//! resamples from the chip's 42818 Hz); MIDI comes in on the ALSA
//! sequencer port "XW-P1 Emulator In", and what the firmware sends (SysEx
//! replies) leaves on "XW-P1 Emulator Out".
//!
//! With `--voices N` the Solo Synth is polyphonic: N instances of the
//! instrument, each on its own thread, play one note each (`xwp1::poly`
//! shares the notes out, MPE included), and the reverb and the output stage
//! run once on their sum. The panel can instead make the eight instances an
//! eight-part multitimbral instrument with configurable MIDI receive channels.
//!
//! usage: xwp1-rt [--image FILE] [--syx FILE] [--program N] [--hex | --drawbar] [--keys] [--node NAME]
//!                [--voices N] [--bend SEMITONES] [--block SAMPLES] [--script FILE --wav FILE]
//!                [--no-connect] [--latency MS] [--gain G] [--bench SECONDS] [--cpu native|unicorn] [--exact] [--dry]
//!                [--reverb DIR] [--no-user] [--card FILE | --no-card]
//!                [--remote PORT | --no-remote] [--panel-dir DIR]
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use midir::os::unix::{VirtualInput, VirtualOutput};
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput};
use xwp1::engine::{Config, Engine, Framer, Poly, BLOCK, DRAWBAR_BANK, HEX_LAYER_BANK, MAX_VOICES, SOLO_SYNTH_BANK};
use xwp1::machine::CpuKind;
use xwp1::sound::{SAMPLE_RATE, WaveLibrary, WaveMorph};


struct Args {
    image: PathBuf,
    syx: Option<PathBuf>,
    program: u8,
    no_user: bool, // do not keep the instrument's user memory in ~/.config/xwp1/user.bin
    card: Option<PathBuf>, // the SD card's image instead of ~/.config/xwp1/card.img
    no_card: bool, // nothing in the card slot
    keys: bool, // notes on channel 1 play the instrument's own keyboard (zones, arpeggio, phrases) instead of MIDI IN
    bank: u8, // bank select MSB: Solo Synth, 97 = Hex Layer (--hex), 96 = Drawbar Organ (--drawbar)
    node: String,
    connect: bool,
    latency_ms: u32,
    gain: f32,
    input_gain: f32,
    remote: Option<u16>,
    panel_dir: PathBuf,
    bench: Option<f64>,
    cpu: CpuKind, // the interpreter, or Unicorn as the reference
    fast: bool,   // Unicorn only: count instructions per block (--exact turns it off)
    dry: bool,
    reverb: PathBuf,
    voices: Option<usize>,  // instances of the instrument: above 1 it is polyphonic; None = as last set in the panel
    bend: Option<f32>,      // semitones of a full bend; None = as last set in the panel
    block: Option<usize>,   // samples per round of the main loop, a multiple of 64; None = 64, or 256 when polyphonic
    script: Option<PathBuf>, // play this file ("SECONDS HEX HEX ..." per line) instead of live MIDI,
    wav: Option<PathBuf>,    // into this file
}

fn args() -> Args {
    let mut a = Args { image: xwp1::setup::image_path(), syx: None, program: 0, keys: false, no_user: false, card: None, no_card: false, bank: SOLO_SYNTH_BANK,
                       node: "xwp1".into(), connect: true, latency_ms: 20, gain: 1.0, input_gain: 1.0, remote: Some(8800), panel_dir: xwp1::setup::panel_dir(), bench: None, cpu: CpuKind::Native, fast: true, dry: false,
                       reverb: xwp1::setup::data_root().join("reverb"), voices: None, bend: None, block: None, script: None, wav: None };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| { eprintln!("{flag} needs a value"); std::process::exit(2) });
        match flag.as_str() {
            "--image" => a.image = value().into(),
            "--syx" => a.syx = Some(value().into()),
            "--program" => a.program = value().parse().expect("--program"),
            "--keys" => a.keys = true,
            "--hex" => a.bank = HEX_LAYER_BANK,
            "--drawbar" => a.bank = DRAWBAR_BANK,
            "--node" => a.node = value(),
            "--no-connect" => a.connect = false,
            "--latency" => a.latency_ms = value().parse().expect("--latency"),
            "--gain" => a.gain = value().parse().expect("--gain"),
            "--input-gain" => a.input_gain = value().parse().expect("--input-gain"),
            "--remote" => a.remote = Some(value().parse().expect("--remote")),
            "--no-remote" => a.remote = None,
            "--panel-dir" => a.panel_dir = value().into(),
            "--cpu" => a.cpu = match value().as_str() {
                "native" => CpuKind::Native,
                "unicorn" => CpuKind::Unicorn,
                other => { eprintln!("--cpu {other}: native or unicorn"); std::process::exit(2) }
            },
            "--exact" => a.fast = false,
            "--dry" => a.dry = true,
            "--no-user" => a.no_user = true,
            "--card" => a.card = Some(value().into()),
            "--no-card" => a.no_card = true,
            "--reverb" => a.reverb = value().into(),
            "--voices" => a.voices = Some(value().parse().expect("--voices")),
            "--bend" => a.bend = Some(value().parse().expect("--bend")),
            "--block" => a.block = Some(value().parse::<usize>().expect("--block").div_ceil(BLOCK).max(1) * BLOCK),
            "--script" => a.script = Some(value().into()),
            "--wav" => a.wav = Some(value().into()),
            "--bench" => a.bench = Some(value().parse().expect("--bench")),
            _ => { eprintln!("unknown option {flag} (see xwp1 --help)"); std::process::exit(2) }
        }
    }
    if a.bank != SOLO_SYNTH_BANK {
        a.voices = Some(1); // the other tone types are polyphonic on their own
    }
    a
}


/// `--bench`: a note held on every instance, as fast as it goes.
fn bench(engine: &mut Engine, seconds: f64) {
    let count = engine.voices.len();
    for note in [48, 55, 60, 64, 67, 72, 76, 79].iter().cycle().take(count) {
        engine.play(&[0x90, *note, 100], false);
    }
    let blocks = (seconds * SAMPLE_RATE / engine.block as f64).ceil() as usize;
    let (mut worst, mut late, mut peak) = (Duration::ZERO, 0, 0.0f32);
    let real = Duration::from_secs_f64(engine.block as f64 / SAMPLE_RATE);
    let start = Instant::now();
    for _ in 0..blocks {
        let t = Instant::now();
        engine.run(&[]);
        worst = worst.max(t.elapsed());
        late += (t.elapsed() > real) as usize;
        peak = engine.out.iter().fold(peak, |p, x| p.max(x.abs()));
    }
    let wall = start.elapsed().as_secs_f64();
    assert!(peak > 0.0, "silent: the benchmark played nothing");
    eprintln!("{seconds} s with {count} note{} held in {wall:.2} s: {:.2}x real time; slowest block {:.2} ms of {:.2} ms, {late} of {blocks} late; peak {peak:.3}; FIQs lost {}",
              if count == 1 { "" } else { "s" }, seconds / wall, worst.as_secs_f64() * 1e3, real.as_secs_f64() * 1e3, engine.fiq_lost);
}

/// `--script FILE --wav FILE`: play timed MIDI ("SECONDS HEX HEX ..." per
/// line, # starts a comment; a line with a time alone sets the end) and
/// write the result as 32-bit float stereo at the chip's rate.
fn render(engine: &mut Engine, script: &std::path::Path, wav: &std::path::Path, gain: f32, keys: bool) {
    engine.key_mode(keys, false);
    let text = std::fs::read_to_string(script).unwrap_or_else(|e| panic!("{}: {e}", script.display()));
    let mut events: Vec<(f64, Vec<u8>)> = text.lines().map(|l| l.split('#').next().unwrap().trim()).filter(|l| !l.is_empty()).map(|l| {
        let mut words = l.split_whitespace();
        let time = words.next().unwrap().parse().unwrap_or_else(|_| panic!("bad time in {l:?}"));
        (time, words.map(|w| u8::from_str_radix(w, 16).unwrap_or_else(|_| panic!("bad byte in {l:?}"))).collect())
    }).collect();
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let end = events.last().map_or(0.0, |e| e.0);
    let (mut samples, mut next, mut produced) = (Vec::<f32>::new(), 0, 0usize);
    while (produced as f64) < end * SAMPLE_RATE {
        while next < events.len() && events[next].0 * SAMPLE_RATE <= produced as f64 {
            let mut framer = Framer::default();
            let mut messages = Vec::new();
            for &b in &events[next].1 {
                framer.push(b, |msg| messages.push(msg.to_vec()));
            }
            for msg in messages {
                engine.play(&msg, keys);
            }
            next += 1;
        }
        engine.run(&[]);
        samples.extend(engine.out.iter().map(|x| x * gain));
        produced += engine.block;
    }
    let bytes = 4 * samples.len() as u32;
    let rate = SAMPLE_RATE.round() as u32;
    let mut file = Vec::with_capacity(44 + bytes as usize);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + bytes).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    for field in [16u32, 3 | 2 << 16, rate, rate * 8, 8 | 32 << 16] {
        file.extend_from_slice(&field.to_le_bytes()); // float, 2 channels; 8-byte frames of 32-bit samples
    }
    file.extend_from_slice(b"data");
    file.extend_from_slice(&bytes.to_le_bytes());
    file.extend(samples.iter().flat_map(|x| x.to_le_bytes()));
    std::fs::write(wav, file).unwrap_or_else(|e| panic!("{}: {e}", wav.display()));
    let peak = samples.iter().fold(0.0f32, |p, x| p.max(x.abs()));
    eprintln!("{}: {:.2} s, peak {peak:.3}", wav.display(), samples.len() as f64 / 2.0 / SAMPLE_RATE);
}

/// MIDI controllers the player listens to by itself, chosen in the panel
/// (so nothing has to be cabled by hand) and remembered in
/// ~/.config/xwp1/controllers.txt. Their channel messages are put on
/// channel 1, the Solo Synth part's, unless the player is polyphonic: then
/// the channels are kept, as MPE needs them.
struct Controllers {
    channels: Arc<std::sync::atomic::AtomicBool>, // keep channels for polyphonic or multitimbral mode
    tx: mpsc::Sender<Vec<u8>>,
    open: HashMap<String, MidiInputConnection<()>>,
    wanted: Vec<String>,
    file: PathBuf,
}

impl Controllers {
    fn new(tx: mpsc::Sender<Vec<u8>>, channels: Arc<std::sync::atomic::AtomicBool>) -> Controllers {
        let file = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/xwp1/controllers.txt");
        let wanted = std::fs::read_to_string(&file).map(|t| t.lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
                                                   .unwrap_or_default();
        Controllers { channels, tx, open: HashMap::new(), wanted, file }
    }

    /// Readable ports by a name that survives replugging (ALSA's client and
    /// port numbers cut off), without this program's own and the system's.
    fn present() -> Vec<(String, midir::MidiInputPort, MidiInput)> {
        let Ok(probe) = MidiInput::new("XW-P1 Emulator scan") else { return Vec::new() };
        let mut found = Vec::new();
        for port in probe.ports() {
            let Ok(name) = probe.port_name(&port) else { continue };
            let name = name.rsplit_once(' ').filter(|(_, n)| n.contains(':') && n.chars().all(|c| c.is_ascii_digit() || c == ':'))
                           .map_or(name.clone(), |(head, _)| head.to_string());
            if name.starts_with("XW-P1 Emulator") || name.starts_with("Midi Through") || found.iter().any(|(n, _, _)| *n == name) {
                continue;
            }
            if let Ok(mut input) = MidiInput::new("XW-P1 Emulator controller") {
                input.ignore(Ignore::TimeAndActiveSense);
                found.push((name, port, input));
            }
        }
        found
    }

    /// Open what is wanted and present, drop what has gone; -> the list as JSON.
    fn refresh(&mut self) -> String {
        let present = Self::present();
        let names: Vec<String> = present.iter().map(|(n, _, _)| n.clone()).collect();
        self.open.retain(|name, _| names.contains(name) && self.wanted.contains(name));
        for (name, port, input) in present {
            if self.wanted.contains(&name) && !self.open.contains_key(&name) {
                let (tx, channels) = (self.tx.clone(), self.channels.clone());
                let made = input.connect(&port, "in", move |_, msg, _| {
                    let mut msg = msg.to_vec();
                    if !channels.load(std::sync::atomic::Ordering::Relaxed) && (0x80..0xF0).contains(&msg[0]) {
                        msg[0] &= 0xF0;
                    }
                    let _ = tx.send(msg);
                }, ());
                if let Ok(connection) = made {
                    self.open.insert(name, connection);
                }
            }
        }
        let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
        let mut rows: Vec<String> = names.iter().map(|n| format!("{{\"name\":{},\"on\":{},\"here\":true}}", quote(n), self.open.contains_key(n))).collect();
        rows.extend(self.wanted.iter().filter(|w| !names.contains(w)).map(|w| format!("{{\"name\":{},\"on\":true,\"here\":false}}", quote(w))));
        format!("P [{}]", rows.join(","))
    }

    fn want(&mut self, name: &str, on: bool) -> String {
        self.wanted.retain(|w| w != name);
        if on {
            self.wanted.push(name.to_string());
        }
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.file, self.wanted.join("\n"));
        self.refresh()
    }
}

const HELP: &str = "\
XW-P1 Emulator: the player (audio to PipeWire, MIDI on the ALSA sequencer, the editor on a local web port)

  xwp1 setup UPDATER.zip   import the XW-P1 1.11 firmware from Casio's updater ZIP (or its p1-update.bin)
  xwp1 check               say whether the firmware is imported and its data complete
  xwp1 waves [JOBS]        build the optional wave pictures (resumes where it stopped)
  xwp1 [OPTIONS]           play; the editor is at http://localhost:8800/

  --remote PORT | --no-remote   the editor's port (8800), or no editor
  --node NAME                   PipeWire node name (xwp1)
  --no-connect                  leave the node uncabled instead of connecting it to the default output
  --latency MS                  output latency (20)
  --gain G                      output gain (1.0)
  --voices N                    Solo Synth voices, 1..8 (as last set in the editor)
  --bend SEMITONES              bend range (as last set in the editor)
  --program N  --hex | --drawbar   the tone to start on: Solo Synth preset N, or a Hex Layer / Drawbar Organ one
  --keys                        channel-1 notes play the instrument's keys (zones, arpeggio, phrases)
  --dry                         no system reverb
  --no-user                     do not read or keep the user memory (~/.config/xwp1/user.bin)
  --card FILE | --no-card       another SD card image, or none (~/.config/xwp1/card.img)
  --script FILE --wav FILE      render timed MIDI (\"SECONDS HEX ...\" per line) to a file instead of playing
  --syx FILE                    send a SysEx file after start
  --bench SECONDS               time the emulation and exit
  --image FILE  --panel-dir DIR  --reverb DIR   run from a source tree instead of the installed data
  --version
";

fn main() {
    let mut setup_args = std::env::args().skip(1);
    match setup_args.next().as_deref() {
        Some("--help" | "-h" | "help") => {
            print!("{HELP}");
            return;
        }
        Some("--version" | "-V") => {
            println!("xwp1 {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("setup") => {
            let source = setup_args.next().unwrap_or_else(|| { eprintln!("usage: xwp1 setup UPDATER.zip|p1-update.bin"); std::process::exit(2) });
            if setup_args.next().is_some() { eprintln!("setup accepts one file"); std::process::exit(2); }
            if let Err(e) = xwp1::setup::install(std::path::Path::new(&source)) { eprintln!("setup: {e}"); std::process::exit(1); }
            println!("XW-P1 1.11 installed in {}", xwp1::setup::data_root().display());
            return;
        }
        Some("check") => {
            if let Err(e) = xwp1::setup::validate() { eprintln!("setup check: {e}"); std::process::exit(1); }
            println!("XW-P1 1.11 installation is valid");
            return;
        }
        Some("waves") => {
            let workers = setup_args.next().as_deref().map(str::parse::<usize>).transpose()
                .unwrap_or_else(|_| { eprintln!("usage: xwp1 waves [JOBS]"); std::process::exit(2) }).unwrap_or(4);
            if setup_args.next().is_some() { eprintln!("usage: xwp1 waves [JOBS]"); std::process::exit(2); }
            if let Err(e) = xwp1::waves::build(workers) { eprintln!("wave previews: {e}"); std::process::exit(1); }
            return;
        }
        _ => {}
    }
    let a = args();
    if a.image == xwp1::setup::image_path() {
        if let Err(e) = xwp1::setup::validate() {
            eprintln!("firmware setup is needed: {e}\nRun: xwp1 setup UPDATER.zip");
            std::process::exit(1);
        }
    }
    let t = Instant::now();
    let mut wanted = Poly::load();
    (wanted.voices, wanted.bend) = (a.voices.unwrap_or(wanted.voices).clamp(1, MAX_VOICES), a.bend.unwrap_or(wanted.bend));
    let config = Config { image: a.image.clone(), syx: a.syx.clone(), program: a.program, bank: a.bank, cpu: a.cpu, fast: a.fast,
                          dry: a.dry, reverb: a.reverb.clone(), block: a.block, history: Vec::new(),
                          // offline runs (tests, timings) start from the empty instrument and leave the file alone
                          host_user: None,
                          user: (!a.no_user && a.bench.is_none() && a.script.is_none()).then(|| Poly::file().with_file_name("user.bin")) };
    let mut engine = Engine::start(&config, wanted);
    // the SD card: an image file the firmware reads and writes as it does a card (made, formatted, when it is not
    // there); like the user memory, a render or a benchmark runs without unless it names one
    if !a.no_card {
        let default = (a.bench.is_none() && a.script.is_none()).then(|| Poly::file().with_file_name("card.img"));
        if let Some(path) = a.card.clone().or(default).and_then(|path| xwp1::card::ready(&path)) {
            engine.card(Some(path));
        }
    }
    if wanted.multitimbral {
        eprintln!("8 independent MIDI parts, {} samples at a time", engine.block);
    } else if wanted.voices > 1 {
        eprintln!("{} voices, {} samples at a time; MPE {}, bend range {} semitones", wanted.voices, engine.block,
                  if wanted.mpe { "on" } else { "off" }, wanted.bend);
    }
    eprintln!("ready after {:.1} s", t.elapsed().as_secs_f64());

    if let Some(seconds) = a.bench {
        bench(&mut engine, seconds);
        return;
    }
    if let Some(script) = &a.script {
        render(&mut engine, script, a.wav.as_deref().expect("--script needs --wav FILE"), a.gain, a.keys);
        return;
    }

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let mut input = MidiInput::new("XW-P1 Emulator").expect("ALSA sequencer");
    input.ignore(Ignore::TimeAndActiveSense);
    let keep_channels = Arc::new(std::sync::atomic::AtomicBool::new(engine.alloc.is_some() || engine.poly.multitimbral));
    let controllers = Arc::new(Mutex::new(Controllers::new(tx.clone(), keep_channels.clone())));
    {
        // controllers plugged in later are picked up within a couple of seconds
        let controllers = controllers.clone();
        std::thread::spawn(move || loop {
            controllers.lock().unwrap().refresh();
            std::thread::sleep(Duration::from_secs(2));
        });
    }
    let _input = input.create_virtual("XW-P1 Emulator In", move |_, msg, _| { let _ = tx.send(msg.to_vec()); }, ())
                      .expect("MIDI in port");
    let mut output = MidiOutput::new("XW-P1 Emulator").expect("ALSA sequencer")
                         .create_virtual("XW-P1 Emulator Out").expect("MIDI out port");

    let rate = SAMPLE_RATE.round();

    // The web panel (xwp1/panel): the same MIDI as the ALSA ports, plus status and sound on request.
    let (panel_tx, panel_rx) = mpsc::channel::<Vec<u8>>();
    // text commands from a page: "m?" lists the MIDI controllers, "m+ NAME" / "m- NAME" listens to one or stops;
    // "k?" / "k1" / "k0" asks or sets whether notes play the instrument's own keyboard (answer "K 0|1");
    // "b?" / "b voices N" / "b mode poly|multi" / "b channel PART CHANNEL" / "b mpe 0|1" / "b bend SEMITONES" / "b glide 0|1" asks or
    // sets the instance mode (answer "B VOICES BEND MPE GLIDE", the four voice variations and the mode).
    // "v?" / "v DB" asks or sets the master volume, -40..+24 dB on top of --gain (answer "V DB"), kept in
    // ~/.config/xwp1/volume.txt
    let volume_file = Poly::file().with_file_name("volume.txt");
    let saved_db = std::fs::read_to_string(&volume_file).ok().and_then(|t| t.trim().parse::<f32>().ok()).unwrap_or(0.0);
    let volume = Arc::new(std::sync::atomic::AtomicU32::new(saved_db.clamp(-40.0, 24.0).to_bits()));
    // the key mode is a player setting like the others: kept in ~/.config/xwp1/keys.txt (--keys switches it on for the run)
    let keys_file = Poly::file().with_file_name("keys.txt");
    let keys_text = std::fs::read_to_string(&keys_file).unwrap_or_default();
    let keys_saved = matches!(keys_text.trim(), "1" | "2");
    // "2": with several voices each has its own keyboard (an arpeggio per voice); text command "k2", answer "K 2"
    let keys_each = Arc::new(std::sync::atomic::AtomicBool::new(keys_text.trim() == "2"));
    let keys_mode = Arc::new(std::sync::atomic::AtomicBool::new(a.keys || (keys_saved && a.script.is_none() && a.bench.is_none())));
    let shared = Arc::new(Mutex::new(wanted));
    let morph_ready = std::fs::read(xwp1::setup::generated_dir().join("wave_morph.json")).ok()
        .and_then(|bytes| serde_json::from_slice::<WaveLibrary>(&bytes).ok()).is_some();
    let wave_morph = Arc::new(Mutex::new(None::<WaveMorph>));
    let control: xwp1::panel::Control = {
        let controllers = controllers.clone();
        let keys_mode = keys_mode.clone();
        let keys_each = keys_each.clone();
        let shared = shared.clone();
        let volume = volume.clone();
        let wave_morph = wave_morph.clone();
        Arc::new(move |text: &str| {
            if text == "w?" { return Some(format!("W {}", morph_ready as u8)); }
            if text == "w off" { *wave_morph.lock().unwrap() = None; return Some(format!("W {}", morph_ready as u8)); }
            if let Some(rest) = text.strip_prefix("w ") {
                if morph_ready {
                    let nums: Option<Vec<u16>> = rest.split_whitespace().map(|s| s.parse().ok()).collect();
                    if let Some(nums) = nums.filter(|n| n.len() == 11 && n[0] <= 100) {
                        let mut a = [0; 5]; let mut b = [0; 5];
                        a.copy_from_slice(&nums[1..6]); b.copy_from_slice(&nums[6..11]);
                        if (0..5).all(|i| a[i] <= [310, 310, 2157, 2157, 13][i] && b[i] <= [310, 310, 2157, 2157, 13][i]) {
                            *wave_morph.lock().unwrap() = Some(WaveMorph { amount: nums[0] as u8, a, b });
                        }
                    }
                }
                return Some(format!("W {}", morph_ready as u8));
            }
            if text.starts_with('v') {
                if let Some(db) = text.get(2..).and_then(|t| t.trim().parse::<f32>().ok()).filter(|d| d.is_finite()) {
                    let db = db.clamp(-40.0, 24.0);
                    volume.store(db.to_bits(), std::sync::atomic::Ordering::Relaxed);
                    if let Some(dir) = volume_file.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(&volume_file, db.to_string());
                }
                return Some(format!("V {}", f32::from_bits(volume.load(std::sync::atomic::Ordering::Relaxed))));
            }
            if text.starts_with('b') {
                let mut p = shared.lock().unwrap();
                let before = *p;
                p.set(text.get(2..).unwrap_or(""));
                if *p != before {
                    p.save();
                }
                return Some(p.answer());
            }
            if text.starts_with('k') {
                match text.get(1..2) {
                    Some(on @ ("2" | "1" | "0")) => {
                        keys_mode.store(on != "0", std::sync::atomic::Ordering::Relaxed);
                        keys_each.store(on == "2", std::sync::atomic::Ordering::Relaxed);
                        if let Some(dir) = keys_file.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        let _ = std::fs::write(&keys_file, on);
                    }
                    _ => {}
                }
                let (on, each) = (keys_mode.load(std::sync::atomic::Ordering::Relaxed), keys_each.load(std::sync::atomic::Ordering::Relaxed));
                return Some(format!("K {}", if !on { 0 } else if each { 2 } else { 1 }));
            }
            let mut c = controllers.lock().unwrap();
            match (text.get(..2), text.get(3..)) {
                (Some("m?"), _) => Some(c.refresh()),
                (Some("m+"), Some(name)) => Some(c.want(name, true)),
                (Some("m-"), Some(name)) => Some(c.want(name, false)),
                _ => None,
            }
        })
    };
    let panel = a.remote.and_then(|port| match xwp1::panel::Panel::start(port, a.panel_dir.clone(), panel_tx, rate as u32, control.clone()) {
        Ok(p) => {
            eprintln!("panel: http://localhost:{}/", p.port);
            Some(p)
        }
        Err(e) => {
            eprintln!("panel: not started ({e})");
            None
        }
    });
    let (mut pcm, mut peak, mut status) = (Vec::<i16>::with_capacity(4096), [0.0f32; 2], Instant::now());
    let (mut status_busy, mut status_dropped) = (Duration::ZERO, 0u64);
    let props = format!("{{ node.name = \"{0}\" media.name = \"XW-P1 Emulator\" node.description = \"XW-P1 Emulator\" }}",
                        a.node);
    let mut cmd = Command::new("pw-cat");
    cmd.args(["--playback", "--raw", "--format", "f32", "--channels", "2", "--rate", &format!("{rate}"),
              "--latency", &format!("{}ms", a.latency_ms), "-P", &props]);
    if !a.connect {
        cmd.args(["--target", "0"]);
    }
    // The two pw-cat children must not outlive this process (the capture one
    // never notices on its own while its node is uncabled).
    let with_parent = || unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) }.min(0).eq(&0).then_some(()).ok_or_else(std::io::Error::last_os_error);
    unsafe { cmd.pre_exec(with_parent) };
    let no_pw_cat = |e: std::io::Error| -> ! {
        eprintln!("pw-cat could not be started ({e}): it is the audio output and comes with PipeWire's tools (see README, Requirements)");
        std::process::exit(1)
    };
    let mut player = cmd.arg("-").stdin(Stdio::piped()).spawn().unwrap_or_else(|e| no_pw_cat(e));
    let mut pipe = player.stdin.take().unwrap();
    // pw-cat reads nothing while its node is uncabled. Keep the pipe short
    // (it would otherwise hold 190 ms of stale audio from then on) and
    // write from a thread, so the emulation keeps running in real time and
    // drops its output instead of stalling with MIDI unanswered.
    unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) };
    let (audio_tx, audio_rx) = mpsc::sync_channel::<Vec<u8>>(8);
    std::thread::spawn(move || {
        for block in audio_rx {
            if pipe.write_all(&block).is_err() {
                break; // pw-cat went away
            }
        }
    });
    let mut dropped = 0u64;

    // The instrument input: a mono capture node. Uncabled it delivers
    // nothing and the input is silent.
    let (in_tx, in_rx) = mpsc::channel::<Vec<f32>>();
    let in_props = format!("{{ node.name = \"{0}-in\" media.name = \"XW-P1 Emulator input\" node.description = \"XW-P1 Emulator input\" }}",
                           a.node);
    let mut recorder = Command::new("pw-cat");
    unsafe { recorder.pre_exec(with_parent) };
    let mut recorder = recorder.args(["--record", "--raw", "--format", "f32", "--channels", "1", "--rate",
                                                    &format!("{rate}"), "--latency",
                                                    &format!("{}ms", a.latency_ms), "--target", "0", "-P", &in_props, "-"])
                                             .stdout(Stdio::piped()).spawn().unwrap_or_else(|e| no_pw_cat(e));
    let mut capture = recorder.stdout.take().unwrap();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4 * BLOCK];
        while capture.read_exact(&mut buf).is_ok() {
            let block = buf.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            if in_tx.send(block).is_err() {
                break;
            }
        }
    });
    eprintln!("playing: PipeWire node \"{}\", MIDI ports \"XW-P1 Emulator In\" / \"XW-P1 Emulator Out\"", a.node);

    // Stay `lead` ahead of the wall clock; the rest of the latency is PipeWire's.
    // (A long round is late more often in absolute terms: give it its own length on top.)
    let (mut start, mut produced) = (Instant::now(), 0u64);
    let mut framer = Framer::default();
    let (mut busy, mut report) = (Duration::ZERO, Instant::now());
    let mut bytes = Vec::with_capacity(4 * BLOCK * 8);
    // The hardware output is AC coupled; a pulse wave leaves the model with DC.
    let dc_r = 1.0 - 2.0 * std::f32::consts::PI * 5.0 / rate as f32;
    let (mut dc_x, mut dc_y) = ([0.0f32; 2], [0.0f32; 2]);
    const PRIME: usize = 1024;
    let (mut pending, mut primed) = (std::collections::VecDeque::<f32>::new(), false);
    loop {
        let keys = keys_mode.load(std::sync::atomic::Ordering::Relaxed);
        {
            // settings come from a page, or the bend range from the controller itself (RPN 0)
            let mut p = shared.lock().unwrap();
            if *p != wanted {
                wanted = *p;
                engine.set(wanted);
            } else if let Some(range) = engine.bend_set_by_controller() {
                (p.bend, wanted.bend) = (range, range);
                p.save();
            }
        }
        engine.key_mode(keys, keys_each.load(std::sync::atomic::Ordering::Relaxed));
        engine.set_wave_morph(*wave_morph.lock().unwrap());
        keep_channels.store(engine.alloc.is_some() || engine.poly.multitimbral, std::sync::atomic::Ordering::Relaxed);
        let block = engine.block;
        // (A long round is late more often in absolute terms: give it its own length on top.)
        let lead = Duration::from_millis(10) + Duration::from_secs_f64((block - BLOCK) as f64 / SAMPLE_RATE);
        while let Ok(msg) = rx.try_recv() {
            engine.play(&msg, keys);
            if let Some(p) = &panel {
                // pages follow notes and channel-1 edits, not every finger's bend and pressure
                if msg[0] & 0x0F == 0 || msg[0] >= 0xF0 {
                    p.midi_in(&msg);
                } else if matches!(msg[0] & 0xF0, 0x80 | 0x90) {
                    p.midi_in(&[msg[0] & 0xF0, msg[1], msg[2]]);
                }
            }
        }
        while let Ok(msg) = panel_rx.try_recv() {
            engine.play(&msg, keys);
        }
        // The capture arrives a PipeWire quantum at a time; hold about one
        // quantum back so the emulation never runs dry between arrivals.
        while let Ok(block) = in_rx.try_recv() {
            pending.extend(block.iter().map(|x| x * a.input_gain));
        }
        if pending.len() > 4 * PRIME {
            pending.drain(..pending.len() - PRIME); // fell behind the capture: skip ahead
        }
        primed = if primed { pending.len() >= block } else { pending.len() >= PRIME.max(block) };
        let input: Vec<f32> = if primed { pending.drain(..block).collect() } else { Vec::new() };
        let t = Instant::now();
        engine.run(&input);
        busy += t.elapsed();
        status_busy += t.elapsed();
        bytes.clear();
        let gain = a.gain * 10f32.powf(f32::from_bits(volume.load(std::sync::atomic::Ordering::Relaxed)) / 20.0);
        for (i, s) in engine.out.drain(..).enumerate() {
            let ch = i & 1;
            dc_y[ch] = s - dc_x[ch] + dc_r * dc_y[ch];
            dc_x[ch] = s;
            let v = (dc_y[ch] * gain).clamp(-1.0, 1.0);
            bytes.extend_from_slice(&v.to_le_bytes());
            peak[ch] = peak[ch].max(v.abs());
            if panel.as_ref().is_some_and(|p| p.wants_audio()) {
                pcm.push((v * 32767.0) as i16);
            }
        }
        if pcm.len() >= 2 * 1024 {
            if let Some(p) = &panel {
                p.audio(&pcm);
            }
            pcm.clear();
        }
        match audio_tx.try_send(bytes.clone()) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                dropped += 1;
                status_dropped += 1;
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break,
        }
        produced += block as u64;
        for b in std::mem::take(&mut engine.midi) {
            framer.push(b, |msg| {
                let _ = output.send(msg);
                if let Some(p) = &panel {
                    p.midi_out(msg);
                }
            });
        }
        // memory, display, LEDs: for the pages, not for what listens on the MIDI port
        for msg in engine.pages.drain(..) {
            if let Some(p) = &panel {
                p.midi_out(&msg);
            }
        }
        let due = start + Duration::from_secs_f64(produced as f64 / rate);
        let now = Instant::now();
        if due > now + lead {
            std::thread::sleep(due - now - lead);
        } else if now > due + Duration::from_millis(250) {
            eprintln!("fell {} ms behind; resynchronising", (now - due).as_millis());
            (start, produced) = (now, 0);
        }
        if status.elapsed() > Duration::from_millis(100) {
            if let Some(p) = &panel {
                let fx = engine.fx;
                p.status(&format!("{{\"cpu\":{:.3},\"peak\":[{:.4},{:.4}],\"uncabled\":{},\"fx\":[{},{},{}],\"voices\":{},\"starting\":{},\"held\":{}}}",
                                  status_busy.as_secs_f64() / status.elapsed().as_secs_f64(), peak[0], peak[1],
                                  status_dropped > 0, fx.reverb_type, fx.reverb_time, fx.reverb_level, engine.voices.len(), engine.starting.len(),
                                  engine.alloc.as_ref().map_or(0, |al| al.held())));
            }
            (peak, status, status_busy, status_dropped) = ([0.0; 2], Instant::now(), Duration::ZERO, 0);
        }
        if report.elapsed() > Duration::from_secs(10) {
            eprintln!("CPU load {:.0} %{}", 100.0 * busy.as_secs_f64() / report.elapsed().as_secs_f64(),
                      if dropped > 0 { format!("; {dropped} blocks not played (node uncabled?)") } else { String::new() });
            dropped = 0;
            (busy, report) = (Duration::ZERO, Instant::now());
        }
    }
}
