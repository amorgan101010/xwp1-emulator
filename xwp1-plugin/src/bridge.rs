//! Between the engine's thread and the host's audio callback: MIDI one
//! way, stamped with the instrument sample it is due at, and the sound the
//! other way, read at any position of the instrument's sample stream (a
//! windowed-sinc interpolator), since the host's rate is not 42818 Hz.

use std::time::{Duration, Instant};

use xwp1::sound::SAMPLE_RATE;

#[derive(Clone, Copy)]
pub struct Event {
    pub due: u64, // instrument sample
    pub len: u8,
    pub data: [u8; 3],
}

const TAPS: usize = 24;
const HALF: i64 = TAPS as i64 / 2;
const PHASES: usize = 256;

pub struct Reader {
    ring: rtrb::Consumer<f32>, // left, right per sample, from sample 0 on
    buf: Vec<f32>,
    base: i64, // the sample `buf` starts at
    step: f64, // instrument samples per host frame
    table: Vec<f32>, // PHASES + 1 rows of TAPS
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)) * (x / (2.0 * k));
        sum += term;
        k += 1.0;
    }
    sum
}

impl Reader {
    pub fn new(ring: rtrb::Consumer<f32>, step: f64) -> Reader {
        // Up to the instrument's own band; below it when the host's rate is the lower one.
        let cutoff = 0.96 * (1.0 / step).min(1.0);
        let beta = 8.0;
        let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
        for phase in 0..=PHASES {
            let frac = phase as f64 / PHASES as f64;
            let row: Vec<f64> = (0..TAPS).map(|t| {
                let x = t as f64 - (HALF - 1) as f64 - frac;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * cutoff * x) };
                let w = (x / HALF as f64).clamp(-1.0, 1.0);
                cutoff * sinc * bessel_i0(beta * (1.0 - w * w).sqrt()) / bessel_i0(beta)
            }).collect();
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|h| (h / sum) as f32));
        }
        Reader { ring, buf: Vec::with_capacity(1 << 18), base: 0, step, table }
    }

    fn pull(&mut self) {
        let room = (self.buf.capacity() - self.buf.len()) & !1; // never grows: this is the audio callback
        for _ in 0..(self.ring.slots() & !1).min(room) {
            self.buf.push(self.ring.pop().unwrap_or(0.0));
        }
    }

    fn have(&self) -> i64 {
        self.base + (self.buf.len() / 2) as i64
    }

    /// Fill `out` (left, right) with the stream from position `pos` on,
    /// one `step` per frame. Samples before the start are silence. What
    /// the engine has not made yet is silence too when playing live (the
    /// first number returned: frames short), or waited for when
    /// `offline`. If it is more than a tenth of a second behind, the
    /// caller is to set its clock back by the second number, which is
    /// applied here already.
    pub fn read(&mut self, pos: f64, out: &mut [&mut [f32]], offline: bool) -> (u64, f64) {
        self.pull();
        let frames = out.first().map_or(0, |ch| ch.len());
        if frames == 0 || out.len() < 2 {
            return (0, 0.0);
        }
        let need = (pos + (frames - 1) as f64 * self.step).floor() as i64 + HALF + 1;
        let mut slip = 0.0;
        if self.have() < need {
            if offline {
                let start = Instant::now();
                while self.have() < need && start.elapsed() < Duration::from_secs(5) {
                    std::thread::sleep(Duration::from_micros(200));
                    self.pull();
                }
            } else if (need - self.have()) as f64 > 0.1 * SAMPLE_RATE {
                slip = (need - self.have()) as f64;
            }
        }
        let (pos, have, mut short) = (pos - slip, self.have(), 0);
        let (left, right) = out.split_at_mut(1);
        for f in 0..frames {
            let p = pos + f as f64 * self.step;
            let i = p.floor();
            let phase = (p - i) * PHASES as f64;
            let (row, mix) = (phase as usize, (phase - phase.floor()) as f32);
            let first = i as i64 - (HALF - 1);
            if first + TAPS as i64 > have {
                short += 1;
            }
            let (mut l, mut r) = (0.0f32, 0.0f32);
            if first >= self.base && first + TAPS as i64 <= have {
                let at = 2 * (first - self.base) as usize;
                let (a, b) = (&self.table[row * TAPS..][..TAPS], &self.table[(row + 1) * TAPS..][..TAPS]);
                for (t, pair) in self.buf[at..at + 2 * TAPS].chunks_exact(2).enumerate() {
                    let h = a[t] + (b[t] - a[t]) * mix;
                    l += h * pair[0];
                    r += h * pair[1];
                }
            } else {
                for t in 0..TAPS {
                    let idx = first + t as i64;
                    if idx >= self.base && idx < have {
                        let at = 2 * (idx - self.base) as usize;
                        let h = self.table[row * TAPS + t] + (self.table[(row + 1) * TAPS + t] - self.table[row * TAPS + t]) * mix;
                        l += h * self.buf[at];
                        r += h * self.buf[at + 1];
                    }
                }
            }
            left[0][f] = l;
            right[0][f] = r;
        }
        // what the next call can still need
        let keep = (pos + frames as f64 * self.step).floor() as i64 - HALF;
        if keep > self.base {
            let drop = ((keep - self.base) as usize * 2).min(self.buf.len());
            self.buf.drain(..drop);
            self.base = keep.min(self.base + (drop / 2) as i64).max(keep);
        }
        (short, slip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine through the interpolator at 48 kHz, read in host-sized blocks, against the sine itself.
    #[test]
    fn sine_is_kept() {
        for hz in [220.0, 1000.0, 8000.0, 15000.0] {
            let (mut tx, rx) = rtrb::RingBuffer::new(200_000);
            let step = SAMPLE_RATE / 48000.0;
            let mut reader = Reader::new(rx, step);
            let wave = |n: f64| (2.0 * std::f64::consts::PI * hz * n / SAMPLE_RATE).sin() as f32;
            for n in 0..40_000 {
                tx.push(wave(n as f64)).unwrap();
                tx.push(-wave(n as f64)).unwrap();
            }
            let (mut pos, mut worst) = (-300.5, 0.0f32);
            for _ in 0..60 {
                let (mut l, mut r) = (vec![0.0f32; 480], vec![0.0f32; 480]);
                let (short, slip) = reader.read(pos, &mut [&mut l[..], &mut r[..]], false);
                assert_eq!((short, slip), (0, 0.0));
                for f in 0..480 {
                    let p = pos + f as f64 * step;
                    let want = if p > 40.0 { wave(p) } else { continue };
                    worst = worst.max((l[f] - want).abs()).max((r[f] + want).abs());
                }
                pos += 480.0 * step;
            }
            assert!(worst < 0.01, "{hz} Hz: error {worst}");
        }
    }

    #[test]
    fn late_engine_is_silence_then_slip() {
        let (mut tx, rx) = rtrb::RingBuffer::new(200_000);
        let mut reader = Reader::new(rx, 1.0);
        for _ in 0..2 * 1000 {
            tx.push(1.0).unwrap();
        }
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let (short, slip) = reader.read(900.0, &mut [&mut l[..], &mut r[..]], false);
        assert!(short > 100 && slip == 0.0 && (l[0] - 1.0).abs() < 1e-3 && l[255] == 0.0);
        let (_, slip) = reader.read(20_000.0, &mut [&mut l[..], &mut r[..]], false);
        assert!(slip > 0.1 * SAMPLE_RATE);
    }
}
