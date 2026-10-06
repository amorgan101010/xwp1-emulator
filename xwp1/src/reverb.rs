//! System reverb. The chip's own algorithm is fixed inside the sound source
//! and was not read, so this convolves the reverb send with an impulse
//! response measured from the hardware: one for each reverb type and each
//! of the 16 time codes the firmware can set (`ReverbBank`;
//! tools/reverb_ir.py writes data/reverb/), scaled by the reverb level. With
//! no data it falls back to a fit of the default setting (`new`): noise
//! that starts after 61 ms, builds up with a 27 ms time constant and decays
//! with RT60 about 1.35 s, faster above about 6 kHz, independent in each
//! channel (the hardware's two channels are uncorrelated).
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;

const BLOCK: usize = 64;
const TAIL_BLOCK: usize = 2048;
const PRE_DELAY: f64 = 0.0609;
const RISE: f64 = 0.0268;
const RT60: f64 = 1.35;
const RT60_HIGH: f64 = 0.65; // above the 6 kHz split
const SPLIT_HZ: f64 = 6000.0;
const LENGTH: f64 = 1.5; // seconds of tail kept (-66 dB)
/// Envelope gain of the summed L + R response, per unit of reverb send.
const GAIN: f64 = 0.0093;

/// Uniformly partitioned convolution with blocks of `block` samples; the
/// output lags the input by one block.
struct Convolver {
    block: usize,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    ir: [Vec<Vec<Complex32>>; 2], // per channel, per partition: spectrum of 2 x block
    history: Vec<Vec<Complex32>>, // input spectra, newest at `head`
    head: usize,
    input: Vec<f32>, // previous block, then the one being filled
    fill: usize,
    output: [Vec<f32>; 2],
    acc: Vec<Complex32>,
}

impl Convolver {
    fn new(block: usize, left: &[f32], right: &[f32]) -> Convolver {
        let mut planner = FftPlanner::new();
        let (fft, ifft) = (planner.plan_fft_forward(2 * block), planner.plan_fft_inverse(2 * block));
        let ir = [left, right].map(|taps| {
            taps.chunks(block)
                .map(|part| {
                    let mut buf = vec![Complex32::default(); 2 * block];
                    for (b, &x) in buf.iter_mut().zip(part) {
                        b.re = x;
                    }
                    fft.process(&mut buf);
                    buf
                })
                .collect::<Vec<_>>()
        });
        let parts = ir[0].len().max(ir[1].len()).max(1);
        Convolver { block, fft, ifft, ir, history: vec![vec![Complex32::default(); 2 * block]; parts], head: 0,
                    input: vec![0.0; 2 * block], fill: 0, output: [vec![0.0; block], vec![0.0; block]],
                    acc: vec![Complex32::default(); 2 * block] }
    }

    fn tick(&mut self, x: f32) -> [f32; 2] {
        let out = [self.output[0][self.fill], self.output[1][self.fill]];
        self.input[self.block + self.fill] = x;
        self.fill += 1;
        if self.fill == self.block {
            self.fill = 0;
            self.run();
        }
        out
    }

    /// The spectrum of the last two input blocks joins a line of past
    /// spectra, each multiplied by the matching partition of the response.
    fn run(&mut self) {
        let (block, parts) = (self.block, self.history.len());
        self.head = (self.head + parts - 1) % parts;
        let slot = &mut self.history[self.head];
        for (s, &x) in slot.iter_mut().zip(&self.input) {
            *s = Complex32::new(x, 0.0);
        }
        self.fft.process(slot);
        self.input.copy_within(block.., 0);
        for ch in 0..2 {
            self.acc.fill(Complex32::default());
            for (k, part) in self.ir[ch].iter().enumerate() {
                let past = &self.history[(self.head + k) % parts];
                for ((a, p), h) in self.acc.iter_mut().zip(past).zip(part) {
                    *a += p * h;
                }
            }
            self.ifft.process(&mut self.acc);
            for (o, a) in self.output[ch].iter_mut().zip(&self.acc[block..]) {
                *o = a.re / (2 * block) as f32;
            }
        }
    }
}

