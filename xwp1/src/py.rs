//! Python binding, so emu/session.py and the test battery can drive this
//! core in place of emu/machine.py. Build: `maturin develop --release
//! --features python` in the project venv.
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::machine::{CpuKind, Machine};

#[pyclass(unsendable)]
struct Core {
    m: Machine,
    audio: Vec<f32>,       // left, right interleaved since boot
    reverb_send: Vec<f32>, // one per sample
}

#[pymethods]
impl Core {
    /// `image`: flash in address order (tools/xwimg.py `load()`). Runs
    /// match emu/machine.py instruction for instruction. `unicorn` runs the
    /// firmware under Unicorn instead of the interpreter (the reference; the
    /// two give identical audio), and `fast` then counts instructions per
    /// block, which is faster and no longer exact.
    #[new]
    #[pyo3(signature = (image, fast=false, unicorn=false))]
    fn new(image: Vec<u8>, fast: bool, unicorn: bool) -> PyResult<Self> {
        let kind = if unicorn { CpuKind::Unicorn } else { CpuKind::Native };
        let mut m = Machine::with_cpu(image, kind).map_err(PyRuntimeError::new_err)?;
        if fast {
            m.set_block_stepping().map_err(PyRuntimeError::new_err)?;
        }
        Ok(Core { m, audio: Vec::new(), reverb_send: Vec::new() })
    }

    /// Run `samples` audio samples; raises on a CPU fault.
    fn run(&mut self, samples: usize) -> PyResult<()> {
        let ok = self.m.run(samples);
        self.audio.append(&mut self.m.out);
        self.reverb_send.append(&mut self.m.rev_out);
        if ok { Ok(()) } else { Err(PyRuntimeError::new_err(self.m.fault.clone().unwrap_or_default())) }
    }

    fn midi_in(&mut self, data: Vec<u8>) {
        self.m.midi_in(&data);
    }

    /// Queue a key matrix event (key code, flag bit, velocity byte).
    fn key(&mut self, code: u32, flag: bool, velocity: u32) {
        self.m.key(code, flag, velocity);
    }

    fn button(&mut self, code: u8, down: bool) {
        self.m.button(code, down);
    }

