//! Sound source model, stepped once per output sample.
//!
//! - 64 voices playing 16-bit samples from CPU memory (flash, or SRAM for
//!   the streaming voices) with linear interpolation; loop start / loop end
//!   / initial position in voice registers 0 / 1 / 2. Register 3 bits 0-16
//!   hold the current pitch and register 4 the pitch to glide to, with the
//!   glide rate in its top byte; the firmware rewrites the target every
//!   4 ms for pitch envelopes, portamento and LFOs.
//! - Level from register-file word 2: the low half is the current level
//!   (15 bits, linear), the high half a ramp: bit 31 = ramping, bits 24-30
//!   the target and bits 16-23 the rate, both as 4-bit-mantissa floats.
//!   The firmware's envelopes are sequences of such ramps (32 ms steps, or
//!   8 ms in a release); it polls bit 31 and reads the level back.
//! - The Solo Synth path. The chip mixes the oscillator voices (0x33..0x37)
//!   and stores the mix, one word per sample, in its work RAM in a 12-bit
//!   mantissa / 4-bit exponent format; the ARM reads that word in its
//!   per-sample FIQ, runs the Total Filter and effect, and writes the
//!   result into a 64-sample ring in SRAM that voice 0x3e plays at unity.
//!
//! - The per-oscillator filter (`OscFilter`): register-file word 0 bits
//!   27-30 are the cutoff code and word 4 the dry share.
//!
//! - Register-file word 3 is a voice's mix: bits 0-9 and 16-25 the left
//!   and right output levels (linear; the firmware folds volume and pan
//!   into them), bits 10-15 the send to the effect bus, bits 26-31 the
//!   reverb send. The effect bus is the part's chorus send: with Line
//!   Select = DSP (the Solo Synth, 42 of the 50 Hex Layer presets) the firmware
//!   writes 63 there and no output level, and its per-sample routine
//!   processes the bus; otherwise the bus feeds the system chorus.
//! - Hex Layer and the other PCM tones: up to six voices per note taken
//!   from all 64 (0x3a is the external input; 0x3e / 0x3f are ordinary
//!   voices when no effect routine streams through them), the same
//!   registers, and the filter through word 0 alone (word 4 = 0).
//!
//! - Ramp events. The register file has an event word at port + 0x46: bit
//!   15 = valid, bits 3-8 the voice, bit 0 = which ramp has reached its
//!   target (0 the level, word 2; 1 the dry share, word 4), for words
//!   written with bit 15 set. The firmware polls it once per main-loop
//!   pass (RAM 0x1bce, every 4 ms), acknowledges by writing it back
//!   without bit 15, and starts the amp envelope's next segment (0x2ab4
//!   -> 0x7f90) or the filter envelope's (0x2b1c). The PCM tones'
//!   envelopes are sequenced by it; without it they advance only on a
//!   slow fallback.
//! - The voice interrupt (IRQ 29): status at port + 0x22 (bits 8-11 = which
//!   of four event words has an event), event words at + 0x24 .. + 0x2a
//!   (bit 15 = valid, bits 0-5 the voice, bit 14 enable; acknowledged by
//!   writing the word back without bit 15). + 0x28 reports a voice that
//!   has reached the end of its wave for the first time since key-on (the
//!   loop end, or the end of a one-shot): the firmware's handler (RAM
//!   0x180a) answers with voice command 0x53 and, through 0x2a56 ->
//!   0x46de, lets an amp envelope waiting in its hold stage go on to the
//!   decay. Raised for voices keyed with register 3 bit 22 clear (bits
//!   23-22 are 00 or 10 on PCM / Hex voices, 11 on Solo Synth voices and
//!   on a PCM voice with a single pitch glide; 10 was first left out, and
//!   GM Rain Drop's falling voice then never decayed). Which bit the chip
//!   looks at is a guess from those cases. + 0x2a leads to the pitch envelope's next step (0x2b84 ->
//!   0x4d92); what the chip reports there, and on + 0x24 / + 0x26, is open
//!   and the model raises nothing on them.
//!
//! - Recording (the Drawbar Organ). A voice whose sample format (register 4
//!   bits 20-23) is 5 writes instead of reading: one 16-bit sample per
//!   output sample at its position in SRAM, from its start to its end,
//!   the sum of the voices whose word 3 is 0xffff0000. The firmware renders
//!   a drawbar setting that way: nine sine partials at the drawbar levels
//!   into 0x1c018a50, 960 samples, of which it then copies 243 into the
//!   buffer its note voices loop over (RAM 0x570c, 0x5652). `RECORD_DIV`
//!   is the mix's scale, not measured yet.
//!
//! - The system chorus (`Chorus`): what the effect bus feeds while the
//!   firmware has the chorus level up (the low byte of the block 0x60
//!   command: 0x80 for tones with Line Select 0, 0 otherwise). Measured on a sine layer (rec/chorus_sine): the wet
//!   signal is the bus inverted and delayed, per channel, by 184 samples plus
//!   a triangle of 0..255 samples (4.30 to 10.25 ms), left and right in
//!   opposite phase, 0.082 Hz per step of the chorus rate (0 = stopped); at
//!   full send it is as loud as the voice before its pan.

pub const SAMPLE_RATE: f64 = 42818.1; // measured from aliasing in rec/notes_saw (C7)
pub const PORT: u32 = 0x1FFF_0000;
pub const SLOTS: u32 = 0x1FFE_0000;
const COUNTER: u32 = 0x1FFE_04A8;
const DSP_RAM: u32 = 0x1FFE_0500;
pub const SRAM_BASE: u32 = 0x1C00_0000;
pub const HIRAM_BASE: u32 = 0x1FFE_8000; // the firmware's upper RAM
const HIRAM_TOP: u32 = 0x1F00_0000;      // voice address bits 24-26 = 7
const ADC_SLOT: usize = 0x4F0;           // the input converter's sample, in the high half
const UNITY: i32 = 0x10000;
const PER_OCTAVE: f64 = 6144.0;
const PACKED_DIV: f64 = 128.0; // packed wave: running sum -> 16-bit sample, before the wave's gain
const EXP_STEP: [u32; 4] = [0, 1, 2, 7]; // byte-difference wave: exponent change by 2-bit code, mod 8
const RECORD_FORMAT: u32 = 5; // register 4 bits 20-23: the voice writes the record mix to memory
const RECORD_SEND: u32 = 0xFFFF_0000; // word 3 of a voice that feeds the recording
const RECORD_DIV: f64 = 1.0; // sum of sample x level / 32768 over the partials -> the 16-bit value stored (the partial tables peak at 32767 / 6). With 1 the 50 presets, through the Rotary, have the Hex presets' calibration (rec/drawpreset_NNN)
const BUS_GAIN: f64 = 2.0; // oscillator mix -> DSP input: matches the hardware distortion effect (rec/dsp)

