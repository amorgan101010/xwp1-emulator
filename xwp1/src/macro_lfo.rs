//! Per-instance Solo Synth macro modulation. The page supplies the two end
//! states as parameter wire values; the allocator chooses a position for each
//! note. Writes go to that instance's tone edit buffer before its key event.
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::machine::Machine;
use crate::sound::{WaveMorph, SAMPLE_RATE};

#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    pub mode: String,
    pub shape: String,
    pub rate: f64,
    pub depth: f64,
    pub center: f64,
    pub points: Vec<[i64; 7]>, // pid, instance, array index, A wire, B wire, unmodulated wire, stepped
    pub wave: Option<WaveEnds>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct WaveEnds { pub a: [u16; 5], pub b: [u16; 5] }

#[derive(Clone, Copy)]
struct Cell { pid: i64, inst: i64, ai: i64, addr: u32, kind: i64, a: i64, b: i64 }

fn cells() -> &'static Vec<Cell> {
    static CELLS: OnceLock<Vec<Cell>> = OnceLock::new();
    CELLS.get_or_init(|| {
        let data: serde_json::Value = serde_json::from_str(include_str!("../assets/mem.json")).expect("solo map");
        data["cells"].as_array().unwrap().iter().filter_map(|v| {
            let row: Vec<i64> = v.as_array()?.iter().map(|x| x.as_i64()).collect::<Option<_>>()?;
            (row.len() == 8 && row[0] == 9).then(|| Cell {
                pid: row[1], inst: row[2], ai: row[3], addr: row[4] as u32,
                kind: row[5], a: row[6], b: row[7],
            })
        }).collect()
    })
}

impl Config {
    pub fn valid(&self) -> bool {
        matches!(self.mode.as_str(), "hold" | "free" | "trig" | "one" | "half")
            && matches!(self.shape.as_str(), "sine" | "triangle" | "saw" | "ramp" | "exp" | "square" | "random")
            && self.rate.is_finite() && (0.01..=40.0).contains(&self.rate)
            && self.depth.is_finite() && (-100.0..=100.0).contains(&self.depth)
            && self.center.is_finite() && (0.0..=100.0).contains(&self.center)
            && self.points.len() <= 500
            && self.points.iter().all(|p| (0..=65535).contains(&p[3]) && (0..=65535).contains(&p[4])
                && (0..=65535).contains(&p[5]) && (0..=1).contains(&p[6])
                && cells().iter().any(|c| c.pid == p[0] && c.inst == p[1] && c.ai == p[2]))
    }

    pub fn position(&self, samples: u64) -> f64 {
        let cycles = samples as f64 * self.rate / SAMPLE_RATE;
        let cycles = match self.mode.as_str() {
            "one" => cycles.min(1.0 - f64::EPSILON),
            "half" => cycles.min(0.5),
            _ => cycles,
        };
        let phase = cycles.fract();
        let wave = match self.shape.as_str() {
            "sine" => (phase * std::f64::consts::TAU).sin(),
            "triangle" => 1.0 - 4.0 * (phase - 0.5).abs(),
            "saw" => 2.0 * phase - 1.0,
            "ramp" => 1.0 - 2.0 * phase,
            "exp" => 2.0 * phase.powi(3) - 1.0,
            "square" => if phase < 0.5 { 1.0 } else { -1.0 },
            "random" => {
                let mut x = cycles.floor() as u64 ^ 0x9E37_79B9_7F4A_7C15;
                x ^= x >> 30; x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                x ^= x >> 27; x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
                x ^= x >> 31;
                2.0 * (x >> 11) as f64 / (1u64 << 53) as f64 - 1.0
            }
            _ => 0.0,
        };
        (self.center + self.depth * wave * 0.5).clamp(0.0, 100.0)
    }

    pub fn apply(&self, m: &mut Machine, position: f64) {
        self.write_points(m, Some(position));
        if let Some(w) = &self.wave {
            m.devices().sound.set_wave_morph(Some(WaveMorph { amount: position.round() as u8, a: w.a, b: w.b }));
        }
    }