    /// Everything the firmware sent to the panel sub-CPU so far.
    fn panel_sent<'py>(&mut self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.m.devices().panel.sent)
    }

    /// Audio since boot from sample `start`, mono (left + right) / 2, as
    /// little-endian float32 bytes.
    #[pyo3(signature = (start=0))]
    fn audio<'py>(&self, py: Python<'py>, start: usize) -> Bound<'py, PyBytes> {
        let bytes: Vec<u8> = self.audio[(2 * start).min(self.audio.len())..]
            .chunks_exact(2).flat_map(|s| ((s[0] + s[1]) / 2.0).to_le_bytes()).collect();
        PyBytes::new(py, &bytes)
    }

    /// The same in stereo: left, right interleaved.
    #[pyo3(signature = (start=0))]
    fn audio_stereo<'py>(&self, py: Python<'py>, start: usize) -> Bound<'py, PyBytes> {
        let bytes: Vec<u8> =
            self.audio[(2 * start).min(self.audio.len())..].iter().flat_map(|s| s.to_le_bytes()).collect();
        PyBytes::new(py, &bytes)
    }

    /// What the output voices send to the reverb, one float32 per sample.
    #[pyo3(signature = (start=0))]
    fn reverb_send<'py>(&self, py: Python<'py>, start: usize) -> Bound<'py, PyBytes> {
        let bytes: Vec<u8> =
            self.reverb_send[start.min(self.reverb_send.len())..].iter().flat_map(|s| s.to_le_bytes()).collect();
        PyBytes::new(py, &bytes)
    }

    #[getter]
    fn samples(&self) -> usize {
        self.audio.len() / 2
    }

    #[getter]
    fn bus_peak(&mut self) -> f64 {
        self.m.devices().sound.bus_peak
    }

    /// Work RAM accesses: (reads of each sample's first address, other reads, writes).
    fn work_stats(&mut self) -> (u64, u64, u64) {
        let w = self.m.devices().sound.work;
        (w[0], w[1], w[2])
    }

    /// A sound port word (offset from 0x1fff0000) as the firmware last wrote it.
    fn port_word(&mut self, off: usize) -> u32 {
        self.m.devices().sound.port_word(off)
    }

    /// Turn the system chorus model on or off.
    fn set_chorus(&mut self, on: bool) {
        self.m.devices().sound.chorus_on = on;
    }

    /// (fiqs, fiq_lost, irqs)
    fn counters(&self) -> (u64, u64, u64) {
        (self.m.fiqs, self.m.fiq_lost, self.m.irqs)
    }

    /// Bytes on the panel link that the firmware never read (the next one replaced them).
    fn rx_lost(&mut self) -> u64 {
        self.m.devices().panel.rx_lost
    }

    /// CPU instructions run per audio sample (default 561: 48 MHz at two
    /// clocks per instruction). For experiments with the CPU's speed.
    fn set_insns_per_sample(&mut self, n: usize) {
        self.m.insns_per_sample = n;
    }

    /// Experiment knob: level-ramp rate divisor (see sound.rs).
    fn set_ramp_div(&mut self, div: f64) {
        self.m.devices().sound.ramp_div = div;
    }

    /// Queue audio for the instrument input: float32 samples at the chip's
    /// rate, 1.0 = the converter's full scale. One is consumed per sample;
    /// with the queue empty the input is silent.
    fn push_input(&mut self, data: Vec<u8>) {
        self.m.input.extend(data.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
    }

    /// Turn the line-output response (bass roll-off, treble droop) on or off (on by default).
    fn set_output_stage(&mut self, on: bool) {
        self.m.output_stage = on.then(Default::default);
    }

    /// Turn the reverb model on or off (on by default).
    fn set_reverb(&mut self, on: bool) {
        self.m.reverb = on.then(|| crate::reverb::Reverb::new(crate::sound::SAMPLE_RATE));
        self.m.reverb_setting = None;
    }

    /// Use the measured responses in a directory (TYPE_CC.f32, tools/reverb_ir.py):
    /// the reverb then follows the type, time and level the firmware sets.
    fn set_reverb_dir(&mut self, dir: &str) -> bool {
        self.m.reverb_bank = crate::reverb::ReverbBank::open(std::path::Path::new(dir));
        self.m.reverb_setting = None;
        self.m.reverb_bank.is_some()
    }

    /// The system effect settings the firmware last sent: (reverb type, time code, level x 2, chorus rate code, chorus level).
    fn system_fx(&mut self) -> (u8, u8, u8, u8, u8) {
        let fx = self.m.devices().sound.fx;
        (fx.reverb_type, fx.reverb_time, fx.reverb_level, fx.chorus_rate, fx.chorus_level)
    }

    /// Use a measured reverb response (float32, left / right interleaved,
    /// at the chip's sample rate, per unit of reverb send).
    fn set_reverb_response(&mut self, data: Vec<u8>) {
        let taps: Vec<f32> = data.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        self.m.reverb = Some(crate::reverb::Reverb::from_response(&taps));
    }

    /// Experiment knob: gain from the oscillator mix into the DSP bus.
    fn set_bus_gain(&mut self, gain: f64) {
        self.m.devices().sound.bus_gain = gain;
    }

    fn set_adc(&mut self, channel: usize, value: u32) {
        self.m.devices().adc.values[channel] = value;
    }

    /// An input of the slider / knob multiplexer: channel 5 or 6, input 0..7, 10 bits.
    fn set_adc_mux(&mut self, channel: usize, input: usize, value: u32) {
        self.m.devices().adc.muxed[channel - 5][input & 7] = value & 0x3FF;
    }

    fn dial(&mut self, clicks: i8) {
        self.m.dial(clicks);
    }

    /// Bytes into the work RAM at 0x1c000000.. -> whether they were inside it.
    fn poke(&mut self, addr: u32, data: Vec<u8>) -> bool {
        self.m.poke(addr, &data)
    }

    /// A slider (0..8: 1..8 and MASTER) or knob (9..12) at the position the firmware is to read, 0..127.
    fn control(&mut self, control: usize, position: u8) {
        self.m.control(control, position);
    }

    fn mem_read<'py>(&self, py: Python<'py>, addr: u32, size: usize) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.m.mem_read(addr, size))
    }

    /// The stored wave of each voice playing a wave: [(voice, start
    /// address, phase increment, loop start index, int16 samples)].
    fn wave_shots<'py>(&mut self, py: Python<'py>, limit: usize) -> Vec<(usize, u32, f64, usize, Bound<'py, PyBytes>)> {
        self.m.devices().wave_shots(limit).into_iter().map(|(n, addr, inc, looped, s)| {
            let bytes: Vec<u8> = s.iter().flat_map(|v| v.to_le_bytes()).collect();
            (n, addr, inc, looped, PyBytes::new(py, &bytes))
        }).collect()
    }

    /// Turn the sound-register event log on or off.
    fn log_events(&mut self, on: bool) {
        self.m.devices().sound.log = on;
    }

    /// Logged events as (sample, kind, command, data); kind 0 voice,
    /// 1 register file, 2 block 0x60, 3 slot. Clears the log.
    fn events(&mut self) -> Vec<(u64, u8, u32, u64)> {
        std::mem::take(&mut self.m.devices().sound.events).iter().map(|e| (e.sample, e.kind, e.command, e.data)).collect()
    }

    /// Put an SD card in the slot: the image file of its sectors (a whole card, as `dd` reads one), or None
    /// to take it out.
    #[pyo3(signature = (path, read_only=false))]
    fn insert_card(&mut self, path: Option<&str>, read_only: bool) -> PyResult<()> {
        let card = match path {
            Some(path) => {
                let file = std::fs::OpenOptions::new().read(true).write(!read_only).open(path)
                    .map_err(|e| PyRuntimeError::new_err(format!("{path}: {e}")))?;
                Some(crate::card::Card::new(crate::card::Store::File(file), read_only))
            }
            None => None,
        };
        self.m.insert_card(card);
        Ok(())
    }

    /// Start recording the card's commands; -> those so far as (command, | 0x80 for an application command; argument).
    fn card_log(&mut self) -> Vec<(u8, u32)> {
        match &mut self.m.devices().slot.card {
            Some(card) => card.log.replace(Vec::new()).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// (bytes exchanged over the card's line, blocks written, commands the card model does not know).
    fn card_stats(&mut self) -> (u64, u64, Vec<u8>) {
        let slot = &self.m.devices().slot;
        (slot.exchanged, slot.card.as_ref().map_or(0, |c| c.written), slot.card.as_ref().map_or(Vec::new(), |c| c.unknown.clone()))
    }

    /// A port's input pins (port 0..3) as the firmware reads them.
    fn set_port_input(&mut self, port: usize, value: u32) {
        self.m.devices().ports.inputs[port] = value;
    }

    /// Start (or stop) recording port and unmodelled accesses; -> those recorded so far as (is_write, address, value).
    fn trace(&mut self, on: bool) -> Vec<(bool, u32, u32)> {
        let old = self.m.devices().trace.take().unwrap_or_default();
        self.m.devices().trace = on.then(Vec::new);
        old
    }

    /// MMIO accesses nothing models: [(is_write, address, count)].
    fn unknown(&mut self) -> Vec<(bool, u32, u64)> {
        self.m.devices().unknown.iter().map(|(&(w, a), &n)| (w, a, n)).collect()
    }
}

/// Make a card image (`crate::card::create`).
#[pyfunction]
fn card_create(path: &str, bytes: u64) -> PyResult<()> {
    crate::card::create(std::path::Path::new(path), bytes).map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

/// The files in a card image's MUSICDAT folder: [(name, bytes)].
#[pyfunction]
fn card_list(path: &str) -> PyResult<Vec<(String, u64)>> {
    crate::card::list(std::path::Path::new(path)).map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

#[pyfunction]
fn card_read<'py>(py: Python<'py>, path: &str, name: &str) -> PyResult<Bound<'py, PyBytes>> {
    let data = crate::card::read(std::path::Path::new(path), name).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(PyBytes::new(py, &data))
}

#[pyfunction]
fn card_write(path: &str, name: &str, data: Vec<u8>) -> PyResult<()> {
    crate::card::write(std::path::Path::new(path), name, &data).map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

#[pymodule]
fn xwp1(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(card_create, m)?)?;
    m.add_function(wrap_pyfunction!(card_list, m)?)?;
    m.add_function(wrap_pyfunction!(card_read, m)?)?;
    m.add_function(wrap_pyfunction!(card_write, m)?)?;
    m.add_class::<Core>()
}