/// Signed bus sample -> the 16-bit mantissa/exponent word the firmware
/// decodes at 0xeb80: exponent e <= 5 shifts by 2e, above by e + 5.
pub fn encode_bus(value: f64) -> u32 {
    let v = value.clamp(-(0x7FF0_0000u32 as f64), 0x7FF0_0000u32 as f64) as i64;
    for e in 0..16u32 {
        let shift = if e <= 5 { 2 * e } else { e + 5 };
        let m = v >> shift;
        if (-2048..=2047).contains(&m) {
            return e << 12 | (m as u32 & 0xFFF);
        }
    }
    0xF000 | ((v >> 20) as u32 & 0xFFF)
}

/// Ramp target field (word 2 bits 24-30) -> level. The firmware encodes
/// 2 x level as a float (0x1807cf2c, then 0xaa2 keeps 4 mantissa bits).
fn ramp_target(code: u32) -> f64 {
    let (hi, m) = (code >> 4 & 7, code & 15);
    (if hi == 0 { m << 5 } else { (16 + m) << (4 + hi) }) as f64 / 2.0
}

/// Ramp rate field (word 2 bits 16-23) -> level units per sample. The
/// firmware computes 256 x distance / samples (0x7386), encodes it as a
/// float (0x1807cf54) and keeps 4 mantissa bits (0xa20).
fn ramp_rate(code: u32) -> f64 {
    let (hi, m) = (code >> 4 & 15, code & 15);
    (if hi == 0 { (m as u64) << 1 } else { ((16 + m) as u64) << hi }) as f64
}

/// Pitch glide rate (register 4 bits 24-31) -> pitch units per sample: the
/// same float as the level ramp's, over 1024. The firmware encodes
/// 1024 x distance / samples (0x4f5e..0x4fd0; 0xff is "at once"), and the
/// chip moves twice as fast as that reading: the PCM tones that start one
/// of two voices flat and glide it up once (Square Lead2, SS Lead, Gt
/// SynthLead: 100 or 200 cents, codes 0x62 / 0x72 / 0x77) give the
/// hardware's phase between the voices only with this rate (rec/tone_441,
/// 451, 483). The Solo Synth re-aims every 4 ms and its takes do not tell
/// the two rates apart.
fn glide_rate(code: u32) -> f64 {
    ramp_rate(code) / 1024.0
}

/// Per-voice filter, register-file word 0 bits 24-30: a corner code (bits
/// 27-30) and a depth (bits 24-26); the whole field 0 = no filter.
/// - Depth 3..6: a second-order shelf, Butterworth poles at the corner and
///   Butterworth zeros above it, unity at low frequencies and -3 / -6 /
///   -12 / -18 dB at high ones. Solo Synth voices always have depth 6
///   (corners `OSC_FILTER_HZ`, measured with white noise: rec/oscfilter_noise,
///   rec/oscfilter_keyf).
/// - Depth 7: a second-order low-pass, Q about 0.95, -1 dB in the pass
///   band, corners `LOWPASS_HZ`. The Hex Layer / PCM tones use this over
///   most of their cutoff range and depths 3..5 near the top.
/// Depths 3..5 and 7 are from rec/hexfilter (white noise through a Hex
/// layer, 0.02-0.1 dB rms per code for corners 7 and up; below that the
/// fit is limited by the analysis resolution and the corner is 0.72 times
/// the depth-6 one, as it is higher up). Measured there:
/// low-pass corners 0..11 and seven of the shelf codes in `SHELF_HZ`.
/// From rec/hexfilter2 (Saw Lead 1-A at C2, each harmonic over the open
/// setting's): low-pass corners 12..15 (Q 0.945-0.958, -0.98 to -1.00 dB;
/// from corner 7 up the corners are 422.9 Hz x sqrt(2)^n within 1 %) and
/// shelf codes 0x5b, 0x7c, 0x7d.
/// - Depths 2 / 1 / 0: treble boost, the inverse of depths 3 / 4 / 5 at
///   the same corner (+3 / +6 / +12 dB): the firmware's Cutoff path passes
///   "open" and goes on into them. rec/hexfilter3: at corner 0xd (a noise
///   wave) the inverse is within 0.4 dB up to 8 kHz; at corner 0xf (a
///   trumpet wave) it is 0.5 to 2.6 dB too strong around 8 kHz.
///   On a loud saw (Saw Lead 3, corner 0xb, rec/hexfilter2) the hardware
///   overloads instead (-2.7 dB at 2.5 kHz, new content above 12 kHz);
///   that is not modelled.
/// Not measured, extrapolated: the other shelf codes (`SHELF_RATIO` times
/// the depth-6 corner).
/// Word 4 is the share of the unfiltered signal (0x7fff, 0x5a74, 0x3fff,
/// 0x1fff, 0 for the Solo Synth's Filter Gain 0..4; 0 on PCM voices).
#[derive(Default, Clone)]
struct OscFilter {
    code: u32,     // word 0 bits 24-30: corner (4 bits), depth (3 bits)
    enabled: bool, // any of those bits set
    dry: f64, // 1 = filter out of circuit
    b: [f64; 3],
    a: [f64; 2],
    s: [f64; 2],
}

const OSC_FILTER_HZ: [f64; 16] = [143.3, 170.3, 203.2, 243.3, 288.7, 342.0, 408.1, 578.1, 821.6, 1171.3, 1683.4,
                                  2465.1, 3057.7, 3875.9, 6148.9, 8889.7];
const LOWPASS_HZ: [f64; 16] = [103.2, 122.6, 146.3, 177.0, 211.3, 251.7, 299.1, 422.9, 597.6, 844.4, 1193.4, 1688.2, 2396.4, 3383.9, 4766.1, 6701.5];
const LOWPASS_Q: f64 = 0.95;
const LOWPASS_GAIN: f64 = 0.893; // -0.98 dB
const SHELF_GAIN: [f64; 4] = [std::f64::consts::FRAC_1_SQRT_2, 0.5, 0.25, 0.125]; // depth 3..6
/// Pole frequency of the measured shelf codes with depth 3..5.
const SHELF_HZ: [(u32, f64); 10] = [(0x5b, 1930.9), (0x5d, 2396.7), (0x64, 2887.2), (0x65, 2943.5), (0x6b, 3259.0), (0x6c, 3829.2), (0x73, 4621.8),
                                     (0x7b, 6585.6), (0x7c, 7937.3), (0x7d, 8677.3)];
const SHELF_RATIO: [f64; 3] = [0.78, 0.966, 0.967]; // depth 3..5 against depth 6, mean of the measured codes

