//! Voice variation: with several instances playing one Solo Synth tone, every instance but the first holds a few
//! of the tone's values a little off, by an amount fixed for that instance and scaled by four player settings
//! (glide, filters, envelopes, levels). The first instance is the reference: pages read the tone from it.
//!
//! An instance does this to itself. A value set from outside (an edit, which goes to all instances alike) is
//! shifted on its way in; a value that turns up in the tone's memory by another way (a tone chosen, a controller,
//! a knob on the front panel) is taken as the new starting point and shifted once the memory has settled.
//! Levels are a gain in the mix (`gain`), nothing the firmware is told.
use std::collections::VecDeque;

use serde_json::Value;

use crate::machine::Machine;

pub const KNOBS: usize = 4;
pub const NAMES: [&str; KNOBS] = ["vglide", "vfilter", "venv", "vlevel"];
const GLIDE: usize = 0;
const FILTER: usize = 1;
const ENVELOPE: usize = 2;
const LEVEL: usize = 3;

/// -1..1, fixed for an instance and a parameter (never drawn at run time: a render must repeat). The first
/// instance is not shifted.
pub fn offset(voice: usize, parameter: u32) -> f32 {
    if voice == 0 {
        return 0.0;
    }
    let mut x = (voice as u64) << 32 | parameter as u64;
    x = (x ^ (x >> 30)).wrapping_add(0x9E37_79B9_7F4A_7C15).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

/// The instance's gain in the mix: up to 3 dB either way with the level setting full.
pub fn gain(voice: usize, amounts: [u8; KNOBS]) -> f32 {
    10f32.powf(offset(voice, 0x1000) * 3.0 * amounts[LEVEL] as f32 / 127.0 / 20.0)
}

/// A value in the tone's memory (`assets/mem.json`: kind 0 a byte, 4 bits of a byte).
#[derive(Clone, Copy)]
struct Cell {
    addr: u32,
    kind: u8,
    a: i64,
    b: i64,
}

impl Cell {
    fn read(&self, m: &Machine) -> Option<i64> {
        let byte = *m.mem_read(self.addr, 1).first()? as i64;
        match self.kind {
            0 => Some((self.a * byte + self.b) & 127),
            4 => Some((byte >> self.a) & self.b),
            _ => None,
        }
    }
}

struct Slot {
    pid: u8,
    inst: u8,
    cell: Cell,
    knob: usize,
    switch: Option<Cell>, // the oscillator's switch: a block that is off is left alone
    base: Option<u8>,     // the value without the shift
    wrote: Option<u8>,    // the value this last put there
}

pub struct Vary {
    voice: usize,
    amounts: [u8; KNOBS],
    slots: Vec<Slot>,
    tone: Option<u32>, // address of part 1's tone number (a word; the number is bits 1..15)
    seen: Vec<i64>,    // the memory at the last look: acted on when two looks agree
    quiet: i64,        // blocks since anything was on its way in
}

/// Blocks (64 samples) between two looks at the memory, and the wait after a tone was chosen.
const LOOK: i64 = 70;
const AFTER_TONE: i64 = 330;

impl Vary {
    pub fn new(voice: usize) -> Vary {
        let cells = |text: &str| -> Vec<Vec<i64>> {
            let data: Value = serde_json::from_str(text).unwrap_or_default();
            data["cells"].as_array().map(|rows| rows.iter().map(|r| r.as_array().map(|r| r.iter().filter_map(Value::as_i64).collect()).unwrap_or_default()).collect()).unwrap_or_default()
        };
        let solo = cells(include_str!("../assets/mem.json"));
        let cell = |r: &[i64]| Cell { addr: r[4] as u32, kind: r[5] as u8, a: r[6], b: r[7] };
        let find = |pid: i64, inst: i64| solo.iter().find(|r| r.len() == 8 && r[0] == 9 && r[1] == pid && r[2] == inst && r[3] == 0).map(|r| cell(r));
        let mut slots = Vec::new();
        // per oscillator: portamento time, the amp envelope's attack, decay and release times
        for (pid, knob) in [(5, GLIDE), (49, ENVELOPE), (51, ENVELOPE), (53, ENVELOPE), (55, ENVELOPE)] {
            for inst in 0..6 {
                if let Some(c) = find(pid, inst) {
                    slots.push(Slot { pid: pid as u8, inst: inst as u8, cell: c, knob, switch: find(0, inst), base: None, wrote: None });
                }
            }
        }
        // the Total Filter: cutoff, its envelope's times
        for (pid, knob) in [(73, FILTER), (80, ENVELOPE), (82, ENVELOPE), (84, ENVELOPE), (86, ENVELOPE)] {
            if let Some(c) = find(pid, 0) {
                slots.push(Slot { pid: pid as u8, inst: 0, cell: c, knob, switch: None, base: None, wrote: None });
            }
        }
        let tone = cells(include_str!("../assets/perf_mem.json")).iter().find(|r| r.len() == 8 && r[0] == 2 && r[1] == 0x69 && r[2] == 0).map(|r| r[4] as u32);
        Vary { voice, amounts: [0; KNOBS], slots, tone, seen: Vec::new(), quiet: 0 }
    }

    pub fn set(&mut self, amounts: [u8; KNOBS]) {
        self.amounts = amounts;
    }

    /// The value an instance holds for one that is `base` in the tone.
    fn shifted(&self, slot: &Slot, base: u8) -> u8 {
        let amount = self.amounts[slot.knob] as f32 / 127.0;
        // a voice's blocks move together: the shift is the parameter's, not the oscillator's
        let r = offset(self.voice, slot.pid as u32);
        let span = if slot.knob == FILTER { 16.0 } else { 3.0 + 0.3 * base as f32 };
        (base as f32 + r * amount * span).round().clamp(0.0, 127.0) as u8
    }

    fn set_message(slot: &Slot, value: u8) -> Vec<u8> {
        let mut msg = vec![0xF0, 0x44, 0x16, 0x03, 0x7F, 1, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, slot.inst, 0, slot.pid, 0, 0, 0, 0, 0, value, 0xF7];
        msg[16] = slot.inst;
        msg
    }

    /// A message on its way to the firmware: a value this shifts is shifted here, and a tone choice means the
    /// memory is about to change under it. `waiting`: this one's own messages not sent yet (`look`), which the
    /// message overtakes: those it makes stale are dropped.
    pub fn incoming(&mut self, msg: &mut [u8], waiting: &mut VecDeque<Vec<u8>>) {
        // what can move the tone's values starts the wait again; notes, bends, pressure and the wheel do not, or
        // nothing would ever be shifted while somebody plays
        if match msg[0] & 0xF0 { 0xF0 | 0xC0 => true, 0xB0 => !matches!(msg.get(1), Some(1 | 64 | 120..=127)), _ => false } {
            self.quiet = self.quiet.min(0);
        }
        let set = msg.len() == 26 && msg[..6] == [0xF0, 0x44, 0x16, 0x03, 0x7F, 1];
        if set && msg[6] == 9 && msg[19] == 0 && msg[20] == 0 {
            if let Some(i) = self.slots.iter().position(|s| s.pid == msg[18] && s.inst == msg[16]) {
                let value = self.shifted(&self.slots[i], msg[24]);
                (self.slots[i].base, self.slots[i].wrote) = (Some(msg[24]), Some(value));
                waiting.retain(|w| (w[18], w[16]) != (msg[18], msg[16]));
                msg[24] = value;
            }
        } else if (set && msg[6] == 2 && msg[18] == 0x69) || (msg[0] < 0xF0 && msg.contains(&0xC0)) {
            for slot in &mut self.slots {
                (slot.base, slot.wrote) = (None, None);
            }
            waiting.clear();
            self.quiet = -AFTER_TONE;
        }
    }

    /// After every run of `blocks`: with nothing on its way in (`idle`) and the memory the same at two looks,
    /// the messages that put every value where it belongs.
    pub fn look(&mut self, m: &Machine, blocks: usize, idle: bool) -> Vec<Vec<u8>> {
        if !idle {
            return Vec::new();
        }
        let before = self.quiet;
        self.quiet += blocks as i64;
        if self.quiet < LOOK || before.div_euclid(LOOK) == self.quiet.div_euclid(LOOK) {
            return Vec::new();
        }
        // a Solo Synth tone (tone numbers 0..99)? Another kind keeps the Solo values of the tone before it.
        let solo = self.tone.and_then(|a| m.mem_read(a, 2).get(..2).map(|w| (w[0] as u32 | (w[1] as u32) << 8) >> 1 & 0x7FFF)).is_none_or(|t| t < 100);
        let now: Vec<i64> = self.slots.iter().map(|s| {
            let on = s.switch.is_none_or(|c| c.read(m).is_some_and(|v| v != 0));
            if on && solo { s.cell.read(m).unwrap_or(-1) } else { -1 }
        }).collect();
        if now != self.seen {
            self.seen = now;
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 0..self.slots.len() {
            let Ok(held) = u8::try_from(now[i]) else { continue };
            let slot = &self.slots[i];
            let base = match (slot.base, slot.wrote) {
                (Some(base), Some(wrote)) if wrote == held => base,
                _ => held, // put there by something else: the tone's own value now
            };
            let value = self.shifted(slot, base);
            if value != held {
                out.push(Self::set_message(slot, value));
            }
            (self.slots[i].base, self.slots[i].wrote) = (Some(base), Some(value));
        }
        if !out.is_empty() {
            self.quiet = 0;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_fixed_and_spread() {
        assert_eq!(offset(0, 73), 0.0);
        assert_eq!(gain(0, [127; KNOBS]), 1.0);
        assert_eq!(gain(3, [127, 127, 127, 0]), 1.0);
        let all: Vec<f32> = (1..8).flat_map(|v| [5, 49, 73, 0x1000].map(|p| offset(v, p))).collect();
        assert!(all.iter().all(|r| (-1.0..=1.0).contains(r)));
        assert_eq!(offset(3, 73), offset(3, 73));
        let (low, high) = (all.iter().cloned().fold(1.0, f32::min), all.iter().cloned().fold(-1.0, f32::max));
        assert!(low < -0.5 && high > 0.5, "{low} .. {high}");
        // the seven voices' filter shifts are not bunched on one side
        let filters: Vec<f32> = (1..8).map(|v| offset(v, 73)).collect();
        assert!(filters.iter().any(|r| *r < -0.2) && filters.iter().any(|r| *r > 0.2), "{filters:?}");
    }

    #[test]
    fn an_edit_is_shifted_on_its_way_in() {
        let mut v = Vary::new(2);
        assert_eq!(v.slots.len(), 35);
        v.set([0, 127, 0, 0]);
        let slot = v.slots.iter().position(|s| s.pid == 73).unwrap();
        let mut msg = Vary::set_message(&v.slots[slot], 64);
        v.incoming(&mut msg, &mut VecDeque::new());
        let want = (64.0 + offset(2, 73) * 16.0).round() as u8;
        assert_eq!(msg[24], want);
        assert_ne!(want, 64);
        assert_eq!((v.slots[slot].base, v.slots[slot].wrote), (Some(64), Some(want)));
        // a value of another knob, at amount 0, passes as it is
        let other = v.slots.iter().position(|s| s.pid == 49 && s.inst == 1).unwrap();
        let mut msg = Vary::set_message(&v.slots[other], 30);
        v.incoming(&mut msg, &mut VecDeque::new());
        assert_eq!(msg[24], 30);
    }
}
