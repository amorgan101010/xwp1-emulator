//! The XW-P1 emulator as a VST3 / CLAP instrument.
//!
//! The engine (`xwp1::engine`: the instances, the allocator, reverb and
//! line output) runs on a thread of its own at the instrument's 42818 Hz.
//! The host's audio callback hands it time-stamped MIDI and reads its
//! output through a resampler (`bridge`), a fixed delay behind, which is
//! reported to the host as latency.
//!
//! Every plugin instance serves the panel like `xwp1-rt` does, on the first
//! free port from 8820. The plugin's own window is small and native (hosts
//! in a sandbox have no web view to embed): its button opens
//! `xwp1://PORT`, which the desktop entry of `xwp1-app` handles by showing
//! that panel in a window.
//!
//! The edits made since the instance started (tone choice, SysEx,
//! controllers on channel 1: the same history a voice added later is
//! brought up to date with) are saved with the host's project and replayed
//! when it is loaded.
//!
//! The firmware, the data built from it and `reverb/` are in the user's data
//! root (`xwp1::setup::data_root`), the panel's static files where
//! `tools/install_app.sh` put them (`xwp1::setup::panel_dir`).

pub mod bridge;
mod compat;

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use nih_plug::midi::MidiResult;
use nih_plug::params::persist::PersistentField;
use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, egui, EguiState};
use serde::{Deserialize, Serialize};
use xwp1::engine::{Config, Engine, Framer, Poly, DEFAULT_CHANNELS, SOLO_SYNTH_BANK};
use xwp1::machine::CpuKind;
use xwp1::panel::Panel;
use xwp1::sound::SAMPLE_RATE;

use bridge::{Event, Reader};

const FIRST_PORT: u16 = 8820;

/// What a project keeps of an instance.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    history: Vec<Vec<u8>>,
    voices: usize,
    #[serde(default)]
    multitimbral: bool,
    #[serde(default = "default_channels")]
    channels: [u8; 8],
    mpe: bool,
    bend: f32,
    volume_db: f32,
    keys: bool,
    #[serde(default)]
    glide: bool, // the tones' portamento kept when polyphonic
    #[serde(default)]
    each: bool, // key mode with several voices: a keyboard (and so an arpeggio) per voice
    #[serde(default)]
    vary: [u8; 4], // voice variation: glide, filters, envelopes, levels (`xwp1::vary`)
    /// The instrument's user memory (what WRITE stores: Performances, sequences, tones), deflated: a megabyte
    /// that is mostly the firmware's default user data packs to about 11 kB. Empty: not formatted yet.
    #[serde(default)]
    user: Vec<u8>,
}

fn default_channels() -> [u8; 8] { DEFAULT_CHANNELS }

impl Default for State {
    fn default() -> State {
        // MPE and bend range as last chosen in the panel; one voice, since every voice is a thread at work
        let poly = Poly::load();
        State { history: Vec::new(), voices: 1, multitimbral: false, channels: poly.channels, mpe: poly.mpe, bend: poly.bend, volume_db: 0.0, keys: false, glide: poly.glide, each: false, vary: poly.vary, user: Vec::new() }
    }
}

impl State {
    fn poly(&self) -> Poly {
        Poly { voices: self.voices, multitimbral: self.multitimbral, channels: self.channels, mpe: self.mpe, bend: self.bend, glide: self.glide, vary: self.vary }
    }
}

/// The state, shared with the engine's thread; `loaded` counts the times the host has put one in.
#[derive(Clone, Default)]
struct Saved(Arc<(Mutex<State>, AtomicU64)>);

impl<'a> PersistentField<'a, State> for Saved {
    fn set(&self, new_value: State) {
        *self.0 .0.lock().unwrap() = new_value;
        self.0 .1.fetch_add(1, Ordering::SeqCst);
    }

    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&State) -> R,
    {
        f(&self.0 .0.lock().unwrap())
    }
}