impl OscFilter {
    fn set_code(&mut self, code: u32) {
        if code == self.code && self.b[0] != 0.0 {
            return;
        }
        self.code = code;
        let (corner, depth) = ((code >> 3 & 15) as usize, code & 7);
        // 1 + c1 z^-1 + c2 z^-2 (times the returned scale) for corner k = tan(pi f / fs)
        let section = |k: f64, q: f64| -> (f64, f64, f64) {
            let n = 1.0 + k / q + k * k;
            (n, 2.0 * (k * k - 1.0) / n, (1.0 - k / q + k * k) / n)
        };
        let warp = |hz: f64| (std::f64::consts::PI * hz / SAMPLE_RATE).tan();
        match depth {
            7 => {
                let k = warp(LOWPASS_HZ[corner]);
                let pole = section(k, LOWPASS_Q);
                let gain = LOWPASS_GAIN * k * k / pole.0;
                self.b = [gain, 2.0 * gain, gain];
                self.a = [pole.1, pole.2];
            }
            _ if code == 0 => {
                self.b = [1.0, 0.0, 0.0];
                self.a = [0.0, 0.0];
            }
            0..=6 => {
                let q = std::f64::consts::FRAC_1_SQRT_2;
                // Depths 2 / 1 / 0 are the inverses of 3 / 4 / 5 at the same corner.
                let cut = if depth < 3 { 5 - depth } else { depth };
                let high = SHELF_GAIN[cut as usize - 3]; // gain above the corner
                let hz = match SHELF_HZ.iter().find(|(c, _)| *c == (corner as u32) << 3 | cut) {
                    Some((_, hz)) => *hz,
                    None if cut == 6 => OSC_FILTER_HZ[corner],
                    None => OSC_FILTER_HZ[corner] * SHELF_RATIO[cut as usize - 3],
                };
                let k = warp(hz);
                let (pole, zero) = (section(k, q), section(k / high.sqrt(), q));
                let gain = (1.0 + pole.1 + pole.2) / (1.0 + zero.1 + zero.2); // unity at DC
                if depth < 3 {
                    self.b = [1.0 / gain, pole.1 / gain, pole.2 / gain];
                    self.a = [zero.1, zero.2];
                } else {
                    self.b = [gain, gain * zero.1, gain * zero.2];
                    self.a = [pole.1, pole.2];
                }
            }
            _ => {
                self.b = [1.0, 0.0, 0.0];
                self.a = [0.0, 0.0];
            }
        }
    }

    fn run(&mut self, x: f64) -> f64 {
        if self.dry >= 1.0 || !self.enabled {
            return x;
        }
        let y = self.b[0] * x + self.s[0];
        self.s = [self.b[1] * x - self.a[0] * y + self.s[1], self.b[2] * x - self.a[1] * y];
        let y = y.clamp(-32768.0, 32767.0);
        self.dry * x + (1.0 - self.dry) * y
    }
}

#[derive(Default, Clone)]
struct Voice {
    reg: [u32; 8],
    on: bool,
    fresh: bool, // keyed on, no sample played yet
    wave_event: bool, // report the first time the wave's end is reached (IRQ 29)
    ended: bool,      // that has happened and is not yet collected
    pos: f64,
    looped: f64,
    end: f64,
    inc: f64,
    pitch: f64,        // current, in register units (0x10000 = unity, 512 per semitone)
    pitch_target: f64,
    pitch_rate: f64,
    level: f64,
    level_flag: u32, // word 2 bit 15: report the end of the ramp (event word + 0x46)
    dry_flag: u32,   // the same for word 4
    ramp: u32,       // word 2 high half as written, bit 15 (bit 31) = still ramping
    target: f64,
    rate: f64,
    base: u32,
    ram: bool,
    filter: OscFilter,
    sum: f64,     // packed / byte-difference waves: decoded value at byte `sum_at`
    sum_at: i64,
    prev: f64,    // byte-difference waves: decoded value at byte `sum_at - 1`
    exp: u32,     // byte-difference waves: current shift, 0..7
    loop_state: [u32; 2], // slots voice * 16 + 4 / + 8: decoder state at the loop start
    dry: f64,       // word 4 low half: share of the unfiltered signal, 0..0x7fff
    dry_ramp: u32,  // word 4 high half, as `ramp`
    dry_target: f64,
    dry_rate: f64,
    mix: [f64; 2],  // word 3: left / right output gain, 0..1
    bus_send: f64,  // word 3 bits 10-15, 0..1
    rev_send: f64,  // word 3 bits 26-31, 0..1
    rec_send: bool, // word 3 = 0xffff0000: feeds a recording voice, not the outputs
}

impl Voice {
    /// Packed waves (register 4 bits 20-23 = 2): 512-byte blocks of a
    /// 16-bit scale followed by 510 signed bytes, each byte the difference
    /// to the next sample in units of the scale; the running sum / 128 is
    /// the 16-bit sample.
    /// Byte-difference waves (code 1; most of the remaining PCM waves): the
    /// GT913 format (ref/mame/gt913.cpp) with one byte per sample. Bits 1-7
    /// of a byte are a signed difference, shifted left by a 3-bit exponent;
    /// bit 0 of the two bytes of a 16-bit word (even address first) is a
    /// 2-bit code that changes the exponent by 0, +1, +2 or -1 before the
    /// word's differences are applied.
    /// For both, the firmware hands the chip the decoder state from the
    /// wave's header: at the start position in registers 5 (exponent, or
    /// two block scales) and 6 (sample << 8), at the loop start in the
    /// voice's slots + 4 and + 8. A state is the one reached after the byte
    /// at position + 1. Other waves are 16-bit.
    fn packed(&self) -> bool {
        self.reg[4] >> 20 & 0xF == 2
    }

    fn byte_diff(&self) -> bool {
        self.reg[4] >> 20 & 0xF == 1
    }

    fn recording(&self) -> bool {
        self.reg[4] >> 20 & 0xF == RECORD_FORMAT
    }

    /// A recording voice's sample: store `value` at the position and move
    /// on; the voice stops at its end.
    fn record(&mut self, sram: &mut [u8], value: f64) {
        self.fresh = false;
        if self.pos >= self.end || !self.ram {
            self.on = false;
            return;
        }
        let p = self.base as i64 - SRAM_BASE as i64 + 2 * self.pos as i64;
        if p >= 0 && p as usize + 1 < sram.len() {
            let v = (value.clamp(-32768.0, 32767.0) as i16).to_le_bytes();
            sram[p as usize] = v[0];
            sram[p as usize + 1] = v[1];
        }
        self.pos += self.inc;
    }

    fn bytes_per_sample(&self) -> f64 {
        if self.packed() || self.byte_diff() { 1.0 } else { 2.0 }
    }

    /// Register -> position in samples relative to `base`.
    fn position(&self, reg: u32) -> f64 {
        let addr = 0x1800_0000 | (self.reg[4] >> 17 & 7) << 24 | reg >> 8;
        let unit = self.bytes_per_sample();
        (addr as i64 - self.base as i64) as f64 / unit + (reg & 0xFF) as f64 / (256.0 * unit)
    }

