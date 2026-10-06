//! Voice allocation for the polyphonic Solo Synth: several instances of
//! the instrument, each playing one note on channel 1. This turns the MIDI
//! of a keyboard (or an MPE controller) into MIDI for each instance.
//!
//! One rule serves both: every (channel, note) gets an instance, and what
//! arrives on a channel reaches the instances bound to it, except that
//! channel 1 (the MPE master channel) reaches them all. Edits (SysEx,
//! program changes, channel-1 controllers) therefore go to every instance.
//!
//! The firmware's bend range stops at 24 semitones (RPN 0), which is less
//! than MPE's 48: the instances stay at 24 and bends are rescaled here, so
//! RPN 0 never reaches them.

/// Bend range every instance is set to (`BEND_SETUP`), in semitones.
pub const INSTANCE_BEND: f32 = 24.0;
/// RPN 0 = 24 semitones, then RPN null: sent to an instance after boot
/// and after each program change.
pub const BEND_SETUP: [u8; 15] = [0xB0, 0x65, 0, 0xB0, 0x64, 0, 0xB0, 0x06, 24, 0xB0, 0x65, 0x7F, 0xB0, 0x64, 0x7F];

const CENTRE: u16 = 8192;
const NO_RPN: (u8, u8) = (0x7F, 0x7F);

#[derive(Clone, Copy)]
struct Channel {
    bend: f32, // -1..1 of the channel's range
    pressure: u8,
    slide: Option<u8>, // CC 74, once seen
    rpn: (u8, u8),     // MSB, LSB selected for data entry
}

#[derive(Clone, Copy)]
struct Voice {
    key: Option<(u8, u8)>, // channel, note: kept after the release, whose tail still follows the channel
    down: bool,
    pedal: bool, // released with the damper down
    stamp: u64,  // when it was last started or released
    bend: u16,   // what the instance was last sent
    pressure: u8,
    slide: Option<u8>,
}

impl Voice {
    const IDLE: Voice = Voice { key: None, down: false, pedal: false, stamp: 0, bend: CENTRE, pressure: 0, slide: None };
}

pub struct Alloc {
    voices: Vec<Voice>,
    channels: [Channel; 16],
    sustain: bool,
    clock: u64,
    pub member_range: f32, // semitones of a full bend on channels 2..16
    pub master_range: f32, // and on channel 1
}

/// Messages for instances: (instance, MIDI on channel 1).
pub type Out = Vec<(usize, Vec<u8>)>;

impl Alloc {
    pub fn new(voices: usize, member_range: f32) -> Alloc {
        Alloc { voices: vec![Voice::IDLE; voices],
                channels: [Channel { bend: 0.0, pressure: 0, slide: None, rpn: NO_RPN }; 16], sustain: false, clock: 0,
                member_range, master_range: 2.0 }
    }

    /// More or fewer instances: the first ones keep their notes.
    pub fn resize(&mut self, voices: usize) {
        self.voices.resize(voices, Voice::IDLE);
    }

    /// Notes sounding or held by the damper, for display.
    pub fn held(&self) -> usize {
        self.voices.iter().filter(|v| v.down || v.pedal).count()
    }

    fn all(&self, msg: &[u8], out: &mut Out) {
        out.extend((0..self.voices.len()).map(|i| (i, msg.to_vec())));
    }

    /// The instances a channel's messages go to.
    fn bound(&self, ch: u8) -> Vec<usize> {
        (0..self.voices.len()).filter(|&i| ch == 0 || self.voices[i].key.is_some_and(|k| k.0 == ch)).collect()
    }