#[derive(Params)]
struct Xwp1Params {
    #[persist = "editor"]
    editor: Arc<EguiState>,
    #[persist = "state"]
    saved: Saved,
}

/// What the engine's thread shows of itself.
#[derive(Default)]
struct Shared {
    ready: AtomicBool,
    quit: AtomicBool,
    port: AtomicU32,
    voices: AtomicU32,
    load: AtomicU32,            // per cent of one core's real time
    late: AtomicU64,            // samples the callback asked for and did not get
    limit: AtomicU64,           // the engine may run up to this sample
    failed: Mutex<Option<String>>,
}

/// The callback's end of a running engine.
struct Link {
    shared: Arc<Shared>,
    events: rtrb::Producer<Event>,
    reader: Reader,
    thread: std::thread::Thread,
    clock: f64,   // the instrument's sample that the next host frame's events belong to
    step: f64,    // instrument samples per host frame
    delay: f64,   // instrument samples between an event and its sound
    loaded: u64,  // the state it was started from
    rate: f32,
}

impl Link {
    /// One callback: the MIDI of this block (frame, message) in, the block's sound out.
    fn render(&mut self, mut next: impl FnMut() -> Option<(u32, [u8; 3])>, out: &mut [&mut [f32]], offline: bool) {
        let frames = out.first().map_or(0, |ch| ch.len());
        while let Some((timing, data)) = next() {
            let due = (self.clock + timing as f64 * self.step).max(0.0) as u64;
            let len = if matches!(data[0] & 0xF0, 0xC0 | 0xD0) { 2 } else { 3 };
            let _ = self.events.push(Event { due, len, data });
        }
        let end = self.clock + frames as f64 * self.step;
        self.shared.limit.store(end.floor().max(0.0) as u64, Ordering::Release);
        self.thread.unpark();
        let (missed, slip) = self.reader.read(self.clock - self.delay, out, offline);
        self.shared.late.fetch_add(missed, Ordering::Relaxed);
        self.clock += frames as f64 * self.step - slip;
    }
}

pub struct Xwp1 {
    params: Arc<Xwp1Params>,
    status: Arc<Mutex<Arc<Shared>>>, // the running engine's, for the editor
    panel: Option<Panel>,            // served for as long as the instance lives, whichever engine is running
    pages: Option<Arc<Mutex<mpsc::Receiver<Vec<u8>>>>>, // MIDI from its pages
    link: Option<Link>,
    offline: bool,
}

impl Default for Xwp1 {
    fn default() -> Xwp1 {
        Xwp1 { params: Arc::new(Xwp1Params { editor: EguiState::from_size(380, 190), saved: Saved::default() }),
               status: Default::default(), panel: None, pages: None, link: None, offline: false }
    }
}

fn home() -> PathBuf { xwp1::setup::data_root() }

/// The panel, as xwp1-rt serves it. Its text commands: "b..." instance mode, "v..." volume, "k..." key matrix,
/// "m?" the MIDI controllers (none here: the host brings the MIDI).
fn serve(saved: Saved) -> (Option<Panel>, Arc<Mutex<mpsc::Receiver<Vec<u8>>>>) {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let control: xwp1::panel::Control = Arc::new(move |text: &str| {
        let mut s = saved.0 .0.lock().unwrap();
        match text.as_bytes().first() {
            Some(b'v') => {
                if let Some(db) = text.get(2..).and_then(|t| t.trim().parse::<f32>().ok()).filter(|d| d.is_finite()) {
                    s.volume_db = db.clamp(-40.0, 24.0);
                }
                Some(format!("V {}", s.volume_db))
            }
            Some(b'b') => {
                let mut p = s.poly();
                p.set(text.get(2..).unwrap_or(""));
                (s.voices, s.multitimbral, s.channels, s.mpe, s.bend, s.glide, s.vary) = (p.voices, p.multitimbral, p.channels, p.mpe, p.bend, p.glide, p.vary);
                Some(p.answer())
            }
            Some(b'k') => {
                match text.get(1..2) {
                    Some(on @ ("2" | "1" | "0")) => (s.keys, s.each) = (on != "0", on == "2"),
                    _ => {}
                }
                Some(format!("K {}", if !s.keys { 0 } else if s.each { 2 } else { 1 }))
            }
            Some(b'm') => Some("P []".into()),
            _ => None,
        }
    });
    let panel = Panel::start(FIRST_PORT, xwp1::setup::panel_dir(), tx, SAMPLE_RATE.round() as u32, control);
    if let Err(e) = &panel {
        eprintln!("xwp1 panel: not started ({e})");
    }
    (panel.ok(), Arc::new(Mutex::new(rx)))
}