    fn address_reg(&self) -> u32 {
        let byte = (self.pos * self.bytes_per_sample()) as i64 + self.base as i64;
        ((byte & 0xFF_FFFF) as u32) << 8
    }

    fn set_pitch(&mut self, pitch: f64) {
        self.pitch = pitch;
        // The chip's phase increment has 17 fractional bits, truncated (rec/pitchlow:
        // a wave played at 0.03x is 0.3 cents flat, at 0.25x and above exact).
        self.inc = (2f64.powf((pitch - UNITY as f64) / PER_OCTAVE) * 131072.0).floor() / 131072.0;
    }

    /// One sample of the pitch glide.
    fn step_pitch(&mut self) {
        if self.pitch == self.pitch_target {
            return;
        }
        // Rate 0 is no glide: the Drawbar Organ's render voices write register 4
        // with a pitch field of 0 and rate 0 and play at register 3's pitch.
        if self.pitch_rate == 0.0 {
            return;
        }
        let d = self.pitch_target - self.pitch;
        if d.abs() <= self.pitch_rate {
            self.set_pitch(self.pitch_target);
        } else {
            self.set_pitch(self.pitch + self.pitch_rate.copysign(d));
        }
    }

    /// Register 3 bit 17 = key on, bit 18 = hold: the Solo Synth sets both
    /// while it loads a voice and clears bit 18 (command 0x63 with data
    /// 0x20000) to start it, or sends 0x63 with data 0x40000 to start all
    /// held voices together;
    /// the streaming voices are started with bit 17 alone.
    fn key(&mut self, reg3: u32) {
        let on = reg3 & 0x20000 != 0 && reg3 & 0x40000 == 0;
        if on && !self.on {
            self.ended = false;
            self.wave_event = reg3 >> 22 & 1 == 0;
            self.fresh = true;
            self.on = true;
            self.latch();
        }
        self.on = on;
    }

    /// Take loop points (and, until the first sample, the start position)
    /// from the registers. The streaming voices are keyed on before their
    /// address registers are written.
    fn latch(&mut self) {
        let top = 0x1800_0000 | (self.reg[4] >> 17 & 7) << 24;
        self.ram = !((0x1800_0000..0x1A00_0000).contains(&top) || top == 0x1E00_0000);
        self.base = if self.ram || top == 0x1E00_0000 { top } else { 0x1800_0000 };
        self.looped = self.position(self.reg[0]);
        self.end = self.position(self.reg[1] & !0xFF); // low byte is not a fraction
        if self.fresh {
            self.pos = self.position(self.reg[2]);
            self.sum = 0.0;
            self.sum_at = self.pos as i64 - 1;
        }
    }

    /// One sample of the word 4 ramp (the filter envelope and LFO move the
    /// dry share with the same target / rate encoding as the level). True
    /// when the ramp ends here and the voice asked for that to be reported.
    fn step_dry(&mut self) -> bool {
        if self.dry_ramp & 0x8000 == 0 {
            return false;
        }
        let mut done = false;
        if (self.dry_target - self.dry).abs() <= self.dry_rate {
            self.dry = self.dry_target;
            self.dry_ramp &= 0x7FFF;
            done = self.dry_flag != 0;
        } else {
            self.dry += self.dry_rate.copysign(self.dry_target - self.dry);
        }
        self.filter.dry = self.dry.floor() / 0x7FFF as f64;
        done
    }

    /// One sample of the level ramp. True when the ramp ends here and the
    /// voice asked for that to be reported.
    fn step_level(&mut self) -> bool {
        if self.ramp & 0x8000 == 0 {
            return false;
        }
        if (self.target - self.level).abs() <= self.rate {
            self.level = self.target;
            self.ramp &= 0x7FFF;
            return self.level_flag != 0;
        }
        self.level += self.rate.copysign(self.target - self.level);
        false
    }

    /// Advance by one output sample; returns the 16-bit sample.
    fn sample(&mut self, flash: &[u8], sram: &[u8], hiram: &[u8]) -> f64 {
        // A wave whose loop start is at (or past) its end is a one-shot: it
        // plays from its start position to the end and then stops.
        let one_shot = self.end <= self.looped;
        if self.inc == 0.0 || (one_shot && self.pos >= self.end) {
            if self.inc != 0.0 {
                self.ended = std::mem::take(&mut self.wave_event);
            }
            return 0.0;
        }
        let seed = std::mem::take(&mut self.fresh);
        let i = self.pos as i64;
        let frac = self.pos - i as f64;
        let mut j = i + 1;
        if j as f64 >= self.end {
            j = if one_shot { i } else { self.looped as i64 };
        }
        let (mem, off) = if self.ram {
            if self.base == HIRAM_TOP {
                // the external input's ring, in the firmware's own RAM
                (hiram, HIRAM_TOP as i64 - HIRAM_BASE as i64)
            } else {
                (sram, self.base as i64 - SRAM_BASE as i64)
            }
        } else {
            (flash, if self.base == 0x1E00_0000 { 0x100_0000 } else { 0 })
        };
        if self.byte_diff() {
            let byte = |p: i64| -> u8 {
                let q = off + p;
                if q < 0 || q as usize >= mem.len() { 0 } else { mem[q as usize] }
            };
            let diff = |p: i64, exp: u32| -> f64 { (((byte(p) as i8) >> 1) as i32 * (1 << exp)) as f64 };
            // A stored state (16-bit sample << 8, exponent) holds after the
            // byte at position + 1.
            let state = if seed {
                Some((self.reg[6], self.reg[5], i + 1))
            } else if i + 1 < self.sum_at {
                Some((self.loop_state[1], self.loop_state[0], self.looped as i64 + 1))
            } else {
                None
            };
            if let Some((sample, exp, at)) = state {
                self.sum = ((sample >> 8) as u16 as i16) as f64;
                self.exp = exp & 7;
                self.sum_at = at;
                self.prev = self.sum - diff(at, self.exp);
            }
            while self.sum_at < i + 1 {
                self.sum_at += 1;
                let p = self.sum_at;
                if (off + p) & 1 == 0 {
                    let code = (byte(p) & 1) | (byte(p + 1) & 1) << 1;
                    self.exp = (self.exp + EXP_STEP[code as usize]) & 7;
                }
                self.prev = self.sum;
                self.sum += diff(p, self.exp);
            }
            let gain = (self.reg[1] & 0xFF) as f64 / 32.0;
            let (a, b) = (self.prev * gain, self.sum * gain);
            self.pos += self.inc;
            if self.pos >= self.end && !one_shot {
                self.ended = std::mem::take(&mut self.wave_event);
                self.pos = self.looped + (self.pos - self.end) % (self.end - self.looped);
            }
            return a + (b - a) * frac;
        }
        if self.packed() {
            // one difference byte, or 0 for a block's scale word
            let delta = |p: i64| -> f64 {
                let q = off + p;
                if q & 0x1FF < 2 || q < 0 || q as usize >= mem.len() {
                    return 0.0;
                }
                let block = (q & !0x1FF) as usize;
                (mem[q as usize] as i8) as f64 * u16::from_le_bytes([mem[block], mem[block + 1]]) as f64
            };
            // A stored state is the 16-bit sample << 8 after the byte at
            // position + 1; the running sum is in units of 1 / 128.
            let stored = |v: u32, at: i64| -> f64 { ((v << 8) as i32 >> 8) as f64 / 2.0 - delta(at + 1) };
            let at_loop = stored(self.loop_state[1], self.looped as i64);
            if seed {
                self.sum = stored(self.reg[6], i);
                self.sum_at = i;
            } else if i < self.sum_at {
                self.sum = at_loop;
                self.sum_at = self.looped as i64;
            }
            while self.sum_at < i {
                self.sum_at += 1;
                self.sum += delta(self.sum_at);
            }
            let next = if j > i { self.sum + delta(j) } else { at_loop };
            let gain = (self.reg[1] & 0xFF) as f64 / 32.0 / PACKED_DIV;
            let (a, b) = (self.sum * gain, next * gain);
            self.pos += self.inc;
            if self.pos >= self.end && !one_shot {
                self.ended = std::mem::take(&mut self.wave_event);
                self.pos = self.looped + (self.pos - self.end) % (self.end - self.looped);
            }
            return a + (b - a) * frac;
        }
        let at = |k: i64| -> f64 {
            let p = off + 2 * k;
            if p < 0 || p as usize + 1 >= mem.len() {
                return 0.0;
            }
            i16::from_le_bytes([mem[p as usize], mem[p as usize + 1]]) as f64
        };
        // Register 1's low byte is the wave's gain, 0x20 = unity (the
        // single-cycle synth waves); the long synth waves have 0x10..0x18.
        let gain = (self.reg[1] & 0xFF) as f64 / 32.0;
        let (a, b) = (at(i) * gain, at(j) * gain);
        self.pos += self.inc;
        if self.pos >= self.end && !one_shot {
            self.ended = std::mem::take(&mut self.wave_event);
            self.pos = self.looped + (self.pos - self.end) % (self.end - self.looped);
        }
        a + (b - a) * frac
    }
}