    /// Bring instance `i` up to date with its channel's bend, pressure and slide.
    fn follow(&mut self, i: usize, out: &mut Out) {
        let Some((ch, _)) = self.voices[i].key else { return };
        let (own, master) = (self.channels[ch as usize], self.channels[0]);
        let semitones = master.bend * self.master_range + if ch == 0 { 0.0 } else { own.bend * self.member_range };
        let bend = (CENTRE as f32 + semitones / INSTANCE_BEND * CENTRE as f32).round().clamp(0.0, 16383.0) as u16;
        let v = &mut self.voices[i];
        if bend != v.bend {
            v.bend = bend;
            out.push((i, vec![0xE0, (bend & 0x7F) as u8, (bend >> 7) as u8]));
        }
        let pressure = if ch == 0 { master.pressure } else { own.pressure.max(master.pressure) };
        if pressure != v.pressure {
            v.pressure = pressure;
            out.push((i, vec![0xD0, pressure]));
        }
        if ch != 0 && own.slide.is_some() && own.slide != v.slide {
            v.slide = own.slide;
            out.push((i, vec![0xB0, 74, own.slide.unwrap()]));
        }
    }

    fn release(&mut self, i: usize) {
        self.clock += 1;
        let v = &mut self.voices[i];
        (v.down, v.pedal, v.stamp) = (false, self.sustain, self.clock);
    }

    fn note_on(&mut self, ch: u8, note: u8, velocity: u8, out: &mut Out) {
        // The same key again, else the instance silent longest, else one the
        // damper holds, else the oldest held note.
        let rank = |v: &Voice| (if v.key == Some((ch, note)) { 0 } else if v.down { 3 } else if v.pedal { 2 } else { 1 }, v.stamp);
        let i = (0..self.voices.len()).min_by_key(|&i| rank(&self.voices[i])).unwrap();
        if let (true, Some((_, old))) = (self.voices[i].down, self.voices[i].key) {
            out.push((i, vec![0x80, old, 64])); // or the instance would play it legato
        }
        self.clock += 1;
        let v = &mut self.voices[i];
        (v.key, v.down, v.pedal, v.stamp) = (Some((ch, note)), true, false, self.clock);
        self.follow(i, out); // an MPE controller sends these before the note
        out.push((i, vec![0x90, note, velocity]));
    }

    fn note_off(&mut self, ch: u8, note: u8, velocity: u8, out: &mut Out) {
        if let Some(i) = self.voices.iter().position(|v| v.down && v.key == Some((ch, note))) {
            self.release(i);
            out.push((i, vec![0x80, note, velocity]));
        }
    }

    fn control(&mut self, ch: u8, cc: u8, value: u8, out: &mut Out) {
        let c = &mut self.channels[ch as usize];
        match cc {
            101 => c.rpn.0 = value,
            100 => c.rpn.1 = value,
            98 | 99 => c.rpn = NO_RPN,
            _ => {}
        }
        let rpn = c.rpn;
        match cc {
            // Bend range (RPN 0) is kept here; the MPE configuration message (RPN 6) means nothing to an instance.
            6 | 38 if rpn == (0, 0) || rpn == (0, 6) => {
                if cc == 6 && rpn == (0, 0) {
                    *(if ch == 0 { &mut self.master_range } else { &mut self.member_range }) = value as f32;
                    for i in 0..self.voices.len() {
                        self.follow(i, out);
                    }
                }
            }
            64 => {
                self.sustain = value >= 64;
                if !self.sustain {
                    self.clock += 1;
                    for v in self.voices.iter_mut().filter(|v| v.pedal) {
                        (v.pedal, v.stamp) = (false, self.clock);
                    }
                }
                self.all(&[0xB0, cc, value], out);
            }
            74 if ch != 0 => {
                self.channels[ch as usize].slide = Some(value);
                for i in self.bound(ch) {
                    self.follow(i, out);
                }
            }
            0 | 6 | 32 | 38 | 96..=101 if ch != 0 => {} // tone selection and data entry belong to channel 1
            _ => {
                for i in self.bound(ch) {
                    if cc >= 120 && self.voices[i].down {
                        self.release(i); // all sound / notes off and the mode messages
                    }
                    if cc == 74 {
                        self.voices[i].slide = Some(value);
                    }
                    out.push((i, vec![0xB0, cc, value]));
                }
            }
        }
    }