    pub fn restore(&self, m: &mut Machine) {
        self.write_points(m, None);
    }

    fn write_points(&self, m: &mut Machine, position: Option<f64>) {
        let t = position.map(|p| (p / 100.0).clamp(0.0, 1.0));
        for p in &self.points {
            let Some(c) = cells().iter().find(|c| c.pid == p[0] && c.inst == p[1] && c.ai == p[2]) else { continue };
            let wire = match t {
                None => p[5],
                Some(t) if p[6] == 1 => if t < 0.5 { p[3] } else { p[4] },
                Some(t) => (p[3] as f64 + (p[4] - p[3]) as f64 * t).round() as i64,
            };
            let Some(mut bytes) = m.tone_pair(c.addr) else { continue; };
            match c.kind {
                0 => bytes[0] = (wire - c.b).clamp(0, 255) as u8,
                2 | 3 => bytes = ((wire - c.b).clamp(0, 65535) as u16).to_le_bytes(),
                4 => {
                    let mask = (c.b as u8) << c.a;
                    bytes[0] = (bytes[0] & !mask) | (((wire as u8) & c.b as u8) << c.a);
                }
                5 => {
                    let mask = (c.b as u16) << c.a;
                    let word = u16::from_le_bytes(bytes);
                    bytes = ((word & !mask) | (((wire as u16) & c.b as u16) << c.a)).to_le_bytes();
                }
                _ => continue,
            }
            let _ = m.poke_tone(c.addr, &bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_phase_continues_between_notes() {
        let c = Config { mode: "hold".into(), shape: "saw".into(), rate: 1.0, depth: 100.0,
            center: 50.0, points: vec![], wave: None };
        assert!(c.position(0) < 1.0);
        assert!((c.position((SAMPLE_RATE / 2.0) as u64) - 50.0).abs() < 0.1);
        assert!(c.position((SAMPLE_RATE * 0.75) as u64) > 74.0);
    }

    #[test]
    fn validation_rejects_invalid_wire_values_and_unknown_parameters() {
        let cell = cells().first().expect("solo parameter map has cells");
        let mut c = Config { mode: "free".into(), shape: "triangle".into(), rate: 1.0,
            depth: 100.0, center: 50.0,
            points: vec![[cell.pid, cell.inst, cell.ai, 0, 65535, 42, 0]], wave: None };
        assert!(c.valid());
        for index in [3, 4, 5] {
            let old = c.points[0][index];
            c.points[0][index] = 65536;
            assert!(!c.valid(), "wire column {index}");
            c.points[0][index] = old;
        }
        c.points[0][6] = 2;
        assert!(!c.valid());
        c.points[0][6] = 0;
        c.points[0][0] = -1;
        assert!(!c.valid());
        c.points.clear();
        c.rate = f64::NAN;
        assert!(!c.valid());
        c.rate = 1.0;
        c.depth = f64::INFINITY;
        assert!(!c.valid());
        c.depth = 0.0;
        c.center = -0.1;
        assert!(!c.valid());
    }

    #[test]
    fn one_shot_and_half_cycle_stop_at_their_endpoints() {
        let mut c = Config { mode: "one".into(), shape: "ramp".into(), rate: 1.0,
            depth: 100.0, center: 50.0, points: vec![], wave: None };
        let end = c.position((2.0 * SAMPLE_RATE) as u64);
        assert!(end < 1.0);
        assert_eq!(c.position((4.0 * SAMPLE_RATE) as u64), end);
        c.mode = "half".into();
        assert!((c.position((2.0 * SAMPLE_RATE) as u64) - 50.0).abs() < 0.1);
        assert_eq!(c.position((4.0 * SAMPLE_RATE) as u64), c.position((2.0 * SAMPLE_RATE) as u64));
        c.mode = "free".into();
        c.shape = "random".into();
        assert_eq!(c.position(0), c.position(0), "random value is deterministic for a cycle");
        assert_eq!(c.position(0), c.position((SAMPLE_RATE / 2.0) as u64));
    }
}
