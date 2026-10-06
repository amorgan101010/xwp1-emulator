//! ARM7TDMI interpreter (ARMv4T: ARM and Thumb states, no coprocessor).
//! Written for speed on this one machine: no decode cache (the per-sample
//! routines run from RAM), memory through a statically dispatched `Bus`,
//! and an exact instruction budget per call.
//!
//! One instruction here is one instruction under Unicorn, so a Thumb BL
//! pair counts once (Unicorn decodes it as a single 32-bit instruction).
//! Where ARMv4T differs from the ARMv7 core Unicorn models, this follows
//! ARMv4T and counts the event in `quirks`; all counters at zero means the
//! two cannot have diverged for that reason.

pub trait Bus {
    fn read8(&mut self, addr: u32) -> u32;
    /// `addr` is even.
    fn read16(&mut self, addr: u32) -> u32;
    /// `addr` is a multiple of 4.
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, value: u32);
    fn write16(&mut self, addr: u32, value: u32);
    fn write32(&mut self, addr: u32, value: u32);
    /// True once an access has failed; the CPU stops after that instruction.
    fn faulted(&self) -> bool;
}

/// Events where ARMv4T and ARMv7 behave differently (index into `Arm7::quirks`).
pub const Q_UNALIGNED: usize = 0; // word or halfword access at an unaligned address
pub const Q_PC_STORE: usize = 1; // STR / STM of pc (address + 12 here, + 8 on ARMv7)
pub const Q_PC_LOAD: usize = 2; // pc loaded with a value whose bit 0 disagrees with the state
pub const Q_STM_BASE: usize = 3; // STM with writeback, base in the list and not first
pub const Q_NV: usize = 4; // condition code 1111
pub const QUIRKS: [&str; 5] = ["unaligned access", "pc stored", "pc load would interwork", "STM stores updated base",
                               "condition NV"];

const MODE_SVC: u32 = 0x13;

/// For each condition code, the set of N Z C V values (as a 4-bit index) that pass.
const COND: [u16; 16] = {
    let mut table = [0u16; 16];
    let mut flags = 0;
    while flags < 16 {
        let (n, z, c, v) = (flags & 8 != 0, flags & 4 != 0, flags & 2 != 0, flags & 1 != 0);
        let pass = [z, !z, c, !c, n, !n, v, !v, c && !z, !c || z, n == v, n != v, !z && n == v, z || n != v, true, false];
        let mut cond = 0;
        while cond < 16 {
            table[cond] |= (pass[cond] as u16) << flags;
            cond += 1;
        }
        flags += 1;
    }
    table
};

fn bank_of(mode: u32) -> usize {
    match mode & 0x1F {
        0x11 => 1,
        0x12 => 2,
        0x13 => 3,
        0x17 => 4,
        0x1B => 5,
        _ => 0, // user and system
    }
}

pub struct Arm7 {
    /// r15 holds the executing instruction's address + 8 (ARM) or + 4
    /// (Thumb); it is only ever read. `pc` is what gets fetched next.
    pub r: [u32; 16],
    pub pc: u32,
    n: bool,
    z: bool,
    c: bool,
    v: bool,
    thumb: bool,
    ctrl: u32,           // CPSR bits 7, 6 and 4..0 (I, F, mode)
    bank: [[u32; 2]; 6], // r13, r14 of the modes not running
    hi: [u32; 5],        // r8..r12 of FIQ mode, or of the others while in FIQ mode
    spsr: [u32; 6],
    stop: bool,
    pub fault: Option<String>,
    pub quirks: [u64; QUIRKS.len()],
}

impl Default for Arm7 {
    fn default() -> Self {
        Arm7 { r: [0; 16], pc: 0, n: false, z: false, c: false, v: false, thumb: false, ctrl: 0xD3, bank: [[0; 2]; 6],
               hi: [0; 5], spsr: [0; 6], stop: false, fault: None, quirks: [0; QUIRKS.len()] }
    }
}

impl Arm7 {
    pub fn cpsr(&self) -> u32 {
        (self.n as u32) << 31 | (self.z as u32) << 30 | (self.c as u32) << 29 | (self.v as u32) << 28 | self.ctrl
        | (self.thumb as u32) << 5
    }

    pub fn set_cpsr(&mut self, value: u32) {
        let (old, new) = (bank_of(self.ctrl), bank_of(value));
        if old != new {
            self.bank[old] = [self.r[13], self.r[14]];
            if (old == 1) != (new == 1) {
                for i in 0..5 {
                    std::mem::swap(&mut self.r[8 + i], &mut self.hi[i]);
                }
            }
            [self.r[13], self.r[14]] = self.bank[new];
        }
        self.n = value >> 31 != 0;
        self.z = value >> 30 & 1 != 0;
        self.c = value >> 29 & 1 != 0;
        self.v = value >> 28 & 1 != 0;
        self.thumb = value & 0x20 != 0;
        self.ctrl = value & 0xDF;
    }

    /// The current mode's SPSR (the CPSR itself in user and system mode).
    pub fn spsr(&self) -> u32 {
        match bank_of(self.ctrl) {
            0 => self.cpsr(),
            b => self.spsr[b],
        }
    }

    /// Take the FIQ (vector 0x1c) or IRQ (0x18) exception unless it is masked.
    pub fn interrupt(&mut self, fiq: bool) -> bool {
        let cpsr = self.cpsr();
        let (mask, mode, vector) = if fiq { (0x40, 0xD1, 0x1C) } else { (0x80, 0x92, 0x18) };
        if cpsr & mask != 0 {
            return false;
        }
        self.exception(vector, (cpsr & !0x3F) | mode, self.pc.wrapping_add(4));
        true
    }

    fn exception(&mut self, vector: u32, cpsr: u32, lr: u32) {
        let old = self.cpsr();
        self.set_cpsr(cpsr);
        self.spsr[bank_of(cpsr)] = old;
        self.r[14] = lr;
        self.pc = vector;
    }

    #[cold]
    fn undefined(&mut self, addr: u32, op: u32) {
        let state = if self.thumb { "Thumb" } else { "ARM" };
        self.fault = Some(format!("undefined {state} instruction {op:#x} at {addr:#010x}"));
        self.pc = addr;
        self.stop = true;
    }

    #[inline(always)]
    fn quirk(&mut self, which: usize) {
        self.quirks[which] += 1;
    }

