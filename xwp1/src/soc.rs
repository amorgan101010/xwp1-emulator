//! uPD800468 on-chip peripherals: interrupt controller, timers, UARTs, key
//! controller, ADC and GPIO. Register behaviour follows MAME's
//! upd800468.cpp / vic_pl192.cpp (ref/mame/).
use std::collections::VecDeque;

pub const VIC_BASE: u32 = 0xFFFF_F000;
pub const TIMER_BASE: u32 = 0x2A00_3500;
const TIMER_IRQ: [u32; 3] = [21, 22, 23];

/// PL190-style vectored interrupt controller with 32 vector slots.
pub struct Vic {
    raw: u32,
    soft: u32,
    enable: u32,
    select: u32,
    vectaddr: [u32; 32],
    vectctl: [u32; 32],
    defaddr: u32,
    priority: usize, // slot being serviced; lower slots may pre-empt
    stack: Vec<usize>,
}

impl Vic {
    pub fn new() -> Self {
        Vic { raw: 0, soft: 0, enable: 0, select: 0, vectaddr: [0; 32], vectctl: [0; 32], defaddr: 0,
              priority: 32, stack: Vec::new() }
    }

    pub fn set_line(&mut self, irq: u32, state: bool) {
        if state {
            self.raw |= 1 << irq;
        } else {
            self.raw &= !(1 << irq);
        }
    }

    fn pending(&self) -> u32 {
        (self.raw | self.soft) & self.enable & !self.select
    }

    fn slot(&self) -> Option<usize> {
        let active = self.pending();
        (0..self.priority).find(|&i| {
            let ctl = self.vectctl[i];
            ctl & 0x20 != 0 && active >> (ctl & 0x1F) & 1 != 0
        })
    }

    /// True when the IRQ line to the CPU is asserted.
    pub fn irq(&self) -> bool {
        self.slot().is_some()
    }

    /// True when a source routed to FIQ (interrupt select) is pending.
    pub fn fiq(&self) -> bool {
        (self.raw | self.soft) & self.enable & self.select != 0
    }

    pub fn read(&mut self, addr: u32) -> u32 {
        let off = addr - VIC_BASE;
        match off {
            0x000 => self.pending(),
            0x004 => (self.raw | self.soft) & self.enable & self.select,
            0x008 => self.raw,
            0x00C => self.select,
            0x010 => self.enable,
            0x018 => self.soft,
            0x030 => match self.slot() {
                None => self.defaddr,
                Some(slot) => {
                    self.stack.push(self.priority);
                    self.priority = slot;
                    self.vectaddr[slot]
                }
            },
            0x034 => self.defaddr,
            0x100..=0x17F => self.vectaddr[((off - 0x100) >> 2) as usize],
            0x200..=0x27F => self.vectctl[((off - 0x200) >> 2) as usize],
            _ => 0,
        }
    }

    pub fn write(&mut self, addr: u32, value: u32) {
        let off = addr - VIC_BASE;
        match off {
            0x00C => self.select = value,
            0x010 => self.enable |= value,
            0x014 => self.enable &= !value,
            0x018 => self.soft |= value,
            0x01C => self.soft &= !value,
            0x030 => self.priority = self.stack.pop().unwrap_or(32),
            0x034 => self.defaddr = value,
            0x100..=0x17F => self.vectaddr[((off - 0x100) >> 2) as usize] = value,
            0x200..=0x27F => self.vectctl[((off - 0x200) >> 2) as usize] = value,
            0x2C8 => self.raw &= !value,
            _ => {}
        }
    }
}

/// Three interval timers. A timer fires every rate + 1 timer clocks, and
/// the timer clock is 128 per audio sample: timer 1 (rate 127) is the Solo
/// Synth's per-sample FIQ.
pub struct Timers {
    rate: [u32; 3],
    control: [u32; 3],
    left: [f64; 3],
}

impl Timers {
    pub fn new() -> Self {
        Timers { rate: [0; 3], control: [0; 3], left: [0.0; 3] }
    }

    pub fn read(&self, addr: u32) -> u32 {
        let (n, reg) = (((addr - TIMER_BASE) >> 4) as usize, (addr - TIMER_BASE) & 0xF);
        if n > 2 {
            return 0;
        }
        match reg {
            4 => self.rate[n],
            8 => self.control[n],
            _ => 0,
        }
    }