/// The system chorus: one delay line, two taps moving in opposite directions.
struct Chorus {
    line: [f64; 512],
    at: usize,
    phase: f64, // 0..1 through the triangle
}

const CHORUS_BASE: f64 = 184.0;  // samples
const CHORUS_SWEEP: f64 = 255.0; // samples, peak to peak
const CHORUS_HZ_PER_STEP: f64 = 0.082;

impl Chorus {
    fn new() -> Self {
        Chorus { line: [0.0; 512], at: 0, phase: 0.0 }
    }

    fn tap(&self, delay: f64) -> f64 {
        let whole = delay as usize;
        let frac = delay - whole as f64;
        let a = self.line[(self.at + 512 - whole) & 511];
        let b = self.line[(self.at + 511 - whole) & 511];
        a + (b - a) * frac
    }

    /// One sample in, the wet left / right out.
    fn run(&mut self, x: f64, rate_step: u8) -> [f64; 2] {
        self.at = (self.at + 1) & 511;
        self.line[self.at] = x;
        self.phase = (self.phase + rate_step as f64 * CHORUS_HZ_PER_STEP / SAMPLE_RATE).fract();
        let tri = 1.0 - (2.0 * self.phase - 1.0).abs(); // 0..1..0
        [-self.tap(CHORUS_BASE + CHORUS_SWEEP * tri), -self.tap(CHORUS_BASE + CHORUS_SWEEP * (1.0 - tri))]
    }
}

/// What the firmware last told the chip about the system reverb and chorus.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SystemFx {
    pub reverb_type: u8,  // 0 Rectangle, 1 Round
    pub reverb_time: u8,  // 0..15
    pub reverb_level: u8, // 0..254; 32 at the default level 16
    pub chorus_rate: u8,  // 0..15
    pub chorus_level: u8, // 0..0x80: 0 while an effect routine has the bus, 0x80 otherwise
}

/// A register write or command, for tracing (`Sound::log`).
#[derive(Clone, Copy)]
pub struct Event {
    pub sample: u64,
    pub kind: u8, // 0 voice, 1 blk40, 2 blk60, 3 slot
    pub command: u32,
    pub data: u64,
}

pub struct Sound {
    voices: Vec<Voice>,
    wave_library: Option<Arc<WaveLibrary>>,
    wave_morph: Option<WaveMorph>,
    morph_voices: [Option<Voice>; 5],
    morph_note: u8,
    port: [u32; 0x80],
    slots: Vec<u32>, // 0x1ffe0000..DSP_RAM, by byte offset
    file: [u32; 1024],
    pub samples: u64,
    bus_word: u32,
    count_reads: u32,
    pub fx: SystemFx,
    pub bus_peak: f64,
    pub work: [u64; 3], // work RAM accesses: reads of the sample's first address, other reads, writes
    work_first: u32,
    pub bus_gain: f64, // oscillator mix -> DSP input; calibrated on the distortion effect
    pub ramp_div: f64, // ramp rate units per level unit: 512 fits hardware, the firmware computes for 256
    pub log: bool,
    pub events: Vec<Event>,
    chorus: Chorus,
    pub chorus_on: bool, // model the system chorus
    file_done: [u64; 2], // event word + 0x46: voices with a finished level / dry ramp not yet acknowledged
    wave_done: u64, // voices that have reached their wave's end, not yet acknowledged (event word + 0x28)
}

impl Sound {
    pub fn new() -> Self {
        Sound { voices: vec![Voice { filter: OscFilter { dry: 1.0, ..Default::default() }, dry: 0x7FFF as f64, ..Default::default() }; 64], wave_library: None, wave_morph: None, morph_voices: std::array::from_fn(|_| None), morph_note: 60, port: [0; 0x80], slots: vec![0; (DSP_RAM - SLOTS) as usize],
                file: [0; 1024], samples: 0, bus_word: 0, count_reads: 0, fx: SystemFx { reverb_type: 1, reverb_time: 11, reverb_level: 32, chorus_rate: 5, chorus_level: 0 }, bus_peak: 0.0, work: [0; 3], work_first: 0, bus_gain: BUS_GAIN, ramp_div: 512.0, log: false,
                events: Vec::new(), chorus: Chorus::new(), chorus_on: true, file_done: [0; 2], wave_done: 0 }
    }

    pub fn wave_spec(&self, voice: usize, key: u8) -> Option<WaveSpec> {
        let v = self.voices.get(voice)?;
        v.on.then(|| WaveSpec { reg: v.reg[..7].try_into().unwrap(), loop_state: v.loop_state, inc: v.inc, key, track: 1.0 })
    }