/// The engine's thread: boots, then keeps `shared.limit` samples made.
fn engine(shared: Arc<Shared>, saved: Saved, mut events: rtrb::Consumer<Event>, mut audio: rtrb::Producer<f32>,
          panel: Option<Panel>, pages: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>) {
    let home = home();
    let image = xwp1::setup::image_path();
    if let Err(e) = xwp1::setup::validate() {
        *shared.failed.lock().unwrap() = Some(format!("Choose an XW-P1 1.11 updater in the desktop app ({e})"));
        return;
    }
    let start = saved.0 .0.lock().unwrap().clone();
    let config = Config { image, syx: None, program: 0, bank: SOLO_SYNTH_BANK, cpu: CpuKind::Native, fast: true, dry: false,
                          reverb: home.join("reverb"), block: None, history: start.history.clone(), user: None,
                          host_user: Some(miniz_oxide::inflate::decompress_to_vec(&start.user).ok().filter(|d| !d.is_empty())) };
    let mut wanted = start.poly();
    let mut engine = Engine::start(&config, wanted);
    // the SD card is the player's: one image for the app and every plugin instance (~/.config/xwp1/card.img)
    if let Some(path) = xwp1::card::ready(&Poly::file().with_file_name("card.img")) {
        engine.card(Some(path));
    }

    let rate = SAMPLE_RATE.round();
    shared.voices.store(engine.voices.len() as u32, Ordering::Relaxed);
    shared.ready.store(true, Ordering::Release);

    let mut pending = std::collections::VecDeque::<Event>::new();
    let (mut produced, mut framer) = (0u64, Framer::default());
    // The hardware output is AC coupled; a pulse wave leaves the model with DC.
    let dc_r = 1.0 - 2.0 * std::f32::consts::PI * 5.0 / rate as f32;
    let (mut dc_x, mut dc_y) = ([0.0f32; 2], [0.0f32; 2]);
    let (mut peak, mut status, mut busy, mut pcm) = ([0.0f32; 2], Instant::now(), Duration::ZERO, Vec::<i16>::new());
    let mut known = engine.history.len();
    while !shared.quit.load(Ordering::Relaxed) {
        let (keys, each, volume) = {
            // settings come from a page, or the bend range from the controller itself (RPN 0)
            let mut s = saved.0 .0.lock().unwrap();
            if s.poly() != wanted {
                wanted = s.poly();
                engine.set(wanted);
            } else if let Some(range) = engine.bend_set_by_controller() {
                (s.bend, wanted.bend) = (range, range);
            }
            (s.keys, s.each, s.volume_db)
        };
        engine.key_mode(keys, each);
        // The callback puts its events in before it raises the limit: read in the other order,
        // so a stretch is never begun without the events that belong to it.
        let limit = shared.limit.load(Ordering::Acquire);
        while let Ok(e) = events.pop() {
            pending.push_back(e);
        }
        let mut edited = false;
        while let Ok(msg) = pages.lock().unwrap().try_recv() {
            engine.play(&msg, keys);
            edited = true;
        }
        // Whole rounds only, never past the limit: the next callback's events are due from there on.
        if produced + engine.block as u64 > limit {
            if edited || engine.history.len() != known {
                saved.0 .0.lock().unwrap().history = engine.history.clone();
                known = engine.history.len();
            }
            std::thread::park_timeout(Duration::from_millis(2));
            continue;
        }
        while pending.front().is_some_and(|e| e.due < produced + engine.block as u64) {
            let e = pending.pop_front().unwrap();
            let msg = &e.data[..e.len as usize];
            engine.play(msg, keys);
            edited |= msg[0] == 0xB0 || msg[0] == 0xC0;
            if let Some(p) = &panel {
                // pages follow notes and channel-1 edits, not every finger's bend and pressure
                if msg[0] & 0x0F == 0 {
                    p.midi_in(msg);
                } else if matches!(msg[0] & 0xF0, 0x80 | 0x90) {
                    p.midi_in(&[msg[0] & 0xF0, msg[1], msg[2]]);
                }
            }
        }
        if edited {
            saved.0 .0.lock().unwrap().history = engine.history.clone();
            known = engine.history.len();
        }
        let t = Instant::now();
        engine.run(&[]);
        busy += t.elapsed();
        let gain = 10f32.powf(volume / 20.0);
        let listened = panel.as_ref().is_some_and(|p| p.wants_audio());
        for (i, s) in engine.out.drain(..).enumerate() {
            let ch = i & 1;
            dc_y[ch] = s - dc_x[ch] + dc_r * dc_y[ch];
            dc_x[ch] = s;
            let v = dc_y[ch] * gain;
            peak[ch] = peak[ch].max(v.abs());
            let _ = audio.push(v); // full only if the callback stopped reading: it resynchronises
            if listened {
                pcm.push((v.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
        }
        if pcm.len() >= 2 * 1024 {
            if let Some(p) = &panel {
                p.audio(&pcm);
            }
            pcm.clear();
        }
        produced += engine.block as u64;
        for b in std::mem::take(&mut engine.midi) {
            framer.push(b, |msg| {
                if let Some(p) = &panel {
                    p.midi_out(msg);
                }
            });
        }
        for msg in engine.pages.drain(..) {
            if let Some(p) = &panel {
                p.midi_out(&msg);
            }
        }
        // WRITE on the instrument (or its first-boot format): the project keeps the user memory
        if let Some(area) = engine.user_written.take() {
            saved.0 .0.lock().unwrap().user = miniz_oxide::deflate::compress_to_vec(&area, 6);
        }
        if status.elapsed() > Duration::from_millis(100) {
            let load = busy.as_secs_f64() / status.elapsed().as_secs_f64();
            shared.load.store((100.0 * load) as u32, Ordering::Relaxed);
            shared.voices.store(engine.voices.len() as u32, Ordering::Relaxed);
            if let Some(p) = &panel {
                let fx = engine.fx;
                p.status(&format!("{{\"cpu\":{:.3},\"peak\":[{:.4},{:.4}],\"uncabled\":false,\"fx\":[{},{},{}],\"voices\":{},\"starting\":{},\"held\":{}}}",
                                  load, peak[0], peak[1], fx.reverb_type, fx.reverb_time, fx.reverb_level, engine.voices.len(),
                                  engine.starting.len(), engine.alloc.as_ref().map_or(0, |al| al.held())));
            }
            (peak, status, busy) = ([0.0; 2], Instant::now(), Duration::ZERO);
        }
    }
}

impl Xwp1 {
    fn start(&mut self, rate: f32, max_block: u32) -> u32 {
        if let Some(old) = self.link.take() {
            old.shared.quit.store(true, Ordering::Relaxed);
            old.thread.unpark();
        }
        let shared = Arc::new(Shared::default());
        *self.status.lock().unwrap() = shared.clone();
        let pages = match &self.pages {
            Some(pages) => {
                // the pages that are open read the instrument again once it is back
                self.panel.iter().for_each(|p| p.hang_up());
                pages.clone()
            }
            None => {
                let (panel, pages) = serve(self.params.saved.clone());
                (self.panel, self.pages) = (panel, Some(pages.clone()));
                pages
            }
        };
        let panel = self.panel.clone();
        shared.port.store(panel.as_ref().map_or(0, |p| p.port as u32), Ordering::Relaxed);
        let (event_tx, event_rx) = rtrb::RingBuffer::new(4096);
        let (audio_tx, audio_rx) = rtrb::RingBuffer::new(2 * SAMPLE_RATE as usize);
        let saved = self.params.saved.clone();
        let loaded = saved.0 .1.load(Ordering::SeqCst);
        let theirs = shared.clone();
        let thread = std::thread::Builder::new().name("xwp1".into()).spawn(move || {
            let shared = theirs.clone();
            let run = std::panic::AssertUnwindSafe(move || engine(theirs, saved, event_rx, audio_tx, panel, pages));
            if let Err(e) = std::panic::catch_unwind(run) {
                let text = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
                *shared.failed.lock().unwrap() = Some(text.unwrap_or_else(|| "the emulation stopped".into()));
                shared.ready.store(false, Ordering::Release);
            }
        }).expect("thread");
        let step = SAMPLE_RATE / rate as f64;
        // The engine learns of a callback's events when the callback begins and has until the next one
        // to make that stretch: one host block, one engine round (256 samples when polyphonic) and some
        // slack, counted in the instrument's samples so that it holds at any host rate.
        let delay = max_block.min(4096) as f64 * step + 256.0 + 128.0;
        let delay_frames = (delay / step).ceil() as u32;
        self.link = Some(Link { shared, events: event_tx, reader: Reader::new(audio_rx, step), thread: thread.thread().clone(),
                                clock: 0.0, step, delay: delay_frames as f64 * step, loaded, rate });
        delay_frames
    }
}

impl Drop for Xwp1 {
    fn drop(&mut self) {
        if let Some(link) = &self.link {
            link.shared.quit.store(true, Ordering::Relaxed);
            link.thread.unpark();
        }
        if let Some(panel) = &self.panel {
            panel.stop();
        }
    }
}

impl Plugin for Xwp1 {
    const NAME: &'static str = "XW-P1 Emulator";
    const VENDOR: &'static str = "xwp1";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: None,
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];
    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        let status = self.status.clone();
        create_egui_editor(self.params.editor.clone(), (), |_, _| {}, move |ctx, _setter, _| {
            let shared = status.lock().unwrap().clone();
            ctx.request_repaint_after(Duration::from_millis(200));
            let gold = egui::Color32::from_rgb(0xd9, 0xb5, 0x6c);
            let frame = egui::Frame::default().fill(egui::Color32::from_rgb(0x0f, 0x10, 0x12)).inner_margin(18.0);
            egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                ui.label(egui::RichText::new("XW-P1").size(26.0).strong().color(gold));
                ui.add_space(6.0);
                let port = shared.port.load(Ordering::Relaxed);
                let failed = shared.failed.lock().unwrap().clone();
                let grey = egui::Color32::from_rgb(0x9a, 0x93, 0x8a);
                if let Some(why) = failed {
                    ui.label(egui::RichText::new("The emulation is not running").color(egui::Color32::from_rgb(0xe0, 0x70, 0x60)));
                    ui.label(egui::RichText::new(why).small().color(grey));
                    if ui.button("Set up firmware").clicked() {
                        let _ = std::process::Command::new("xdg-open").arg("xwp1://setup").spawn();
                    }
                    if xwp1::setup::generated_dir().join("manifest.json").exists() { ui.label("Reopen this plugin instance to start the instrument."); }
                } else if !shared.ready.load(Ordering::Acquire) {
                    ui.label(egui::RichText::new("starting the instrument…").color(grey));
                } else {
                    let voices = shared.voices.load(Ordering::Relaxed);
                    ui.label(egui::RichText::new(format!("{voices} voice{}  ·  {} % of a core  ·  panel on port {port}",
                                                         if voices == 1 { "" } else { "s" }, shared.load.load(Ordering::Relaxed)))
                             .color(grey));
                    ui.add_space(12.0);
                    let button = egui::Button::new(egui::RichText::new("Open editor").size(16.0).color(egui::Color32::from_rgb(0x1a, 0x15, 0x08)))
                        .fill(gold).min_size(egui::vec2(150.0, 34.0));
                    if ui.add(button).clicked() && port != 0 {
                        // the desktop entry of xwp1-app takes the URL (through the portal when the host is sandboxed)
                        if let Ok(mut child) = std::process::Command::new("xdg-open").arg(format!("xwp1://{port}")).spawn() {
                            std::thread::spawn(move || { let _ = child.wait(); });
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Sounds, polyphony or 8-part multitimbral mode, and MPE are set there and saved with the project.").small().color(grey));
                }
            });
        })
    }

    fn initialize(&mut self, _layout: &AudioIOLayout, config: &BufferConfig, context: &mut impl InitContext<Self>) -> bool {
        self.offline = config.process_mode == ProcessMode::Offline;
        let loaded = self.params.saved.0 .1.load(Ordering::SeqCst);
        let same = self.link.as_ref().is_some_and(|l| l.loaded == loaded && l.rate == config.sample_rate
                                                       && l.shared.failed.lock().unwrap().is_none());
        let delay = match (&self.link, same) {
            (Some(l), true) => (l.delay / l.step).round() as u32,
            _ => self.start(config.sample_rate, config.max_buffer_size),
        };
        context.set_latency_samples(delay);
        true
    }

    fn process(&mut self, buffer: &mut Buffer, _aux: &mut AuxiliaryBuffers, context: &mut impl ProcessContext<Self>) -> ProcessStatus {

        let out = buffer.as_slice();
        let Some(link) = self.link.as_mut().filter(|l| l.shared.ready.load(Ordering::Acquire)) else {
            while context.next_event().is_some() {}
            out.iter_mut().for_each(|ch| ch.fill(0.0));
            return ProcessStatus::KeepAlive;
        };
        link.render(|| loop {
            let event = context.next_event()?;
            if let Some(MidiResult::Basic(data)) = event.as_midi() {
                return Some((event.timing(), data));
            }
        }, out, self.offline);
        ProcessStatus::KeepAlive
    }
}

impl ClapPlugin for Xwp1 {
    const CLAP_ID: &'static str = "local.xwp1.emulator";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Casio XW-P1 emulator (bring your own firmware)");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::Instrument, ClapFeature::Synthesizer, ClapFeature::Stereo];
}

impl Vst3Plugin for Xwp1 {
    const VST3_CLASS_ID: [u8; 16] = *b"Xwp1EmulatorSolo";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Instrument, Vst3SubCategory::Synth];
}