    /// Run `count` instructions, fewer on a fault (`fault`, or the bus's own).
    pub fn run<B: Bus + 'static>(&mut self, bus: &mut B, count: usize) {
        let mut left = count;
        while left > 0 {
            if self.thumb {
                while left > 0 && self.thumb {
                    left -= 1;
                    let addr = self.pc;
                    let op = bus.read16(addr);
                    self.pc = addr.wrapping_add(2);
                    self.r[15] = addr.wrapping_add(4);
                    Table::<B>::THUMB[(op >> 6) as usize](self, bus, op);
                    if self.stop | bus.faulted() {
                        return self.halt(addr, bus.faulted());
                    }
                }
            } else {
                while left > 0 && !self.thumb {
                    left -= 1;
                    let addr = self.pc;
                    let op = bus.read32(addr);
                    self.pc = addr.wrapping_add(4);
                    self.r[15] = addr.wrapping_add(8);
                    let cond = op >> 28;
                    if cond == 14 || self.cond(cond) {
                        Table::<B>::ARM[(op >> 16 & 0xFF0 | op >> 4 & 15) as usize](self, bus, op);
                    } else if cond == 15 {
                        self.quirk(Q_NV);
                    }
                    if self.stop | bus.faulted() {
                        return self.halt(addr, bus.faulted());
                    }
                }
            }
        }
    }

    #[cold]
    fn halt(&mut self, addr: u32, bus: bool) {
        if bus {
            self.pc = addr; // as a data abort would report it
        }
        self.stop = false;
    }

    #[inline(always)]
    fn cond(&self, cond: u32) -> bool {
        let flags = (self.n as u32) << 3 | (self.z as u32) << 2 | (self.c as u32) << 1 | self.v as u32;
        COND[cond as usize] >> flags & 1 != 0
    }

    #[inline(always)]
    fn nz(&mut self, res: u32) {
        self.n = res >> 31 != 0;
        self.z = res == 0;
    }

    /// a + b + carry, setting all four flags.
    #[inline(always)]
    fn adds(&mut self, a: u32, b: u32, carry: bool) -> u32 {
        let wide = a as u64 + b as u64 + carry as u64;
        let res = wide as u32;
        self.nz(res);
        self.c = wide >> 32 != 0;
        self.v = (!(a ^ b) & (a ^ res)) >> 31 != 0;
        res
    }

    /// The barrel shifter: result and carry out. `imm` is the immediate
    /// form, where amount 0 means LSL #0, LSR #32, ASR #32 or RRX.
    #[inline(always)]
    fn shift(&self, kind: u32, val: u32, amount: u32, imm: bool) -> (u32, bool) {
        if amount == 0 {
            return match kind {
                _ if !imm => (val, self.c),
                0 => (val, self.c),
                1 => (0, val >> 31 != 0),
                2 => (((val as i32) >> 31) as u32, val >> 31 != 0),
                _ => ((self.c as u32) << 31 | val >> 1, val & 1 != 0),
            };
        }
        match kind {
            0 => match amount {
                1..=31 => (val << amount, val >> (32 - amount) & 1 != 0),
                32 => (0, val & 1 != 0),
                _ => (0, false),
            },
            1 => match amount {
                1..=31 => (val >> amount, val >> (amount - 1) & 1 != 0),
                32 => (0, val >> 31 != 0),
                _ => (0, false),
            },
            2 => match amount {
                1..=31 => (((val as i32) >> amount) as u32, val >> (amount - 1) & 1 != 0),
                _ => (((val as i32) >> 31) as u32, val >> 31 != 0),
            },
            _ => {
                let res = val.rotate_right(amount & 31);
                (res, res >> 31 != 0)
            }
        }
    }

    /// A register as user mode sees it (LDM / STM with the S bit).
    fn user_reg(&mut self, i: usize) -> &mut u32 {
        let bank = bank_of(self.ctrl);
        match i {
            8..=12 if bank == 1 => &mut self.hi[i - 8],
            13 | 14 if bank != 0 => &mut self.bank[0][i - 13],
            _ => &mut self.r[i],
        }
    }

    /// Load pc in the current state (ARMv4T never interworks here).
    #[inline(always)]
    fn jump(&mut self, target: u32) {
        if (target & 1 != 0) != self.thumb {
            self.quirk(Q_PC_LOAD);
        }
        self.pc = target & if self.thumb { !1 } else { !3 };
    }

    #[inline(always)]
    fn load32<B: Bus>(&mut self, bus: &mut B, addr: u32) -> u32 {
        if addr & 3 != 0 {
            self.quirk(Q_UNALIGNED);
        }
        bus.read32(addr & !3).rotate_right((addr & 3) * 8)
    }

    #[inline(always)]
    fn load16<B: Bus>(&mut self, bus: &mut B, addr: u32) -> u32 {
        if addr & 1 != 0 {
            self.quirk(Q_UNALIGNED);
        }
        bus.read16(addr & !1)
    }

    #[inline(always)]
    fn store32<B: Bus>(&mut self, bus: &mut B, addr: u32, value: u32) {
        if addr & 3 != 0 {
            self.quirk(Q_UNALIGNED);
        }
        bus.write32(addr & !3, value);
    }

    #[inline(always)]
    fn store16<B: Bus>(&mut self, bus: &mut B, addr: u32, value: u32) {
        if addr & 1 != 0 {
            self.quirk(Q_UNALIGNED);
        }
        bus.write16(addr & !1, value & 0xFFFF);
    }

    // ---- ARM state ----

    fn a_undefined<B: Bus>(&mut self, _bus: &mut B, op: u32) {
        self.undefined(self.pc.wrapping_sub(4), op);
    }

    /// Data processing. SH: 0..3 shift by immediate (LSL, LSR, ASR, ROR),
    /// 4..7 the same by register, 8 immediate operand.
    fn a_data<B: Bus, const CODE: u32, const S: bool, const SH: u32>(&mut self, _bus: &mut B, op: u32) {
        let rn = (op >> 16 & 15) as usize;
        let rm = (op & 15) as usize;
        let mut a = self.r[rn];
        let (b, carry) = match SH {
            8 => {
                let rot = op >> 7 & 30;
                let b = (op & 0xFF).rotate_right(rot);
                (b, if rot == 0 { self.c } else { b >> 31 != 0 })
            }
            4..=7 => {
                // pc reads as + 12 when the shift amount is a register
                let val = self.r[rm].wrapping_add(if rm == 15 { 4 } else { 0 });
                a = a.wrapping_add(if rn == 15 { 4 } else { 0 });
                self.shift(SH - 4, val, self.r[(op >> 8 & 15) as usize] & 0xFF, false)
            }
            _ => self.shift(SH, self.r[rm], op >> 7 & 31, true),
        };
        // Subtraction is addition of the complement with a carry in.
        let (a, b, carry_in) = match CODE {
            2 | 10 => (a, !b, true),
            3 => (b, !a, true),
            5 => (a, b, self.c),
            6 => (a, !b, self.c),
            7 => (b, !a, self.c),
            _ => (a, b, false),
        };
        let res = match CODE {
            0 | 8 => a & b,
            1 | 9 => a ^ b,
            12 => a | b,
            13 => b,
            14 => a & !b,
            15 => !b,
            _ if S => self.adds(a, b, carry_in),
            _ => a.wrapping_add(b).wrapping_add(carry_in as u32),
        };
        if S && matches!(CODE, 0 | 1 | 8 | 9 | 12..=15) {
            self.nz(res);
            self.c = carry;
        }
        if CODE & 0xC == 8 {
            return; // TST, TEQ, CMP, CMN
        }
        let rd = (op >> 12 & 15) as usize;
        if rd != 15 {
            self.r[rd] = res;
        } else if S {
            let spsr = self.spsr();
            self.set_cpsr(spsr); // exception return
            self.pc = res & if self.thumb { !1 } else { !3 };
        } else {
            self.jump(res);
        }
    }

    fn a_bx<B: Bus>(&mut self, _bus: &mut B, op: u32) {
        if op & 0x0FFF_FFF0 != 0x012F_FF10 {
            return self.undefined(self.pc.wrapping_sub(4), op);
        }
        let target = self.r[(op & 15) as usize];
        self.thumb = target & 1 != 0;
        self.pc = target & if self.thumb { !1 } else { !3 };
    }

    fn a_status<B: Bus, const IMM: bool>(&mut self, _bus: &mut B, op: u32) {
        let operand = if IMM { (op & 0xFF).rotate_right(op >> 7 & 30) } else { self.r[(op & 15) as usize] };
        self.arm_status(op, operand);
    }

    fn a_branch<B: Bus, const LINK: bool>(&mut self, _bus: &mut B, op: u32) {
        if LINK {
            self.r[14] = self.pc;
        }
        self.pc = self.r[15].wrapping_add(((op << 8) as i32 >> 6) as u32);
    }

    fn a_swi<B: Bus>(&mut self, _bus: &mut B, _op: u32) {
        let cpsr = self.cpsr();
        self.exception(0x08, (cpsr & !0x3F) | 0x80 | MODE_SVC, self.pc);
    }

    /// LDR, STR, LDRB, STRB.
    fn a_single<B: Bus, const REG: bool, const LOAD: bool, const BYTE: bool>(&mut self, bus: &mut B, op: u32) {
        let offset = if REG { self.shift(op >> 5 & 3, self.r[(op & 15) as usize], op >> 7 & 31, true).0 } else { op & 0xFFF };
        let rn = (op >> 16 & 15) as usize;
        let rd = (op >> 12 & 15) as usize;
        let base = self.r[rn];
        let moved = if op & 0x0080_0000 != 0 { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let pre = op & 0x0100_0000 != 0;
        let addr = if pre { moved } else { base };
        let writeback = !pre || op & 0x0020_0000 != 0;
        if LOAD {
            let value = if BYTE { bus.read8(addr) } else { self.load32(bus, addr) };
            if writeback {
                self.r[rn] = moved;
            }
            if rd == 15 {
                self.jump(value);
            } else {
                self.r[rd] = value;
            }
        } else {
            let mut value = self.r[rd];
            if rd == 15 {
                value = value.wrapping_add(4);
                self.quirk(Q_PC_STORE);
            }
            if BYTE {
                bus.write8(addr, value & 0xFF);
            } else {
                self.store32(bus, addr, value);
            }
            if writeback {
                self.r[rn] = moved;
            }
        }
    }

    /// MRS and MSR.
    fn arm_status(&mut self, op: u32, operand: u32) {
        let saved = op & 0x0040_0000 != 0;
        if op & 0x0020_0000 == 0 {
            self.r[(op >> 12 & 15) as usize] = if saved { self.spsr() } else { self.cpsr() };
            return;
        }
        let mut mask = 0;
        for (bit, field) in [(16, 0x0000_00FF), (17, 0x0000_FF00), (18, 0x00FF_0000), (19, 0xFF00_0000u32)] {
            if op >> bit & 1 != 0 {
                mask |= field;
            }
        }
        if saved {
            let bank = bank_of(self.ctrl);
            if bank != 0 {
                self.spsr[bank] = (self.spsr[bank] & !mask) | (operand & mask);
            }
        } else {
            if self.ctrl & 0x1F == 0x10 {
                mask &= 0xFF00_0000; // user mode changes the flags only
            }
            mask &= !0x20; // the T bit only changes through BX and exception returns
            let cpsr = self.cpsr();
            self.set_cpsr((cpsr & !mask) | (operand & mask));
        }
    }

    /// Multiplies, SWP and the halfword / signed-byte transfers.
    fn arm_extra<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let s = op & 0x0010_0000 != 0;
        let rm = self.r[(op & 15) as usize];
        if op & 0x60 != 0 {
            // LDRH, STRH, LDRSB, LDRSH
            let rn = (op >> 16 & 15) as usize;
            let rd = (op >> 12 & 15) as usize;
            let offset = if op & 0x0040_0000 != 0 { (op >> 4 & 0xF0) | (op & 15) } else { rm };
            let base = self.r[rn];
            let moved = if op & 0x0080_0000 != 0 { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
            let pre = op & 0x0100_0000 != 0;
            let addr = if pre { moved } else { base };
            let writeback = !pre || op & 0x0020_0000 != 0;
            if !s {
                if op & 0x60 != 0x20 {
                    return self.undefined(self.pc.wrapping_sub(4), op); // LDRD / STRD are ARMv5TE
                }
                self.store16(bus, addr, self.r[rd]);
                if writeback {
                    self.r[rn] = moved;
                }
                return;
            }
            let value = match op & 0x60 {
                0x20 => self.load16(bus, addr),
                0x40 => bus.read8(addr) as u8 as i8 as u32,
                _ => self.load16(bus, addr) as u16 as i16 as u32,
            };
            if writeback {
                self.r[rn] = moved;
            }
            if rd == 15 {
                self.jump(value);
            } else {
                self.r[rd] = value;
            }
            return;
        }
        let rs = self.r[(op >> 8 & 15) as usize];
        match op >> 23 & 31 {
            0 if op & 0x00C0_0000 == 0 => {
                // MUL, MLA (the carry flag is left alone, as under Unicorn)
                let mut res = rm.wrapping_mul(rs);
                if op & 0x0020_0000 != 0 {
                    res = res.wrapping_add(self.r[(op >> 12 & 15) as usize]);
                }
                self.r[(op >> 16 & 15) as usize] = res;
                if s {
                    self.nz(res);
                }
            }
            1 => {
                // UMULL, UMLAL, SMULL, SMLAL
                let (hi, lo) = ((op >> 16 & 15) as usize, (op >> 12 & 15) as usize);
                let mut res = if op & 0x0040_0000 != 0 {
                    (rm as i32 as i64).wrapping_mul(rs as i32 as i64) as u64
                } else {
                    rm as u64 * rs as u64
                };
                if op & 0x0020_0000 != 0 {
                    res = res.wrapping_add((self.r[hi] as u64) << 32 | self.r[lo] as u64);
                }
                self.r[lo] = res as u32;
                self.r[hi] = (res >> 32) as u32;
                if s {
                    self.n = res >> 63 != 0;
                    self.z = res == 0;
                }
            }
            2 if op & 0x00B0_0000 == 0 => {
                // SWP, SWPB
                let addr = self.r[(op >> 16 & 15) as usize];
                let old = if op & 0x0040_0000 != 0 {
                    let old = bus.read8(addr);
                    bus.write8(addr, rm & 0xFF);
                    old
                } else {
                    let old = self.load32(bus, addr);
                    bus.write32(addr & !3, rm);
                    old
                };
                self.r[(op >> 12 & 15) as usize] = old;
            }
            _ => self.undefined(self.pc.wrapping_sub(4), op),
        }
    }

    /// LDM, STM.
    fn arm_block<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let rn = (op >> 16 & 15) as usize;
        let list = op & 0xFFFF;
        if list == 0 {
            return self.undefined(self.pc.wrapping_sub(4), op); // unpredictable
        }
        let count = list.count_ones() * 4;
        let (pre, up) = (op & 0x0100_0000 != 0, op & 0x0080_0000 != 0);
        let user = op & 0x0040_0000 != 0;
        let writeback = op & 0x0020_0000 != 0;
        let base = self.r[rn];
        let (mut addr, moved) = if up {
            (base.wrapping_add(if pre { 4 } else { 0 }), base.wrapping_add(count))
        } else {
            (base.wrapping_sub(count).wrapping_add(if pre { 0 } else { 4 }), base.wrapping_sub(count))
        };
        if op & 0x0010_0000 != 0 {
            if writeback {
                self.r[rn] = moved; // a loaded base overwrites this below
            }
            let banked = user && list & 0x8000 == 0;
            for i in 0..15 {
                if list >> i & 1 != 0 {
                    let value = self.load32(bus, addr);
                    addr = addr.wrapping_add(4);
                    if banked {
                        *self.user_reg(i) = value;
                    } else {
                        self.r[i] = value;
                    }
                }
            }
            if list & 0x8000 != 0 {
                let value = self.load32(bus, addr);
                if user {
                    let spsr = self.spsr();
                    self.set_cpsr(spsr); // exception return
                    self.pc = value & if self.thumb { !1 } else { !3 };
                } else {
                    self.jump(value);
                }
            }
        } else {
            let first = list.trailing_zeros() as usize;
            for i in 0..16 {
                if list >> i & 1 != 0 {
                    let mut value = if user { *self.user_reg(i) } else { self.r[i] };
                    if i == 15 {
                        value = value.wrapping_add(4);
                        self.quirk(Q_PC_STORE);
                    } else if i == rn && writeback && i != first {
                        value = moved;
                        self.quirk(Q_STM_BASE);
                    }
                    self.store32(bus, addr, value);
                    addr = addr.wrapping_add(4);
                }
            }
            if writeback {
                self.r[rn] = moved;
            }
        }
    }

    // ---- Thumb state ----

    fn t_undefined<B: Bus>(&mut self, _bus: &mut B, op: u32) {
        self.undefined(self.pc.wrapping_sub(2), op);
    }

    fn t_shift<B: Bus, const KIND: u32>(&mut self, _bus: &mut B, op: u32) {
        let (res, carry) = self.shift(KIND, self.r[(op >> 3 & 7) as usize], op >> 6 & 31, true);
        self.r[(op & 7) as usize] = res;
        self.nz(res);
        self.c = carry;
    }

    /// ADD / SUB with a register or a 3-bit immediate.
    fn t_add<B: Bus, const SUB: bool, const IMM: bool>(&mut self, _bus: &mut B, op: u32) {
        let b = if IMM { op >> 6 & 7 } else { self.r[(op >> 6 & 7) as usize] };
        let a = self.r[(op >> 3 & 7) as usize];
        self.r[(op & 7) as usize] = if SUB { self.adds(a, !b, true) } else { self.adds(a, b, false) };
    }

    /// MOV, CMP, ADD, SUB with an 8-bit immediate.
    fn t_imm<B: Bus, const CODE: u32>(&mut self, _bus: &mut B, op: u32) {
        let rd = (op >> 8 & 7) as usize;
        let imm = op & 0xFF;
        match CODE {
            0 => {
                self.r[rd] = imm;
                self.nz(imm);
            }
            1 => {
                self.adds(self.r[rd], !imm, true);
            }
            2 => self.r[rd] = self.adds(self.r[rd], imm, false),
            _ => self.r[rd] = self.adds(self.r[rd], !imm, true),
        }
    }

    fn t_alu<B: Bus, const CODE: u32>(&mut self, _bus: &mut B, op: u32) {
        let rd = (op & 7) as usize;
        let (a, b) = (self.r[rd], self.r[(op >> 3 & 7) as usize]);
        let res = match CODE {
            0 | 8 => a & b,
            1 => a ^ b,
            2 | 3 | 4 | 7 => {
                let (res, carry) = self.shift([0, 0, 0, 1, 2, 0, 0, 3][CODE as usize], a, b & 0xFF, false);
                self.c = carry;
                res
            }
            5 => return self.r[rd] = self.adds(a, b, self.c),
            6 => return self.r[rd] = self.adds(a, !b, self.c),
            9 => return self.r[rd] = self.adds(0, !b, true),
            10 => {
                self.adds(a, !b, true);
                return;
            }
            11 => {
                self.adds(a, b, false);
                return;
            }
            12 => a | b,
            13 => a.wrapping_mul(b),
            14 => a & !b,
            _ => !b,
        };
        self.nz(res);
        if CODE != 8 {
            self.r[rd] = res;
        }
    }

    /// ADD, CMP, MOV on any register (0..2) and BX (3).
    fn t_high<B: Bus, const CODE: u32>(&mut self, _bus: &mut B, op: u32) {
        let rd = (op & 7 | op >> 4 & 8) as usize;
        let b = self.r[(op >> 3 & 15) as usize];
        match CODE {
            0 | 2 => {
                let res = if CODE == 0 { self.r[rd].wrapping_add(b) } else { b };
                if rd == 15 {
                    self.jump(res | 1);
                } else {
                    self.r[rd] = res;
                }
            }
            1 => {
                self.adds(self.r[rd], !b, true);
            }
            _ => {
                self.thumb = b & 1 != 0;
                self.pc = b & if self.thumb { !1 } else { !3 };
            }
        }
    }

    /// Loads and stores. FORM: 0 [rb, ro], 1 [rb, #imm5 * size], 2 [sp, #imm8 * 4],
    /// 3 [pc, #imm8 * 4]. KIND: 0 STR, 1 STRH, 2 STRB, 3 LDRSB, 4 LDR, 5 LDRH,
    /// 6 LDRB, 7 LDRSH.
    fn t_mem<B: Bus, const FORM: u32, const KIND: u32>(&mut self, bus: &mut B, op: u32) {
        let (rd, addr) = match FORM {
            0 => ((op & 7) as usize, self.r[(op >> 3 & 7) as usize].wrapping_add(self.r[(op >> 6 & 7) as usize])),
            1 => {
                let scale = match KIND {
                    0 | 4 => 2,
                    1 | 5 => 1,
                    _ => 0,
                };
                ((op & 7) as usize, self.r[(op >> 3 & 7) as usize].wrapping_add((op >> 6 & 31) << scale))
            }
            2 => ((op >> 8 & 7) as usize, self.r[13].wrapping_add((op & 0xFF) << 2)),
            _ => ((op >> 8 & 7) as usize, (self.r[15] & !3).wrapping_add((op & 0xFF) << 2)),
        };
        match KIND {
            0 => self.store32(bus, addr, self.r[rd]),
            1 => self.store16(bus, addr, self.r[rd]),
            2 => bus.write8(addr, self.r[rd] & 0xFF),
            3 => self.r[rd] = bus.read8(addr) as u8 as i8 as u32,
            4 => self.r[rd] = self.load32(bus, addr),
            5 => self.r[rd] = self.load16(bus, addr),
            6 => self.r[rd] = bus.read8(addr),
            _ => self.r[rd] = self.load16(bus, addr) as u16 as i16 as u32,
        }
    }

    /// ADD rd, pc / sp, #imm8 * 4.
    fn t_address<B: Bus, const SP: bool>(&mut self, _bus: &mut B, op: u32) {
        let base = if SP { self.r[13] } else { self.r[15] & !3 };
        self.r[(op >> 8 & 7) as usize] = base.wrapping_add((op & 0xFF) << 2);
    }

    fn t_sp<B: Bus>(&mut self, _bus: &mut B, op: u32) {
        let offset = (op & 0x7F) << 2;
        self.r[13] = if op & 0x80 != 0 { self.r[13].wrapping_sub(offset) } else { self.r[13].wrapping_add(offset) };
    }

    fn t_stmia<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let rb = (op >> 8 & 7) as usize;
        let list = op & 0xFF;
        if list == 0 {
            return self.undefined(self.pc.wrapping_sub(2), op);
        }
        let mut addr = self.r[rb];
        let moved = addr.wrapping_add(list.count_ones() * 4);
        let first = list.trailing_zeros() as usize;
        for i in 0..8 {
            if list >> i & 1 != 0 {
                let mut value = self.r[i];
                if i == rb && i != first {
                    value = moved;
                    self.quirk(Q_STM_BASE);
                }
                self.store32(bus, addr, value);
                addr = addr.wrapping_add(4);
            }
        }
        self.r[rb] = moved;
    }

    fn t_ldmia<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let rb = (op >> 8 & 7) as usize;
        let list = op & 0xFF;
        if list == 0 {
            return self.undefined(self.pc.wrapping_sub(2), op);
        }
        let mut addr = self.r[rb];
        self.r[rb] = addr.wrapping_add(list.count_ones() * 4);
        for i in 0..8 {
            if list >> i & 1 != 0 {
                self.r[i] = self.load32(bus, addr);
                addr = addr.wrapping_add(4);
            }
        }
    }

    fn t_bcond<B: Bus>(&mut self, _bus: &mut B, op: u32) {
        if self.cond(op >> 8 & 15) {
            self.pc = self.r[15].wrapping_add(((op as u8 as i8 as i32) << 1) as u32);
        }
    }

    fn t_branch<B: Bus>(&mut self, _bus: &mut B, op: u32) {
        self.pc = self.r[15].wrapping_add((((op << 21) as i32) >> 20) as u32);
    }

    /// BL, first half; with its second half when that follows, as one instruction.
    fn t_bl<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let high = self.r[15].wrapping_add((((op << 21) as i32) >> 9) as u32);
        let next = bus.read16(self.pc);
        if next >> 11 == 31 {
            self.r[14] = self.pc.wrapping_add(2) | 1;
            self.pc = high.wrapping_add((next & 0x7FF) << 1) & !1;
        } else {
            self.r[14] = high;
        }
    }

    fn t_bl_low<B: Bus>(&mut self, _bus: &mut B, op: u32) {
        let next = self.pc | 1;
        self.pc = self.r[14].wrapping_add((op & 0x7FF) << 1) & !1;
        self.r[14] = next;
    }

    fn thumb_push<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let list = op & 0x1FF;
        if list == 0 {
            return self.undefined(self.pc.wrapping_sub(2), op);
        }
        let mut addr = self.r[13].wrapping_sub(list.count_ones() * 4);
        self.r[13] = addr;
        for i in 0..9 {
            if list >> i & 1 != 0 {
                self.store32(bus, addr, self.r[if i == 8 { 14 } else { i }]);
                addr = addr.wrapping_add(4);
            }
        }
    }

    fn thumb_pop<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let list = op & 0x1FF;
        if list == 0 {
            return self.undefined(self.pc.wrapping_sub(2), op);
        }
        let mut addr = self.r[13];
        for i in 0..8 {
            if list >> i & 1 != 0 {
                self.r[i] = self.load32(bus, addr);
                addr = addr.wrapping_add(4);
            }
        }
        if list & 0x100 != 0 {
            let target = self.load32(bus, addr);
            addr = addr.wrapping_add(4);
            self.jump(target);
        }
        self.r[13] = addr;
    }
}