    pub fn write(&mut self, vic: &mut Vic, addr: u32, value: u32) {
        let (n, reg) = (((addr - TIMER_BASE) >> 4) as usize, (addr - TIMER_BASE) & 0xF);
        if n > 2 {
            return;
        }
        if reg == 4 {
            self.rate[n] = value;
        } else if reg == 8 {
            if (value ^ self.control[n]) & 2 != 0 {
                if value & 2 != 0 {
                    self.left[n] = (self.rate[n] as f64) + 1.0;
                } else {
                    vic.set_line(TIMER_IRQ[n], false);
                }
            }
            if value & 1 == 0 {
                vic.set_line(TIMER_IRQ[n], false);
            }
            self.control[n] = value;
        }
    }

    pub fn advance(&mut self, vic: &mut Vic, clocks: f64) {
        for n in 0..3 {
            if self.control[n] & 2 != 0 && self.rate[n] != 0 {
                self.left[n] -= clocks;
                if self.left[n] <= 0.0 {
                    let period = (self.rate[n] as f64) + 1.0;
                    self.left[n] += period;
                    if self.left[n] <= 0.0 {
                        self.left[n] = period;
                    }
                    if self.control[n] & 1 != 0 {
                        vic.set_line(TIMER_IRQ[n], true);
                    }
                }
            }
        }
    }
}

/// On-chip UART: control at +0, receive data at +4, error status at +8,
/// transmit data at +0xc. One interrupt per received byte and one per
/// transmitted byte. UART0 (0x2a003a00, IRQ 16/17) is the link to the panel
/// sub-CPU, which also carries MIDI.
pub struct Uart {
    pub base: u32,
    rx_irq: u32,
    tx_irq: u32,
    byte_clocks: i64, // CPU clocks per byte on the wire
    regs: [u32; 0x40],
    pub sent: Vec<u8>, // everything the firmware transmitted
    rx_queue: VecDeque<u8>,
    rx_data: u32,
    rx_unread: bool,
    pub rx_lost: u64, // received bytes replaced before the firmware read them
    tx_left: i64,
    rx_left: i64,
}

impl Uart {
    pub fn new(base: u32, rx_irq: u32, tx_irq: u32, byte_clocks: i64) -> Self {
        Uart { base, rx_irq, tx_irq, byte_clocks, regs: [0; 0x40], sent: Vec::new(), rx_queue: VecDeque::new(),
               rx_data: 0, rx_unread: false, rx_lost: 0, tx_left: 0, rx_left: 0 }
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.rx_queue.extend(data);
    }

    /// Bytes fed and not yet taken by the firmware.
    pub fn waiting(&self) -> usize {
        self.rx_queue.len()
    }

    pub fn read(&mut self, addr: u32) -> u32 {
        match addr - self.base {
            4 => {
                self.rx_unread = false;
                self.rx_data
            }
            8 => 0,
            off => self.regs[off as usize],
        }
    }

    pub fn write(&mut self, addr: u32, value: u32) {
        let off = addr - self.base;
        self.regs[off as usize] = value;
        if off == 0xC {
            self.sent.push(value as u8);
            self.tx_left = self.byte_clocks;
        }
    }

    pub fn tick(&mut self, vic: &mut Vic, clocks: i64) {
        if self.tx_left > 0 {
            self.tx_left -= clocks;
            if self.tx_left <= 0 {
                vic.set_line(self.tx_irq, true);
            }
        }
        if !self.rx_queue.is_empty() {
            self.rx_left -= clocks;
            if self.rx_left <= 0 {
                self.rx_lost += self.rx_unread as u64;
                self.rx_unread = true;
                self.rx_data = self.rx_queue.pop_front().unwrap() as u32;
                self.rx_left = self.byte_clocks;
                vic.set_line(self.rx_irq, true);
            }
        }
    }
}

/// Key matrix controller at 0x1fff00a0, as MAME's gt913_kbd_hle: a FIFO of
/// key events read from +0 as (press << 15 | code << 8 | velocity), status
/// at +2 (bit 15 = event waiting), control at +4 (bit 14 = interrupt
/// enable, IRQ 31).
pub struct Keys {
    fifo: VecDeque<u32>,
    status: u32,
    regs: [u32; 0x10],
}