nih_export_clap!(Xwp1);
nih_export_vst3!(Xwp1);

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays one note the way a host would: 512-frame callbacks at 48 kHz. Returns left, right interleaved.
    fn play(plugin: &mut Xwp1, offline: bool) -> (Vec<f32>, u64) {
        plugin.start(48000.0, 512);
        let link = plugin.link.as_mut().unwrap();
        let t = Instant::now();
        while !link.shared.ready.load(Ordering::Acquire) {
            assert!(link.shared.failed.lock().unwrap().is_none(), "{:?}", link.shared.failed.lock().unwrap());
            assert!(t.elapsed() < Duration::from_secs(60), "the engine did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut wave = Vec::new();
        for block in 0..188 {
            let mut events = match block {
                10 => vec![(100, [0x90, 60, 100])],
                100 => vec![(7, [0x80, 60, 0])],
                _ => Vec::new(),
            };
            let (mut l, mut r) = ([0.0f32; 512], [0.0f32; 512]);
            let begun = Instant::now();
            link.render(|| events.pop(), &mut [&mut l[..], &mut r[..]], offline);
            wave.extend(l.iter().zip(&r).flat_map(|(l, r)| [*l, *r]));
            if !offline {
                // a callback every 512 frames of real time
                std::thread::sleep(Duration::from_secs_f64(512.0 / 48000.0).saturating_sub(begun.elapsed()));
            }
        }
        (wave, link.shared.late.load(Ordering::Relaxed))
    }

    fn write(name: &str, wave: &[f32]) {
        if let Some(dir) = std::env::var_os("XWP1_TEST_OUT") {
            let bytes: Vec<u8> = wave.iter().flat_map(|s| s.to_le_bytes()).collect();
            std::fs::write(PathBuf::from(dir).join(name), bytes).unwrap();
        }
    }

    /// Needs the firmware: `cargo test --release -- --ignored`. With XWP1_TEST_OUT=DIR the sound is
    /// written there as raw float32 stereo, 48 kHz.
    #[test]
    #[ignore]
    fn a_note_sounds_and_the_state_comes_back() {
        let mut plugin = Xwp1::default();
        // A new instance formats its user memory while it boots and the project has it a second of playing
        // later. An instance started from that memory boots 0.4 s sooner, so its free-running LFOs stand
        // elsewhere: the renders compared below all start from the kept memory.
        play(&mut plugin, true);
        assert!(!plugin.params.saved.0 .0.lock().unwrap().user.is_empty(), "the user memory did not reach the state");
        let (offline, late) = play(&mut plugin, true);
        let peak = |w: &[f32]| w.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak(&offline) > 0.05 && late == 0, "offline: peak {}, {late} frames late", peak(&offline));
        // silent until the note (block 10, frame 100) plus the reported delay
        let delay = (plugin.link.as_ref().unwrap().delay / plugin.link.as_ref().unwrap().step).round() as usize;
        let onset = offline.chunks_exact(2).position(|f| f[0].abs() > 0.002).unwrap();
        let due = 10 * 512 + 100 + delay;
        assert!((due..due + 480).contains(&onset), "note heard at frame {onset}, due at {due}");
        write("offline.f32", &offline);
        // its panel is served
        let port = plugin.link.as_ref().unwrap().shared.port.load(Ordering::Relaxed) as u16;
        let mut page = String::new();
        {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("panel port");
            s.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
            let _ = s.read_to_string(&mut page);
        }
        assert!(port >= FIRST_PORT && page.contains("<title>XW-P1"), "port {port}: {}", &page[..page.len().min(200)]);

        let (live, late) = play(&mut plugin, false);
        assert!(peak(&live) > 0.05 && late == 0, "live: peak {}, {late} frames late", peak(&live));
        // the same events at the same samples, whether the host runs in real time or ahead
        let apart = offline.iter().zip(&live).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(apart < 1e-6, "live and offline renders differ by {apart}");
        assert_eq!(plugin.link.as_ref().unwrap().shared.port.load(Ordering::Relaxed) as u16, port, "the panel moved");
        write("live.f32", &live);
        // the project holds the instrument's user memory once the firmware has formatted it
        let kept = plugin.params.saved.0 .0.lock().unwrap().user.clone();
        let area = miniz_oxide::inflate::decompress_to_vec(&kept).unwrap_or_default();
        assert!(area.len() == 0x10_0000 && kept.len() < 40_000, "user memory in the state: {} bytes packed, {} unpacked", kept.len(), area.len());

        // another tone (program 20) put in as a project would: it is what plays after the restart
        let mut state = State::default();
        state.history = vec![vec![0xC0, 20]];
        plugin.params.saved.set(state);
        let (other, _) = play(&mut plugin, true);
        assert!(peak(&other) > 0.01);
        let differs = offline.iter().zip(&other).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(differs > 0.02, "the restored tone sounds like the default one");
        write("program20.f32", &other);
        assert_eq!(plugin.params.saved.0 .0.lock().unwrap().history, vec![vec![0xC0, 20]]);
        // the port is given back when the instance goes
        drop(plugin);
        std::thread::sleep(Duration::from_millis(300));
        assert!(std::net::TcpListener::bind(("0.0.0.0", port)).is_ok(), "port {port} is still taken");
    }
}