/// The response is split in two: its first `TAIL_BLOCK` samples run in
/// 64-sample blocks (short delay), the rest in `TAIL_BLOCK`-sample blocks,
/// whose one block of delay is exactly where that part starts. A 6.5 s
/// response then costs less than 1.5 s did in 64-sample blocks alone.
pub struct Reverb {
    head: Convolver,
    tail: Option<Convolver>,
    pub gain: f32, // the system reverb's level (1 = level 16, at which the responses were measured)
}

impl Reverb {
    pub fn new(sample_rate: f64) -> Reverb {
        let len = ((LENGTH * sample_rate) as usize).div_ceil(BLOCK) * BLOCK;
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut noise = move || {
            // sum of four uniforms: close enough to Gaussian, unit variance
            let mut s = 0.0;
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                s += (seed >> 40) as f64 / (1u64 << 24) as f64 - 0.5;
            }
            s * (12.0f64 / 4.0).sqrt()
        };
        // One block of the pre-delay is already spent inside the convolver.
        let start = PRE_DELAY - BLOCK as f64 / sample_rate;
        let pole = (-2.0 * std::f64::consts::PI * SPLIT_HZ / sample_rate).exp();
        let taps = [0, 1].map(|_| {
            let mut low = 0.0;
            (0..len)
                .map(|i| {
                    let t = i as f64 / sample_rate - start;
                    let w = noise();
                    low = low * pole + w * (1.0 - pole);
                    if t < 0.0 {
                        return 0.0;
                    }
                    let rise = 1.0 - (-t / RISE).exp();
                    let decay = |rt: f64| (-t * 6.908 / rt).exp();
                    // each channel carries half the power of the summed response
                    ((low * decay(RT60) + (w - low) * decay(RT60_HIGH)) * rise * GAIN / 2f64.sqrt()) as f32
                })
                .collect::<Vec<f32>>()
        });
        Self::build(&taps[0], &taps[1])
    }

    /// From a measured response: left / right interleaved, at the sound
    /// source's sample rate, per unit of reverb send.
    pub fn from_response(interleaved: &[f32]) -> Reverb {
        // One block of delay is spent inside the convolver; the response
        // is silent for longer than that at its start.
        let skip = BLOCK.min(interleaved.len() / 2);
        let ch = |c: usize| interleaved.chunks_exact(2).skip(skip).map(|f| f[c]).collect::<Vec<f32>>();
        Self::build(&ch(0), &ch(1))
    }

    /// `left` / `right`: the response from its sample BLOCK on.
    fn build(left: &[f32], right: &[f32]) -> Reverb {
        let split = (TAIL_BLOCK - BLOCK).min(left.len()).min(right.len());
        let head = Convolver::new(BLOCK, &left[..split], &right[..split]);
        let tail = (left.len() > split || right.len() > split)
            .then(|| Convolver::new(TAIL_BLOCK, &left[split.min(left.len())..], &right[split.min(right.len())..]));
        Reverb { head, tail, gain: 1.0 }
    }

    /// One sample of reverb send in, one stereo sample of reverb out.
    pub fn tick(&mut self, x: f32) -> [f32; 2] {
        let a = self.head.tick(x);
        let b = self.tail.as_mut().map_or([0.0; 2], |t| t.tick(x));
        [(a[0] + b[0]) * self.gain, (a[1] + b[1]) * self.gain]
    }
}

/// The measured responses, one per reverb type (0 Rectangle, 1 Round) and
/// time code (0..15): files TYPE_CC.f32 in a directory (tools/reverb_ir.py).
pub struct ReverbBank {
    dir: std::path::PathBuf,
}

impl ReverbBank {
    pub fn open(dir: &std::path::Path) -> Option<ReverbBank> {
        dir.join("1_11.f32").exists().then(|| ReverbBank { dir: dir.to_path_buf() })
    }

    pub fn reverb(&self, kind: u8, code: u8) -> Option<Reverb> {
        let data = std::fs::read(self.dir.join(format!("{kind}_{code:02}.f32"))).ok()?;
        let taps: Vec<f32> = data.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        Some(Reverb::from_response(&taps))
    }
}