impl Keys {
    pub const BASE: u32 = 0x1FFF_00A0;

    pub fn new() -> Self {
        Keys { fifo: VecDeque::new(), status: 0, regs: [0; 0x10] }
    }

    fn update(&mut self, vic: &mut Vic) {
        self.status = (self.status & 0x7FFF) | if self.fifo.is_empty() { 0 } else { 0x8000 };
        vic.set_line(31, self.status & 0xC000 == 0xC000);
    }

    pub fn key(&mut self, vic: &mut Vic, code: u32, down: bool, velocity: u32) {
        self.fifo.push_back((if down { 0x80 } else { 0 } | code) << 8 | velocity);
        self.update(vic);
    }

    pub fn read(&mut self, vic: &mut Vic, addr: u32) -> u32 {
        match addr - Self::BASE {
            0 => {
                let value = self.fifo.pop_front().unwrap_or(0xFF00);
                self.update(vic);
                value
            }
            2 => self.status,
            off => self.regs[off as usize],
        }
    }

    pub fn write(&mut self, vic: &mut Vic, addr: u32, value: u32) {
        let off = addr - Self::BASE;
        self.regs[off as usize] = value;
        if off == 4 {
            self.status = (self.status & 0x8000) | (value & 0x7FFF);
            self.update(vic);
        }
    }
}

/// 10-bit ADC, control at 0x1fff00c0, channel n at +2n. The firmware reads
/// `raw ^ 0x200`. Channel 4 is the pitch bender (rest 0x220), 7 the
/// modulation wheel, 1 the sustain pedal jack (low = pressed), 3 the
/// battery sense: at 0 the firmware shows "Battery Low!" 9.6 s after reset
/// and, while that alert is up, a program change does not load the tone's
/// effect record. Full scale stands for the mains adapter (the reading on
/// the instrument has not been measured). `values` holds what the
/// firmware should see after its XOR.
pub struct Adc {
    pub values: [u32; 8],
    /// Channels 5 and 6 sit behind a multiplexer the firmware steps with
    /// port 2 bits 2..4 (`Ports::mux`): eight inputs on channel 5, six on
    /// channel 6 (the nine sliders, the four knobs and one more).
    pub muxed: [[u32; 8]; 2],
    control: u32,
}

impl Adc {
    pub const BASE: u32 = 0x1FFF_00C0;

    pub fn new() -> Self {
        let mut values = [0; 8];
        values[4] = 0x220;
        values[1] = 0x3FF;
        values[3] = 0x3FF;
        Adc { values, muxed: [[0; 8]; 2], control: 0 }
    }

    pub fn read(&self, addr: u32, mux: usize) -> u32 {
        match ((addr - Self::BASE) >> 1) as usize {
            0 => self.control,
            ch @ (5 | 6) => self.muxed[ch - 5][mux] ^ 0x200,
            ch => self.values[ch] ^ 0x200,
        }
    }

    pub fn write(&mut self, addr: u32, value: u32) {
        if addr == Self::BASE {
            self.control = value;
        }
    }
}

/// Four GPIO ports at 0x1fff0140 + 4n: direction at +0, data at +2. All
/// input lines default high.
pub struct Ports {
    pub inputs: [u32; 4],
    ddr: [u32; 4],
    out: [u32; 4],
}

impl Ports {
    pub const BASE: u32 = 0x1FFF_0140;

    pub fn new() -> Self {
        Ports { inputs: [0xFFFF; 4], ddr: [0; 4], out: [0; 4] }
    }

    /// The input the slider / knob multiplexer is switched to.
    pub fn mux(&self) -> usize {
        (self.out[2] >> 2 & 7) as usize
    }

    pub fn read(&self, addr: u32) -> u32 {
        let (n, reg) = (((addr - Self::BASE) >> 2) as usize, (addr - Self::BASE) & 3);
        if reg == 0 { self.ddr[n] } else { self.inputs[n] }
    }

    pub fn write(&mut self, addr: u32, value: u32) {
        let (n, reg) = (((addr - Self::BASE) >> 2) as usize, (addr - Self::BASE) & 3);
        if reg == 0 {
            self.ddr[n] = value;
        } else {
            self.out[n] = value;
        }
    }
}
