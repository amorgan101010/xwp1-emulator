//! The front panel as the firmware drives it: the display's memory and the
//! LEDs, taken from what the firmware sends to the panel processor
//! (docs/FINDINGS.md, "Parts, keyboard, Performance, front panel"):
//!
//!   0xa0 nn / 0xa1 nn        LED nn off / on
//!   0x90..0x92, address, ..  display memory from (status & 3) * 128 + address,
//!                            8-bit bytes packed MSB first into 7-bit groups
//!
//! MIDI OUT shares the link (0x80 / 0x81 + seven bits) and is skipped here.
//! The other way go the buttons and the dial (panel link) and the nine
//! sliders and four knobs (two ADC channels behind a multiplexer).
//! A page asks for the state and for the buttons with messages the firmware
//! never sees, as it does for memory (`engine::PEEK`).

pub const RAM: usize = 362; // bytes of display memory the firmware addresses; 0..144 is the 72 x 16 dot matrix
pub const LEDS: usize = 84;

/// F0 7D 58 4C F7 from a page: send the state now and after every change.
/// The answers: F0 7D 58 4C 00, the LEDs (twelve bytes of seven, LED 0 in
/// bit 0 of the first), F7; and F0 7D 58 4C 01, the display memory (two
/// nibbles per byte), F7.
pub const FRONT: [u8; 4] = [0xF0, 0x7D, 0x58, 0x4C];
/// F0 7D 58 42 code down F7 from a page: a panel button (code = matrix column * 8 + row).
pub const BUTTON: [u8; 4] = [0xF0, 0x7D, 0x58, 0x42];
/// F0 7D 58 44 clicks F7 from a page: the dial, a signed count in seven bits (1 = one click up, 0x7F = one down).
pub const DIAL: [u8; 4] = [0xF0, 0x7D, 0x58, 0x44];
/// F0 7D 58 41 control position F7 from a page: sliders 1..8 and MASTER
/// are controls 0..8, knobs 1..4 are 9..12; position 0..127 is the value
/// the firmware is to read. The positions are part of the state a page
/// is told: F0 7D 58 4C 02, thirteen positions, F7.
pub const CONTROL: [u8; 4] = [0xF0, 0x7D, 0x58, 0x41];
pub const CONTROLS: usize = 13;

/// Where a control is wired: (ADC channel, multiplexer input). From the
/// firmware's descriptor tables (0x180b7b34: channel 5 inputs 0..7 are
/// sliders 9..2, channel 6 input 0 is slider 1 and inputs 1..4 the knobs)
/// and from what each input makes the firmware send.
pub fn control_input(control: usize) -> (usize, usize) {
    match control {
        0 => (6, 0),
        1..=8 => (5, 8 - control),
        _ => (6, control - 8),
    }
}

/// The converter reading that makes the firmware see `position`. The
/// firmware scales a reading by a range it starts at 0..0x333 and widens
/// for good when a reading goes beyond, so nothing above that is sent. A
/// slider is one straight line; a knob is two, with a flat stretch at 64
/// (readings 546..604: the centre notch).
pub fn control_reading(control: usize, position: u8) -> u32 {
    let p = position.min(127) as f32;
    let reading = match (control < 9, position) {
        (_, 0) => 0.0,
        (true, _) => 16.0 + 6.32 * (p - 1.0),
        (false, 1..=63) => 18.0 + 8.45 * (p - 1.0),
        (false, 64) => 575.0,
        (false, 127) => 817.0,
        (false, _) => 603.3 + 3.39 * (p - 65.0),
    };
    reading.round() as u32
}

pub struct Front {
    pub ram: [u8; RAM],
    pub leds: u128,
    pub leds_changed: bool,
    pub lcd_changed: bool,
    status: u8,
    at: Option<usize>, // where the next display byte goes, once the address has come
    bits: u32,
    count: u32,
}

impl Default for Front {
    fn default() -> Self {
        Front { ram: [0; RAM], leds: 0, leds_changed: false, lcd_changed: false, status: 0, at: None, bits: 0, count: 0 }
    }
}

impl Front {
    /// Take in bytes the firmware sent to the panel processor.
    pub fn feed(&mut self, raw: &[u8]) {
        for &b in raw {
            if b >= 0x80 {
                (self.status, self.at, self.bits, self.count) = (b, None, 0, 0);
                continue;
            }
            match self.status {
                0xA0 | 0xA1 => {
                    let (bit, before) = (1u128 << (b as usize % 128), self.leds);
                    self.leds = if self.status == 0xA1 { before | bit } else { before & !bit };
                    self.leds_changed |= self.leds != before;
                    self.status = 0;
                }
                0x90..=0x92 => match self.at {
                    None => self.at = Some((self.status as usize & 3) * 128 + b as usize),
                    Some(at) => {
                        self.bits = self.bits << 7 | b as u32;
                        self.count += 7;
                        if self.count >= 8 {
                            self.count -= 8;
                            let value = (self.bits >> self.count) as u8;
                            if at < RAM && self.ram[at] != value {
                                self.ram[at] = value;
                                self.lcd_changed = true;
                            }
                            self.at = Some(at + 1);
                        }
                    }
                },
                _ => {}
            }
        }
    }

    pub fn leds_message(&self) -> Vec<u8> {
        let mut out = FRONT.to_vec();
        out.push(0);
        out.extend((0..LEDS.div_ceil(7)).map(|i| (self.leds >> (7 * i)) as u8 & 0x7F));
        out.push(0xF7);
        out
    }

    pub fn lcd_message(&self) -> Vec<u8> {
        let mut out = FRONT.to_vec();
        out.push(1);
        out.extend(self.ram.iter().flat_map(|b| [b >> 4, b & 15]));
        out.push(0xF7);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leds_and_display() {
        let mut f = Front::default();
        // MIDI OUT bytes in between change nothing
        f.feed(&[0xA1, 37, 0x81, 0x10, 0x80, 0x3C, 0xA1, 8, 0xA0, 8, 0xA1, 78]);
        assert_eq!(f.leds, 1 << 37 | 1 << 78);
        assert!(f.leds_changed && !f.lcd_changed);
        assert_eq!(f.leds_message()[5 + 37 / 7], 1 << (37 % 7));
        // three bytes ff 01 80 at address 130: 24 bits in four groups of seven (the last padded)
        f.feed(&[0x91, 2, 0x7F, 0x40, 0x30, 0x00]);
        assert_eq!(&f.ram[130..133], &[0xFF, 0x01, 0x80]);
        assert!(f.lcd_changed);
        // a message split between two calls
        f.feed(&[0x90, 0]);
        f.feed(&[0x55, 0x2A]);
        assert_eq!(f.ram[0], 0xAA);
        let m = f.lcd_message();
        assert_eq!((m.len(), m[5], m[6]), (5 + 2 * RAM + 1, 0xA, 0xA));
    }
}