    pub fn set_wave_library(&mut self, library: Arc<WaveLibrary>) { self.wave_library = Some(library); }
    pub fn set_morph_note(&mut self, note: u8) { self.morph_note = note; }
    pub fn set_wave_morph(&mut self, value: Option<WaveMorph>) {
        if self.wave_morph.map(|m| (m.a, m.b)) != value.map(|m| (m.a, m.b)) {
            self.morph_voices = std::array::from_fn(|_| None);
        }
        self.wave_morph = value;
    }

    fn event(&mut self, kind: u8, command: u32, data: u64) {
        if self.log {
            self.events.push(Event { sample: self.samples, kind, command, data });
        }
    }

    pub fn read(&mut self, addr: u32) -> u32 {
        if addr >= PORT {
            let off = (addr - PORT) as usize;
            return match off {
                0x06 | 0x0E | 0x44 | 0x60 => self.port[off] & 0x7FFF, // bit 15 = busy
                0x46 => {
                    let which = (self.file_done[0] == 0) as usize;
                    let pending = self.file_done[which];
                    self.port[off] & 0x4000 | if pending != 0 { 0x8000 | pending.trailing_zeros() << 3 | which as u32 } else { 0 }
                }
                0x22 => self.port[off] & !0x0F00 | ((self.wave_done != 0) as u32) << 10,
                0x28 if self.wave_done != 0 => self.port[off] & 0x4000 | 0x8000 | self.wave_done.trailing_zeros(),
                0x24 | 0x26 | 0x28 | 0x2A => self.port[off] & 0x4000,
                _ => self.port[off],
            };
        }
        if addr >= DSP_RAM {
            // Work RAM. The firmware reads the newest bus sample through a
            // pointer it advances itself (0x1ffe8460); every cell returns
            // the current sample, which also satisfies the boot-time check
            // that the DSP has overwritten the 0x55555555 test pattern.
            if self.work_first == 0 {
                self.work_first = addr;
            }
            self.work[(addr != self.work_first) as usize] += 1;
            return self.bus_word;
        }
        let value = self.slots[(addr - SLOTS) as usize];
        if addr == COUNTER {
            self.count_reads += 1;
            return value.wrapping_add(self.count_reads / 4);
        }
        value
    }

    pub fn write(&mut self, addr: u32, value: u32) {
        if addr < PORT {
            if addr >= DSP_RAM {
                self.work[2] += 1;
                return;
            }
            self.slots[(addr - SLOTS) as usize] = value;
            let (voice, word) = ((addr - SLOTS) as usize >> 4, (addr - SLOTS) & 0xF);
            if voice < self.voices.len() && (word == 4 || word == 8) {
                self.voices[voice].loop_state[word as usize / 4 - 1] = value;
            }
            if addr == COUNTER {
                self.count_reads = 0;
            } else {
                self.event(3, addr - SLOTS, value as u64);
            }
            return;
        }
        let off = (addr - PORT) as usize;
        self.port[off] = value;
        match off {
            // acknowledge the event shown: the next voice's comes up
            0x46 if value & 0x8000 == 0 => {
                // acknowledge the event shown: the next one comes up
                let which = (self.file_done[0] == 0) as usize;
                self.file_done[which] &= self.file_done[which].wrapping_sub(1);
            }
            0x28 if value & 0x8000 == 0 => self.wave_done &= self.wave_done.wrapping_sub(1),
            0x06 | 0x0E => {
                let base = off - 6;
                let data = self.port[base] | self.port[base + 2] << 16;
                let voice = self.port[base + 4] & 0xFF;
                self.event(0, value, data as u64 | (voice as u64) << 32);
                self.voice_command(base, value, voice as usize, data);
            }
            0x44 => {
                let data = self.port[0x40] | self.port[0x42] << 16;
                self.event(1, value, data as u64);
                self.file_command(value, data);
            }
            0x60 => {
                // System effects, in one command: bits 8-11 of the command
                // are the chorus rate // 8 and its low byte the chorus level
                // (0 with Line Select = DSP, ramped to 0x80 otherwise); the data
                // word has the reverb level x 2 in bits 0-7, the reverb time // 8
                // in bits 8-11 and the reverb type in bit 18. The firmware ramps
                // the levels to 0 and back around every change.
                let data = self.port[0x62] | self.port[0x64] << 16;
                if value & 0xF000 == 0x1000 {
                    self.fx = SystemFx { reverb_type: (data >> 18 & 1) as u8, reverb_time: (data >> 8 & 15) as u8,
                                         reverb_level: (data & 0xFF) as u8, chorus_rate: (value >> 8 & 15) as u8,
                                         chorus_level: (value & 0xFF) as u8 };
                }
                self.event(2, value, data as u64)
            }
            _ => {}
        }
    }

    fn voice_command(&mut self, base: usize, cmd: u32, number: usize, data: u32) {
        if number >= 64 {
            return;
        }
        let v = &mut self.voices[number];
        let reg = (cmd & 0xFF) as usize;
        if cmd & 0x4000 == 0 {
            // read-back
            let back = match reg {
                0xF3 => v.pitch as u32 & 0x1FFFF, // current pitch
                0xF2 => v.address_reg(),    // current position
                _ => 0,
            };
            self.port[base] = back & 0xFFFF;
            self.port[base + 2] = back >> 16;
            return;
        }
        if reg < 7 {
            v.reg[reg] = data;
            if reg == 4 {
                v.pitch_target = (data & 0x1FFFF) as f64;
                v.pitch_rate = glide_rate(data >> 24);
            }
            if reg == 3 {
                v.set_pitch((data & 0x1FFFF) as f64);
                v.key(data);
            } else if v.on && matches!(reg, 0 | 1 | 2 | 4) {
                v.latch();
            }
        } else if reg == 0x43 || reg == 0x53 {
            // Register 3 without the key acting. The firmware writes back
            // the pitch it has just read (0xf3), so the pitch is left alone
            // (setting it changes nothing in preset 0, the one case traced).
            v.reg[3] = data;
        } else if reg == 0x63 && data & 0x60000 == 0x40000 {
            // Start every voice that is loaded and held (register 3 bits 17
            // and 18 both set) on the same sample: with more than one
            // oscillator on, the firmware loads them all and sends this
            // once, to the first voice.
            for v in &mut self.voices {
                if v.reg[3] & 0x60000 == 0x60000 {
                    v.reg[3] &= !0x40000;
                    v.key(v.reg[3]);
                }
            }
        } else if reg == 0x63 {
            // key bits only
            v.reg[3] = v.reg[3] & !0x60000 | data & 0x60000;
            v.key(v.reg[3]);
        } else if reg == 0x11 {
            v.reg[1] = data;
            v.end = v.position(data & !0xFF);
        }
    }