    /// One whole MIDI message from the player; -> what each instance gets.
    pub fn feed(&mut self, msg: &[u8]) -> Out {
        let mut out = Out::new();
        let Some(&status) = msg.first() else { return out };
        let (kind, ch) = (status & 0xF0, status & 0x0F);
        match (kind, msg.len()) {
            (0xF0, _) => self.all(msg, &mut out),
            (0x90, 3) if msg[2] > 0 => self.note_on(ch, msg[1], msg[2], &mut out),
            (0x90, 3) => self.note_off(ch, msg[1], 64, &mut out),
            (0x80, 3) => self.note_off(ch, msg[1], msg[2], &mut out),
            (0xA0, 3) => {
                // key pressure: the instance knows channel pressure only
                if let Some(i) = self.voices.iter().position(|v| v.key == Some((ch, msg[1]))) {
                    if self.voices[i].pressure != msg[2] {
                        self.voices[i].pressure = msg[2];
                        out.push((i, vec![0xD0, msg[2]]));
                    }
                }
            }
            (0xB0, 3) => self.control(ch, msg[1], msg[2], &mut out),
            (0xC0, 2) if ch == 0 => {
                self.all(&[0xC0, msg[1]], &mut out);
                self.all(&BEND_SETUP, &mut out); // a new tone may bring its own bend range
            }
            (0xD0, 2) | (0xE0, 3) => {
                let c = &mut self.channels[ch as usize];
                if kind == 0xD0 {
                    c.pressure = msg[1];
                } else {
                    c.bend = ((msg[2] as i32) << 7 | msg[1] as i32) as f32 / CENTRE as f32 - 1.0;
                }
                for i in self.bound(ch) {
                    self.follow(i, &mut out);
                }
            }
            _ => {}
        }
        out
    }
}