type Op<B> = fn(&mut Arm7, &mut B, u32);

/// The handler for each instruction pattern, decoded once at compile time:
/// ARM by bits 27..20 and 7..4, Thumb by bits 15..6. One indirect call per
/// instruction, and every handler is small straight code.
struct Table<B>(std::marker::PhantomData<B>);

macro_rules! by_shift {
    ($sh:expr, $code:literal, $s:literal) => {
        match $sh {
            0 => Arm7::a_data::<B, $code, $s, 0> as Op<B>,
            1 => Arm7::a_data::<B, $code, $s, 1>,
            2 => Arm7::a_data::<B, $code, $s, 2>,
            3 => Arm7::a_data::<B, $code, $s, 3>,
            4 => Arm7::a_data::<B, $code, $s, 4>,
            5 => Arm7::a_data::<B, $code, $s, 5>,
            6 => Arm7::a_data::<B, $code, $s, 6>,
            7 => Arm7::a_data::<B, $code, $s, 7>,
            _ => Arm7::a_data::<B, $code, $s, 8>,
        }
    };
}

macro_rules! by_kind {
    ($kind:expr, $form:literal) => {
        match $kind {
            0 => Arm7::t_mem::<B, $form, 0> as Op<B>,
            1 => Arm7::t_mem::<B, $form, 1>,
            2 => Arm7::t_mem::<B, $form, 2>,
            3 => Arm7::t_mem::<B, $form, 3>,
            4 => Arm7::t_mem::<B, $form, 4>,
            5 => Arm7::t_mem::<B, $form, 5>,
            6 => Arm7::t_mem::<B, $form, 6>,
            _ => Arm7::t_mem::<B, $form, 7>,
        }
    };
}