    fn file_command(&mut self, cmd: u32, data: u32) {
        let key = (cmd & 0x3FF) as usize;
        let (number, reg, page) = (key >> 3, key & 7, cmd >> 10 & 0xF);
        if cmd & 0x4000 == 0 {
            // read: current value, never busy
            let mut back = self.file[key] & 0x7FFF_7FFF;
            if reg == 2 && number < 64 {
                let v = &self.voices[number];
                back = v.ramp << 16 | v.level_flag | v.level as u32 & 0x7FFF;
            }
            if reg == 4 && number < 64 {
                let v = &self.voices[number];
                back = v.dry_ramp << 16 | v.dry as u32 & 0x7FFF;
            }
            self.port[0x40] = back & 0xFFFF;
            self.port[0x42] = back >> 16;
            return;
        }
        if number >= 64 {
            return;
        }
        if reg == 0 && (page == 0 || page == 0xC) {
            // The field is 0 on the streaming voices and on PCM voices with
            // the cutoff fully open, whose word 4 is 0 too: no filter.
            // Bit 31 (set in some PCM releases and at low velocity) is not
            // understood and is ignored.
            self.voices[number].filter.enabled = data >> 24 & 0x7F != 0;
            self.voices[number].filter.set_code(data >> 24 & 0x7F);
        }
        if reg == 3 && page == 0 {
            let v = &mut self.voices[number];
            v.mix = [(data & 0x3FF) as f64 / 1024.0, (data >> 16 & 0x3FF) as f64 / 1024.0];
            v.bus_send = (data >> 10 & 0x3F) as f64 / 63.0;
            v.rev_send = (data >> 26) as f64 / 63.0;
            v.rec_send = data == RECORD_SEND;
        }
        if reg == 4 {
            // Same layout as word 2: page 0 writes the value, pages 0 and
            // 0xd the ramp (bit 31 running, target, rate).
            let v = &mut self.voices[number];
            if page == 0 {
                v.dry = (data & 0x7FFF) as f64;
                v.filter.dry = v.dry / 0x7FFF as f64;
            }
            if page == 0 || page == 0xD || page == 7 {
                v.dry_flag = data & 0x8000;
            }
            if page == 0 || page == 0xD {
                v.dry_ramp = data >> 16;
                v.dry_target = ramp_target(data >> 24);
                v.dry_rate = ramp_rate(data >> 16) / self.ramp_div;
            }
            if page == 7 {
                v.dry_ramp = v.dry_ramp & 0x7FFF | data >> 16 & 0x8000;
            }
        }
        if reg == 2 {
            // Page 0 writes both halves, page 0xd only the high half (the
            // firmware passes the level it read back in the low half),
            // page 7 only the two flag bits.
            let v = &mut self.voices[number];
            if page == 0 {
                v.level = (data & 0x7FFF) as f64;
                v.level_flag = data & 0x8000;
            }
            if page == 0xD {
                v.level_flag = data & 0x8000; // set for the segments the firmware waits on, clear for its timed steps
            }
            if page == 0 || page == 0xD {
                // A write without bit 31 stops a running ramp where it is
                // (the firmware does this every tick the target stays the
                // same); keeping it running makes tremolo 14 dB too deep.
                v.ramp = data >> 16;
                v.target = ramp_target(data >> 24);
                v.rate = ramp_rate(data >> 16) / self.ramp_div;
            }
            if page == 7 {
                // Flags only: bit 31 (ramping) and bit 15. A short attack is
                // written as level 0 plus target and rate through page 0
                // with bit 31 clear, then started with this.
                v.ramp = v.ramp & 0x7FFF | data >> 16 & 0x8000;
                v.level_flag = data & 0x8000;
            }
        }
        if page == 0 {
            self.file[key] = data;
        }
    }

    /// The voice interrupt line (IRQ 29): an event is waiting and its word is enabled.
    pub fn irq(&self) -> bool {
        self.wave_done != 0 && self.port[0x28] & 0x4000 != 0
    }

    /// A port word as last written (for inspection).
    pub fn port_word(&self, off: usize) -> u32 {
        self.port[off & 0x7F]
    }

    /// The stored wave of every voice playing a wave (from flash, or one the
    /// firmware rendered into RAM: the PWM waves), for the panel's wave
    /// previews (tools/panel_waves.py): (voice, start address, phase
    /// increment now, index of the loop start or `samples.len()` for a
    /// one-shot, samples from the start position to the end at the wave's
    /// own rate, with the wave's gain). At most `limit` samples.
    pub fn wave_shots(&self, flash: &[u8], sram: &[u8], limit: usize) -> Vec<(usize, u32, f64, usize, Vec<i16>)> {
        let mut shots = Vec::new();
        for (n, v) in self.voices.iter().enumerate() {
            if !v.on || n >= 0x3E || v.base == HIRAM_TOP || v.recording() || v.inc == 0.0 {
                continue;
            }
            let mut w = v.clone();
            w.fresh = true;
            w.latch();
            w.inc = 1.0;
            let start = w.pos;
            let count = ((w.end - start).max(0.0) as usize).min(limit);
            let samples = (0..count).map(|_| w.sample(flash, sram, &[]).clamp(-32768.0, 32767.0) as i16).collect();
            let looped = if w.end <= w.looped { count } else { ((w.looped - start).max(0.0) as usize).min(count) };
            shots.push((n, (v.base as i64 + (start * v.bytes_per_sample()) as i64) as u32, v.inc, looped, samples));
        }
        shots
    }

    /// The instrument / mic input's converter value for this sample
    /// (the firmware's per-sample routine at 0x8b84 reads it).
    pub fn set_input(&mut self, sample: i16) {
        self.slots[ADC_SLOT] = (sample as u16 as u32) << 16;
    }