/// Queue a message for an instance, whose MIDI IN takes about 500 messages
/// a second: a controller, pressure or bend value replaces one of its kind
/// still waiting, unless a note or an edit came between them.
pub fn pace(queue: &mut std::collections::VecDeque<Vec<u8>>, msg: Vec<u8>) {
    let flowing = |m: &[u8]| match m {
        [0xE0, _, _] | [0xD0, _] => true,
        [0xB0, cc, _] => !matches!(cc, 0 | 6 | 32 | 38 | 64 | 96..=101 | 120..=127),
        _ => false,
    };
    if flowing(&msg) {
        for waiting in queue.iter_mut().rev().take_while(|w| flowing(w)) {
            if waiting[0] == msg[0] && (msg[0] != 0xB0 || waiting[1] == msg[1]) {
                *waiting = msg;
                return;
            }
        }
    }
    queue.push_back(msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes(out: &Out) -> Vec<(usize, u8, u8)> {
        out.iter().filter(|(_, m)| m[0] == 0x90 || m[0] == 0x80).map(|(i, m)| (*i, m[0], m[1])).collect()
    }

    #[test]
    fn a_chord_takes_one_instance_per_note() {
        let mut a = Alloc::new(3, 48.0);
        let used: Vec<usize> = [60, 64, 67].iter().map(|&n| a.feed(&[0x90, n, 100])[0].0).collect();
        assert_eq!(used, [0, 1, 2]);
        // a fourth note takes the oldest, releasing its note first
        assert_eq!(notes(&a.feed(&[0x90, 72, 100])), [(0, 0x80, 60), (0, 0x90, 72)]);
        // a released instance is preferred over a held one, the longest silent first
        a.feed(&[0x80, 67, 0]);
        a.feed(&[0x80, 64, 0]);
        assert_eq!(notes(&a.feed(&[0x90, 50, 100])), [(2, 0x90, 50)]);
        assert_eq!(a.held(), 2);
    }

    #[test]
    fn the_damper_keeps_an_instance_busy() {
        let mut a = Alloc::new(2, 48.0);
        assert_eq!(a.feed(&[0xB0, 64, 127]).len(), 2);
        a.feed(&[0x90, 60, 100]);
        a.feed(&[0x80, 60, 0]);
        assert_eq!(notes(&a.feed(&[0x90, 62, 100])), [(1, 0x90, 62)]);
        a.feed(&[0x80, 62, 0]);
        a.feed(&[0xB0, 64, 0]);
        assert_eq!(a.held(), 0);
    }

    #[test]
    fn member_channel_bend_reaches_its_note_only() {
        let mut a = Alloc::new(2, 48.0);
        a.feed(&[0x91, 60, 100]);
        a.feed(&[0x92, 64, 100]);
        // +12 of 48 semitones on channel 3 is +12 of the instance's 24
        assert_eq!(a.feed(&[0xE2, 0, 0x50]), [(1, vec![0xE0, 0, 0x60])]);
        // the master channel moves both, by its own 2 semitones
        let out = a.feed(&[0xE0, 0x7F, 0x7F]);
        assert_eq!(out.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1]);
        // beyond the instance's range the bend stops at the end
        a.feed(&[0xE0, 0, 0x40]);
        assert_eq!(a.feed(&[0xE2, 0x7F, 0x7F]), [(1, vec![0xE0, 0x7F, 0x7F])]);
    }

    #[test]
    fn what_came_before_the_note_is_sent_before_it() {
        let mut a = Alloc::new(2, 24.0);
        assert!(a.feed(&[0xE1, 0, 0x60]).is_empty());
        assert!(a.feed(&[0xD1, 90]).is_empty());
        assert!(a.feed(&[0xB1, 74, 30]).is_empty());
        assert_eq!(a.feed(&[0x91, 60, 100]),
                   [(0, vec![0xE0, 0, 0x60]), (0, vec![0xD0, 90]), (0, vec![0xB0, 74, 30]), (0, vec![0x90, 60, 100])]);
        // the next finger on that channel starts from the channel's state again: nothing to resend
        a.feed(&[0x81, 60, 0]);
        a.feed(&[0x92, 62, 100]);
        assert_eq!(a.feed(&[0x91, 60, 100]), [(0, vec![0x90, 60, 100])]);
    }

    #[test]
    fn bend_range_and_mpe_setup_stay_here() {
        let mut a = Alloc::new(2, 48.0);
        for msg in [[0xB0, 101, 0], [0xB0, 100, 6], [0xB0, 6, 15]] {
            assert!(a.feed(&msg).iter().all(|(_, m)| m[1] != 6));
        }
        for msg in [[0xB1, 101, 0], [0xB1, 100, 0], [0xB1, 6, 12], [0xB1, 38, 0]] {
            assert!(a.feed(&msg).is_empty());
        }
        assert_eq!(a.member_range, 12.0);
        // NRPN data entry on channel 1 is an edit: every instance gets it
        a.feed(&[0xB0, 99, 0x30]);
        a.feed(&[0xB0, 98, 3]);
        assert_eq!(a.feed(&[0xB0, 6, 40]).len(), 2);
        assert_eq!(a.feed(&[0xF0, 0x44, 0xF7]).len(), 2);
    }

    #[test]
    fn pacing_keeps_the_latest_value_and_the_order() {
        let mut q = std::collections::VecDeque::new();
        for msg in [vec![0xE0, 1, 64], vec![0xD0, 5], vec![0xE0, 2, 64], vec![0x90, 60, 100], vec![0xE0, 3, 64], vec![0xB0, 74, 1],
                    vec![0xB0, 1, 9], vec![0xB0, 74, 2], vec![0xB0, 64, 127], vec![0xB0, 64, 0]] {
            pace(&mut q, msg);
        }
        assert_eq!(Vec::from(q), [vec![0xE0, 2, 64], vec![0xD0, 5], vec![0x90, 60, 100], vec![0xE0, 3, 64], vec![0xB0, 74, 2],
                                  vec![0xB0, 1, 9], vec![0xB0, 64, 127], vec![0xB0, 64, 0]]);
    }
}