impl<B: Bus + 'static> Table<B> {
    const ARM: &'static [Op<B>; 4096] = &{
        let mut table = [Arm7::a_undefined::<B> as Op<B>; 4096];
        let mut i = 0;
        while i < 4096 {
            table[i] = Self::arm(i >> 4, i & 15);
            i += 1;
        }
        table
    };

    const THUMB: &'static [Op<B>; 1024] = &{
        let mut table = [Arm7::t_undefined::<B> as Op<B>; 1024];
        let mut i = 0;
        while i < 1024 {
            table[i] = Self::thumb(i);
            i += 1;
        }
        table
    };

    /// `hi` is bits 27..20 of the instruction, `lo` bits 7..4.
    const fn arm(hi: usize, lo: usize) -> Op<B> {
        match hi >> 5 {
            0 | 1 => {
                let imm = hi >> 5 == 1;
                if !imm && lo & 9 == 9 {
                    return Arm7::arm_extra::<B>; // multiplies, SWP, halfword transfers
                }
                if hi & 0x19 == 0x10 {
                    // TST, TEQ, CMP, CMN without S: the status register instructions and BX
                    return match (imm, hi & 2 != 0, lo) {
                        (true, true, _) => Arm7::a_status::<B, true>,
                        (false, _, 0) => Arm7::a_status::<B, false>,
                        (false, true, 1) if hi == 0x12 => Arm7::a_bx::<B>,
                        _ => Arm7::a_undefined::<B>,
                    };
                }
                let shift = if imm { 8 } else { lo >> 1 & 3 | (lo & 1) << 2 };
                match hi & 0x1F {
                    0x00 => by_shift!(shift, 0, false),
                    0x01 => by_shift!(shift, 0, true),
                    0x02 => by_shift!(shift, 1, false),
                    0x03 => by_shift!(shift, 1, true),
                    0x04 => by_shift!(shift, 2, false),
                    0x05 => by_shift!(shift, 2, true),
                    0x06 => by_shift!(shift, 3, false),
                    0x07 => by_shift!(shift, 3, true),
                    0x08 => by_shift!(shift, 4, false),
                    0x09 => by_shift!(shift, 4, true),
                    0x0A => by_shift!(shift, 5, false),
                    0x0B => by_shift!(shift, 5, true),
                    0x0C => by_shift!(shift, 6, false),
                    0x0D => by_shift!(shift, 6, true),
                    0x0E => by_shift!(shift, 7, false),
                    0x0F => by_shift!(shift, 7, true),
                    0x11 => by_shift!(shift, 8, true),
                    0x13 => by_shift!(shift, 9, true),
                    0x15 => by_shift!(shift, 10, true),
                    0x17 => by_shift!(shift, 11, true),
                    0x18 => by_shift!(shift, 12, false),
                    0x19 => by_shift!(shift, 12, true),
                    0x1A => by_shift!(shift, 13, false),
                    0x1B => by_shift!(shift, 13, true),
                    0x1C => by_shift!(shift, 14, false),
                    0x1D => by_shift!(shift, 14, true),
                    0x1E => by_shift!(shift, 15, false),
                    _ => by_shift!(shift, 15, true),
                }
            }
            2 | 3 => match (hi >> 5 == 3, hi & 1 != 0, hi & 4 != 0) {
                (true, ..) if lo & 1 != 0 => Arm7::a_undefined::<B>,
                (false, false, false) => Arm7::a_single::<B, false, false, false>,
                (false, false, true) => Arm7::a_single::<B, false, false, true>,
                (false, true, false) => Arm7::a_single::<B, false, true, false>,
                (false, true, true) => Arm7::a_single::<B, false, true, true>,
                (true, false, false) => Arm7::a_single::<B, true, false, false>,
                (true, false, true) => Arm7::a_single::<B, true, false, true>,
                (true, true, false) => Arm7::a_single::<B, true, true, false>,
                (true, true, true) => Arm7::a_single::<B, true, true, true>,
            },
            4 => Arm7::arm_block::<B>,
            5 if hi & 0x10 != 0 => Arm7::a_branch::<B, true>,
            5 => Arm7::a_branch::<B, false>,
            7 if hi & 0x10 != 0 => Arm7::a_swi::<B>,
            _ => Arm7::a_undefined::<B>, // coprocessor
        }
    }

    /// `i` is bits 15..6 of the instruction.
    const fn thumb(i: usize) -> Op<B> {
        match i >> 5 {
            0 => Arm7::t_shift::<B, 0>,
            1 => Arm7::t_shift::<B, 1>,
            2 => Arm7::t_shift::<B, 2>,
            3 => match i >> 3 & 3 {
                0 => Arm7::t_add::<B, false, false>,
                1 => Arm7::t_add::<B, true, false>,
                2 => Arm7::t_add::<B, false, true>,
                _ => Arm7::t_add::<B, true, true>,
            },
            4 => Arm7::t_imm::<B, 0>,
            5 => Arm7::t_imm::<B, 1>,
            6 => Arm7::t_imm::<B, 2>,
            7 => Arm7::t_imm::<B, 3>,
            8 => match i & 0x1F {
                0 => Arm7::t_alu::<B, 0>,
                1 => Arm7::t_alu::<B, 1>,
                2 => Arm7::t_alu::<B, 2>,
                3 => Arm7::t_alu::<B, 3>,
                4 => Arm7::t_alu::<B, 4>,
                5 => Arm7::t_alu::<B, 5>,
                6 => Arm7::t_alu::<B, 6>,
                7 => Arm7::t_alu::<B, 7>,
                8 => Arm7::t_alu::<B, 8>,
                9 => Arm7::t_alu::<B, 9>,
                10 => Arm7::t_alu::<B, 10>,
                11 => Arm7::t_alu::<B, 11>,
                12 => Arm7::t_alu::<B, 12>,
                13 => Arm7::t_alu::<B, 13>,
                14 => Arm7::t_alu::<B, 14>,
                15 => Arm7::t_alu::<B, 15>,
                16..=19 => Arm7::t_high::<B, 0>,
                20..=23 => Arm7::t_high::<B, 1>,
                24..=27 => Arm7::t_high::<B, 2>,
                28 | 29 => Arm7::t_high::<B, 3>,
                _ => Arm7::t_undefined::<B>, // BLX is ARMv5T
            },
            9 => Arm7::t_mem::<B, 3, 4>,
            10 | 11 => by_kind!(i >> 3 & 7, 0),
            12 => Arm7::t_mem::<B, 1, 0>,
            13 => Arm7::t_mem::<B, 1, 4>,
            14 => Arm7::t_mem::<B, 1, 2>,
            15 => Arm7::t_mem::<B, 1, 6>,
            16 => Arm7::t_mem::<B, 1, 1>,
            17 => Arm7::t_mem::<B, 1, 5>,
            18 => Arm7::t_mem::<B, 2, 0>,
            19 => Arm7::t_mem::<B, 2, 4>,
            20 => Arm7::t_address::<B, false>,
            21 => Arm7::t_address::<B, true>,
            22 | 23 => match i >> 2 & 15 {
                0 => Arm7::t_sp::<B>,
                4 | 5 => Arm7::thumb_push::<B>,
                12 | 13 => Arm7::thumb_pop::<B>,
                _ => Arm7::t_undefined::<B>,
            },
            24 => Arm7::t_stmia::<B>,
            25 => Arm7::t_ldmia::<B>,
            26 | 27 => match i >> 2 & 15 {
                14 => Arm7::t_undefined::<B>,
                15 => Arm7::a_swi::<B>,
                _ => Arm7::t_bcond::<B>,
            },
            28 => Arm7::t_branch::<B>,
            30 => Arm7::t_bl::<B>,
            31 => Arm7::t_bl_low::<B>,
            _ => Arm7::t_undefined::<B>, // 29: BLX suffix, ARMv5T
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram(Vec<u8>, bool);

    impl Bus for Ram {
        fn read8(&mut self, a: u32) -> u32 {
            self.0[a as usize] as u32
        }
        fn read16(&mut self, a: u32) -> u32 {
            self.read8(a) | self.read8(a + 1) << 8
        }
        fn read32(&mut self, a: u32) -> u32 {
            self.read16(a) | self.read16(a + 2) << 16
        }
        fn write8(&mut self, a: u32, v: u32) {
            self.0[a as usize] = v as u8;
        }
        fn write16(&mut self, a: u32, v: u32) {
            self.write8(a, v);
            self.write8(a + 1, v >> 8);
        }
        fn write32(&mut self, a: u32, v: u32) {
            self.write16(a, v);
            self.write16(a + 2, v >> 16);
        }
        fn faulted(&self) -> bool {
            self.1
        }
    }

    const CODE: u32 = 0x100;

    /// Run ARM instructions placed at CODE, after `setup`.
    fn arm(code: &[u32], setup: impl FnOnce(&mut Arm7, &mut Ram)) -> (Arm7, Ram) {
        let mut ram = Ram(vec![0; 0x1000], false);
        for (i, &op) in code.iter().enumerate() {
            ram.write32(CODE + 4 * i as u32, op);
        }
        let mut cpu = Arm7 { pc: CODE, ..Default::default() };
        setup(&mut cpu, &mut ram);
        cpu.run(&mut ram, code.len());
        assert_eq!(cpu.fault, None);
        (cpu, ram)
    }

    fn thumb(code: &[u16], count: usize, setup: impl FnOnce(&mut Arm7, &mut Ram)) -> (Arm7, Ram) {
        let mut ram = Ram(vec![0; 0x1000], false);
        for (i, &op) in code.iter().enumerate() {
            ram.write16(CODE + 2 * i as u32, op as u32);
        }
        let mut cpu = Arm7 { pc: CODE, thumb: true, ..Default::default() };
        setup(&mut cpu, &mut ram);
        cpu.run(&mut ram, count);
        assert_eq!(cpu.fault, None);
        (cpu, ram)
    }

    #[test]
    fn shifter_carry() {
        // MOVS r0, r1, LSR #32
        let (c, _) = arm(&[0xE1B0_0021], |c, _| c.r[1] = 0x8000_0000);
        assert_eq!((c.r[0], c.c, c.z), (0, true, true));
        // MOVS r0, r1, ASR #32
        let (c, _) = arm(&[0xE1B0_0041], |c, _| c.r[1] = 0x8000_0000);
        assert_eq!((c.r[0], c.c, c.n), (0xFFFF_FFFF, true, true));
        // MOVS r0, r1, RRX
        let (c, _) = arm(&[0xE1B0_0061], |c, _| (c.r[1], c.c) = (3, true));
        assert_eq!((c.r[0], c.c), (0x8000_0001, true));
        // MOVS r0, r1, LSL #0 keeps the carry
        let (c, _) = arm(&[0xE1B0_0001], |c, _| (c.r[1], c.c) = (5, true));
        assert_eq!((c.r[0], c.c), (5, true));
        // MOVS r0, r1, LSL r2 with r2 = 0, 32, 33
        for (amount, res, carry) in [(0, 1, true), (32, 0, true), (33, 0, false), (31, 0x8000_0000, false)] {
            let (c, _) = arm(&[0xE1B0_0211], |c, _| (c.r[1], c.r[2], c.c) = (1, amount, true));
            assert_eq!((c.r[0], c.c), (res, carry), "LSL by {amount}");
        }
        // MOVS r0, r1, ROR r2 with r2 = 32: value unchanged, carry = bit 31
        let (c, _) = arm(&[0xE1B0_0271], |c, _| (c.r[1], c.r[2]) = (0x8000_0001, 32));
        assert_eq!((c.r[0], c.c), (0x8000_0001, true));
        // MOVS r0, #0xff000000 takes the carry from bit 31
        let (c, _) = arm(&[0xE3B0_04FF], |_, _| ());
        assert_eq!((c.r[0], c.c), (0xFF00_0000, true));
    }

    #[test]
    fn arithmetic_flags() {
        // SUBS r0, r1, r2: 5 - 5, 0 - 1, min - 1
        for (a, b, res, flags) in [(5, 5, 0, (false, true, true, false)), (0, 1, 0xFFFF_FFFF, (true, false, false, false)),
                                   (0x8000_0000, 1, 0x7FFF_FFFF, (false, false, true, true))]
        {
            let (c, _) = arm(&[0xE051_0002], |c, _| (c.r[1], c.r[2]) = (a, b));
            assert_eq!((c.r[0], (c.n, c.z, c.c, c.v)), (res, flags));
        }
        // ADDS 0x7fffffff + 1 overflows; ADC and SBC use the carry; RSB reverses
        let (c, _) = arm(&[0xE091_0002], |c, _| (c.r[1], c.r[2]) = (0x7FFF_FFFF, 1));
        assert_eq!((c.r[0], c.v, c.c), (0x8000_0000, true, false));
        let (c, _) = arm(&[0xE0A1_0002], |c, _| (c.r[1], c.r[2], c.c) = (1, 2, true));
        assert_eq!(c.r[0], 4);
        let (c, _) = arm(&[0xE0C1_0002], |c, _| (c.r[1], c.r[2], c.c) = (5, 2, false));
        assert_eq!(c.r[0], 2);
        let (c, _) = arm(&[0xE261_0000], |c, _| c.r[1] = 7); // RSB r0, r1, #0
        assert_eq!(c.r[0], (-7i32) as u32);
        // ADD without S leaves the flags
        let (c, _) = arm(&[0xE081_0002], |c, _| (c.r[1], c.r[2], c.z, c.c) = (0xFFFF_FFFF, 1, false, false));
        assert_eq!((c.r[0], c.z, c.c), (0, false, false));
    }

    #[test]
    fn pc_reads() {
        let (c, _) = arm(&[0xE1A0_000F], |_, _| ()); // MOV r0, pc
        assert_eq!(c.r[0], CODE + 8);
        let (c, _) = arm(&[0xE08F_0211], |_, _| ()); // ADD r0, pc, r1, LSL r2
        assert_eq!(c.r[0], CODE + 12);
        let (c, mut r) = arm(&[0xE581_F000], |c, _| c.r[1] = 0x800); // STR pc, [r1]
        assert_eq!((r.read32(0x800), c.quirks[Q_PC_STORE]), (CODE + 12, 1));
        let (c, _) = thumb(&[0x4678, 0xA101], 2, |_, _| ()); // MOV r0, pc; ADD r1, pc, #4
        assert_eq!((c.r[0], c.r[1]), (CODE + 4, ((CODE + 6) & !3) + 4));
    }

    #[test]
    fn loads_and_stores() {
        // LDR r0, [r1] unaligned rotates
        let (c, _) = arm(&[0xE591_0000], |c, r| {
            c.r[1] = 0x801;
            r.write32(0x800, 0x1122_3344);
        });
        assert_eq!((c.r[0], c.quirks[Q_UNALIGNED]), (0x4411_2233, 1));
        // LDR r0, [r1], #4 then LDR r2, [r1, #-4]!
        let (c, _) = arm(&[0xE491_0004, 0xE531_2004], |c, r| {
            c.r[1] = 0x800;
            r.write32(0x800, 77);
        });
        assert_eq!((c.r[0], c.r[1], c.r[2]), (77, 0x800, 77));
        // LDRSH r0, [r1]; LDRSB r2, [r1]; LDRH r3, [r1, #2]; STRH r0, [r1, #4]
        let (c, mut r) = arm(&[0xE1D1_00F0, 0xE1D1_20D0, 0xE1D1_30B2, 0xE1C1_00B4], |c, r| {
            c.r[1] = 0x800;
            r.write32(0x800, 0x1234_8081);
        });
        assert_eq!((c.r[0], c.r[2], c.r[3], r.read32(0x804)), (0xFFFF_8081, 0xFFFF_FF81, 0x1234, 0x8081));
        // LDMIA r1!, {r0, r1}: the loaded base wins
        let (c, _) = arm(&[0xE8B1_0003], |c, r| {
            c.r[1] = 0x800;
            r.write32(0x800, 1);
            r.write32(0x804, 2);
        });
        assert_eq!((c.r[0], c.r[1]), (1, 2));
        // STMDB sp!, {r0, r1, lr}; LDMIA sp!, {r2, r3, pc}
        let (c, mut r) = arm(&[0xE92D_4003, 0xE8BD_800C], |c, _| (c.r[0], c.r[1], c.r[13], c.r[14]) = (10, 11, 0x900, 0x200));
        assert_eq!((r.read32(0x8F4), r.read32(0x8F8), r.read32(0x8FC)), (10, 11, 0x200));
        assert_eq!((c.r[2], c.r[3], c.r[13], c.pc), (10, 11, 0x900, 0x200));
        // SWP r2, r0, [r1]
        let (c, mut r) = arm(&[0xE101_2090], |c, r| {
            (c.r[0], c.r[1]) = (5, 0x800);
            r.write32(0x800, 9);
        });
        assert_eq!((c.r[2], r.read32(0x800)), (9, 5));
    }

    #[test]
    fn multiplies() {
        let setup = |c: &mut Arm7, _: &mut Ram| (c.r[2], c.r[3]) = (0xFFFF_FFFE, 3);
        let (c, _) = arm(&[0xE081_0392], setup); // UMULL r0, r1, r2, r3
        assert_eq!((c.r[1], c.r[0]), (2, 0xFFFF_FFFA));
        let (c, _) = arm(&[0xE0C1_0392], setup); // SMULL
        assert_eq!((c.r[1], c.r[0]), (0xFFFF_FFFF, (-6i32) as u32));
        let (c, _) = arm(&[0xE0E1_0392], |c, _| (c.r[0], c.r[1], c.r[2], c.r[3]) = (10, 0, 0xFFFF_FFFE, 3)); // SMLAL
        assert_eq!((c.r[1], c.r[0]), (0, 4));
        let (c, _) = arm(&[0xE020_1392], |c, _| (c.r[1], c.r[2], c.r[3]) = (100, 6, 7)); // MLA r0, r2, r3, r1
        assert_eq!(c.r[0], 142);
    }

    #[test]
    fn interworking() {
        // BX r0 to Thumb, MOV r1, #5, BX r2 back to ARM
        let (c, _) = arm(&[0xE12F_FF10, 0, 0], |c, r| {
            (c.r[0], c.r[2]) = (0x201, 0x300);
            r.write16(0x200, 0x2105);
            r.write16(0x202, 0x4710);
        });
        assert_eq!((c.r[1], c.pc, c.thumb), (5, 0x300, false));
        // BL as one instruction: lr = return | 1
        let (c, _) = thumb(&[0xF000, 0xF802], 1, |_, _| ());
        assert_eq!((c.pc, c.r[14]), (CODE + 8, (CODE + 4) | 1));
        // backwards BL
        let (c, _) = thumb(&[0xF7FF, 0xFFFE], 1, |_, _| ());
        assert_eq!(c.pc, CODE);
        // POP {r0, pc} stays in Thumb
        let (c, _) = thumb(&[0xBD01], 1, |c, r| {
            c.r[13] = 0x800;
            r.write32(0x800, 3);
            r.write32(0x804, 0x301);
        });
        assert_eq!((c.r[0], c.pc, c.thumb, c.r[13], c.quirks[Q_PC_LOAD]), (3, 0x300, true, 0x808, 0));
        // B, conditional branch taken and not taken
        let (c, _) = thumb(&[0xE7FE], 1, |_, _| ());
        assert_eq!(c.pc, CODE);
        let (c, _) = thumb(&[0xD002], 1, |c, _| c.z = true);
        assert_eq!(c.pc, CODE + 8);
        let (c, _) = thumb(&[0xD002], 1, |_, _| ());
        assert_eq!(c.pc, CODE + 2);
    }

    #[test]
    fn thumb_ops() {
        // PUSH {r0, lr}; LSL r1, r0, #4; NEG r2, r0; CMP r0, #3; ADC r1, r0
        let (c, mut r) = thumb(&[0xB501, 0x0101, 0x4242, 0x2803, 0x4141], 5, |c, _| (c.r[0], c.r[13], c.r[14]) = (3, 0x900, 0x55));
        assert_eq!((r.read32(0x8F8), r.read32(0x8FC), c.r[13]), (3, 0x55, 0x8F8));
        assert_eq!((c.r[1], c.r[2]), (0x30 + 3 + 1, (-3i32) as u32));
        // LDR r0, [pc, #4]; ADD sp, #-8; STR r0, [sp, #4]; LDRB r1, [r2, #1]
        let (c, mut r) = thumb(&[0x4801, 0xB082, 0x9001, 0x7851, 0xBEEF, 0xDEAD], 4, |c, _| (c.r[13], c.r[2]) = (0x900, CODE + 8));
        assert_eq!((c.r[0], c.r[13], r.read32(0x8FC), c.r[1]), (0xDEAD_BEEF, 0x8F8, 0xDEAD_BEEF, 0xBE));
        // STMIA r0!, {r1, r2}; LDMIA r3!, {r4, r5}
        let (c, _) = thumb(&[0xC006, 0xCB30], 2, |c, _| (c.r[0], c.r[1], c.r[2], c.r[3]) = (0x800, 8, 9, 0x800));
        assert_eq!((c.r[0], c.r[3], c.r[4], c.r[5]), (0x808, 0x808, 8, 9));
        // ADD r8, r0 ; CMP r8, r1 ; MOV r2, r8
        let (c, _) = thumb(&[0x4480, 0x4588, 0x4642], 3, |c, _| (c.r[0], c.r[8], c.r[1]) = (2, 3, 5));
        assert_eq!((c.r[2], c.z), (5, true));
        // LSR r0, r1 with r1 = 32; MUL r2, r3
        let (c, _) = thumb(&[0x40C8, 0x435A], 2, |c, _| (c.r[0], c.r[1], c.r[2], c.r[3]) = (0x8000_0000, 32, 6, 7));
        assert_eq!((c.r[0], c.c, c.r[2]), (0, true, 42));
    }

    #[test]
    fn exceptions_and_banks() {
        // IRQ from Thumb system mode, then SUBS pc, lr, #4
        let mut ram = Ram(vec![0; 0x1000], false);
        ram.write32(0x18, 0xE25E_F004);
        let mut cpu = Arm7 { pc: 0x200, ..Default::default() };
        cpu.set_cpsr(0x6000_003F); // system mode, Thumb, Z and C
        (cpu.r[13], cpu.r[14], cpu.r[8]) = (1, 2, 3);
        assert!(cpu.interrupt(false));
        assert_eq!((cpu.cpsr(), cpu.r[14], cpu.pc, cpu.spsr()), (0x6000_0092, 0x204, 0x18, 0x6000_003F));
        assert!(!cpu.interrupt(false));
        cpu.r[13] = 0x777;
        assert!(cpu.interrupt(true)); // FIQ nests, with its own r8..r14
        assert_eq!((cpu.cpsr() & 0xFF, cpu.r[8]), (0xD1, 0));
        cpu.r[8] = 99;
        let spsr = cpu.spsr();
        cpu.set_cpsr(spsr);
        assert_eq!((cpu.r[8], cpu.r[13], cpu.r[14]), (3, 0x777, 0x204));
        cpu.pc = 0x18;
        cpu.run(&mut ram, 1);
        assert_eq!((cpu.cpsr(), cpu.pc, cpu.r[13], cpu.r[14]), (0x6000_003F, 0x200, 1, 2));
        // MSR CPSR_c, r0 switches mode; MRS r1, CPSR; MSR SPSR_fc, r2; MRS r3, SPSR
        let (c, _) = arm(&[0xE121_F000, 0xE10F_1000, 0xE169_F002, 0xE14F_3000], |c, _| (c.r[0], c.r[2], c.r[13]) = (0xD2, 0xF000_001F, 5));
        assert_eq!((c.r[1], c.r[3], c.r[13]), (0xD2, 0xF000_001F, 0));
        // STMDB sp, {sp}^ stores the user stack pointer
        let (_, mut r) = arm(&[0xE121_F000, 0xE3A0_DB02, 0xE94D_2000], |c, _| {
            c.set_cpsr(0xDF);
            c.r[13] = 0x1234;
            c.r[0] = 0xD2;
        });
        assert_eq!(r.read32(0x7FC), 0x1234);
        // undefined instructions stop the CPU
        let mut cpu = Arm7 { pc: CODE, ..Default::default() };
        ram.write32(CODE, 0xEE00_0000);
        cpu.run(&mut ram, 5);
        assert!(cpu.fault.is_some() && cpu.pc == CODE);
    }

    /// The bare interpreter's speed on a small Thumb loop:
    /// `cargo test --release --lib mips -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn mips() {
        // ADD r0, #1; LDR r1, [r2]; ADD r1, r1, r0; STR r1, [r2]; LSL r3, r1, #3; CMP r3, r0; BNE next; B start
        let code = [0x3001, 0x6811, 0x1809, 0x6011, 0x00CB, 0x4283, 0xD1FF, 0xE7F7];
        let count = 400_000_000;
        let start = std::time::Instant::now();
        let (c, _) = thumb(&code, count, |c, _| c.r[2] = 0x800);
        println!("{:.0} million instructions a second", count as f64 / start.elapsed().as_secs_f64() / 1e6);
        assert_eq!(c.r[0], count as u32 / 8);
    }
}