    /// One output sample: feed the Solo Synth bus to the firmware and
    /// return what the streaming voices play back as [left, right,
    /// reverb send], full scale = 1.
    pub fn tick(&mut self, flash: &[u8], sram: &mut [u8], hiram: &[u8]) -> [f64; 3] {
        self.samples += 1;
        let mut bus = 0.0;
        for (n, v) in self.voices.iter_mut().enumerate() {
            if v.step_level() {
                self.file_done[0] |= 1 << n;
            }
            if v.step_dry() {
                self.file_done[1] |= 1 << n;
            }
            if std::mem::take(&mut v.ended) {
                self.wave_done |= 1 << n;
            }
            v.step_pitch();
        }
        // Every keyed voice goes to the outputs and the reverb by its own
        // mix, and to the effect bus by its send. Voices routed to an effect
        // routine (Solo Synth oscillators, Hex Layer with Line Select = DSP)
        // have no output level; the routine's result comes back through
        // the streaming voices 0x3e / 0x3f, whose bus send is 0.
        if self.voices.iter().any(|v| v.on && v.recording()) {
            let mut mix = 0.0;
            for v in &mut self.voices {
                if v.on && v.rec_send && !v.recording() {
                    mix += v.sample(flash, sram, hiram) * v.level.floor() / 32768.0;
                }
            }
            for v in &mut self.voices {
                if v.on && v.recording() {
                    v.record(sram, mix / RECORD_DIV);
                }
            }
        }
        let mut out = [0.0; 3];
        let morph = self.wave_morph;
        let library = self.wave_library.as_deref();
        let note = self.morph_note;
        for (n, v) in self.voices.iter_mut().enumerate() {
            if !v.on || v.rec_send || v.recording() || (v.bus_send == 0.0 && v.mix == [0.0, 0.0] && v.rev_send == 0.0) {
                continue;
            }
            // Solo Synth voices occupy 0x33..0x37. Keep a second wave decoder
            // for each one, so PCM attacks and loops advance at their own rates.
            let blend = if let (Some(m), Some(lib), 0x33..=0x37) = (morph, library, n) {
                // Chip order: Synth 1, Synth 2, Noise, PCM 1, PCM 2.
                let i = [0, 1, 4, 2, 3][n - 0x33];
                let block = if i < 2 { "synth" } else if i < 4 { "pcm" } else { "noise" };
                if m.a[i] != m.b[i] {
                    match (lib.get(block, m.a[i], note), lib.get(block, m.b[i], note)) {
                        (Some(a), Some(b)) if a.inc > 0.0 && v.reg[0] == a.reg[0] && v.reg[1] & !0xFF == a.reg[1] & !0xFF => {
                            if v.fresh || self.morph_voices[i].is_none() {
                                self.morph_voices[i] = Some(morph_voice(v, a, b, note));
                            }
                            let other = self.morph_voices[i].as_mut().unwrap();
                            other.inc = v.inc * b.increment_at(note) / a.increment_at(note).max(1e-9);
                            Some((other.sample(flash, sram, hiram), m.amount as f64 / 100.0))
                        }
                        _ => { self.morph_voices[i] = None; None },
                    }
                } else { None }
            } else { None };
            let original = v.sample(flash, sram, hiram);
            let s = blend.map_or(original, |(other, t)| original * (1.0 - t) + other * t);
            let s = v.filter.run(s) * v.level.floor();
            bus += s * v.bus_send;
            let s = s / (32768.0 * 32768.0);
            out[0] += s * v.mix[0];
            out[1] += s * v.mix[1];
            out[2] += s * v.rev_send;
        }
        bus *= self.bus_gain; // 16-bit sample * 15-bit level = 31 bits at gain 1
        if bus.abs() > self.bus_peak {
            self.bus_peak = bus.abs();
        }
        // The system chorus, when the firmware has its level up (no effect routine on the bus).
        if self.chorus_on && self.fx.chorus_level > 0 {
            let wet = self.chorus.run(bus / self.bus_gain / (32768.0 * 32768.0), self.fx.chorus_rate);
            let gain = self.fx.chorus_level as f64 / 128.0;
            out[0] += wet[0] * gain;
            out[1] += wet[1] * gain;
        }
        self.bus_word = encode_bus(bus);
        self.work_first = 0;
        out
    }
}
use std::sync::Arc;
use serde::{Deserialize, Serialize};

/// One firmware-selected wave, including the decoder seed for compressed PCM.
#[derive(Clone, Serialize, Deserialize)]
pub struct WaveSpec {
    pub reg: [u32; 7],
    pub loop_state: [u32; 2],
    pub inc: f64,
    pub key: u8,
    #[serde(default)]
    pub track: f64,
}

impl WaveSpec {
    fn increment_at(&self, key: u8) -> f64 {
        self.inc * 2f64.powf((key as f64 - self.key as f64) * self.track / 12.0)
    }
}

/// The optional wave index made alongside the panel's previews.
#[derive(Serialize, Deserialize)]
pub struct WaveLibrary {
    pub blocks: std::collections::HashMap<String, Vec<Vec<[usize; 2]>>>,
    pub shots: Vec<WaveSpec>,
}

impl WaveLibrary {
    fn get(&self, block: &str, wave: u16, key: u8) -> Option<&WaveSpec> {
        let splits = self.blocks.get(block)?.get(wave as usize)?;
        let index = splits.iter().rev().find(|s| s[0] <= key as usize).or_else(|| splits.first())?[1];
        self.shots.get(index)
    }
}

fn morph_voice(a: &Voice, source: &WaveSpec, target: &WaveSpec, key: u8) -> Voice {
    let mut b = a.clone();
    b.reg[..7].copy_from_slice(&target.reg);
    b.loop_state = target.loop_state;
    b.fresh = true;
    b.latch();
    let a_start = a.position(a.reg[2]);
    let b_start = b.pos;
    if !b.byte_diff() && !b.packed() && a.pos >= a.looped && a.end > a.looped && b.end > b.looped {
        let phase = (a.pos - a.looped) / (a.end - a.looped);
        b.pos = b.looped + phase.fract() * (b.end - b.looped);
    } else if !b.byte_diff() && !b.packed() && a.looped > a_start && b.looped > b_start {
        let phase = ((a.pos - a_start) / (a.looped - a_start)).clamp(0.0, 1.0);
        b.pos = b_start + phase * (b.looped - b_start);
    }
    b.inc = a.inc * target.increment_at(key) / source.increment_at(key).max(1e-9);
    b
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct WaveMorph {
    pub amount: u8,
    pub a: [u16; 5],
    pub b: [u16; 5],
}

#[cfg(test)]
mod wave_morph_tests {
    use super::*;

    #[test]
    fn blends_different_wave_samples_before_the_oscillator_filter() {
        let mut sound = Sound::new();
        let mut flash = vec![0u8; 32];
        for (offset, sample) in [(0, 1000i16), (16, 3000i16)] {
            for i in 0..4 { flash[offset + i * 2..offset + i * 2 + 2].copy_from_slice(&sample.to_le_bytes()); }
        }
        let spec = |offset: u32| WaveSpec {
            reg: [offset << 8, ((offset + 8) << 8) | 32, offset << 8, 0, 0, 0, 0],
            loop_state: [0; 2], inc: 1.0, key: 60, track: 1.0,
        };
        let a = spec(0);
        let b = spec(16);
        let mut blocks = std::collections::HashMap::new();
        blocks.insert("synth".into(), vec![vec![[0, 0]], vec![[0, 1]]]);
        sound.set_wave_library(Arc::new(WaveLibrary { blocks, shots: vec![a.clone(), b] }));
        let voice = &mut sound.voices[0x33];
        voice.reg[..7].copy_from_slice(&a.reg);
        voice.on = true;
        voice.fresh = true;
        voice.latch();
        voice.set_pitch(UNITY as f64);
        voice.level = 32768.0;
        voice.mix = [1.0, 1.0];
        sound.set_wave_morph(Some(WaveMorph { amount: 25, a: [0; 5], b: [1, 0, 0, 0, 0] }));
        let sample = sound.tick(&flash, &mut [], &[])[0];
        assert!((sample - 1500.0 / 32768.0).abs() < 1e-6, "{sample}");
    }
}
