//! The XW-P1: an ARM7TDMI with flash, RAM and memory-mapped peripherals,
//! stepped one audio sample at a time. The CPU is the interpreter in
//! `arm.rs`; Unicorn remains as the reference it is checked against
//! (`CpuKind::Unicorn`, `xwp1-lockstep`).
use std::collections::HashMap;

use unicorn_engine::unicorn_const::{Arch, HookType, Mode, Prot};
use unicorn_engine::{RegisterARM, Unicorn};

use crate::arm::{Arm7, Bus, QUIRKS};
use crate::flash::{Flash, SECTOR};
use crate::image::{FLASH_BASE, FLASH_SIZE};
use crate::soc::{Adc, Keys, Ports, Timers, Uart, Vic, TIMER_BASE, VIC_BASE};
use crate::reverb::{Reverb, ReverbBank};
use crate::sound::{Sound, HIRAM_BASE, PORT, SAMPLE_RATE, SLOTS, SRAM_BASE};

pub const CLOCK: f64 = 48_000_000.0;
const CYCLES_PER_INSN: f64 = 2.0; // rough ARM7TDMI average; sets timer rate relative to code
const TIMER_CLOCKS_PER_SAMPLE: f64 = 128.0;
/// The instrument's user memory in flash (user tones, Performances, sequences, chains, arpeggios, phrases,
/// settings): the firmware formats it when it finds it empty, and Write stores there.
pub const USER_MEMORY: (u32, usize) = (0x18F0_0000, 0x10_0000);
const VOICE_IRQ: u32 = 29; // the sound source's own interrupt (ramp finished)
const SRAM_SIZE: usize = 0x8_0000;
const MODE_CACHE: usize = 1 << 16;
const RAM_SIZE: usize = 0x1_0000; // at address 0
const HIRAM_SIZE: usize = 0x8000;
// The second flash chip (0x19000000) is also visible at 0x1e000000.
const FLASH2_ALIAS: (u32, u32, usize) = (0x1E00_0000, 0x1900_0000, 0x0100_0000);
const MMIO: [(u32, usize); 6] = [(0x1FFD_0000, 0x1_0000), (0x1FFE_0000, 0x8000), (0x1FFF_0000, 0x1_0000),
                                 (0x2A00_0000, 0x1_0000), (0x5000_0000, 0x1000), (0xFFFF_F000, 0x1000)];

/// Everything the firmware can reach through MMIO or that is stepped per sample.
pub struct Devices {
    pub vic: Vic,
    pub timers: Timers,
    pub keys: Keys,
    pub adc: Adc,
    pub ports: Ports,
    pub panel: Uart,
    pub uart1: Uart,
    pub flash: Flash,
    pub sound: Sound,
    image: Vec<u8>,   // pristine flash, what the voices play from
    sram: Box<[u8]>,  // mapped at SRAM_BASE; the streaming voices read it
    hiram: Box<[u8]>, // mapped at HIRAM_BASE; the external input voice reads its ring here
    pub unknown: HashMap<(bool, u32), u64>, // (is write, address) -> count
    budget: i64,      // instructions left in this slice (block stepping)
    modes: Vec<u32>,  // block address -> instruction width, see set_block_stepping
    // The interpreter's memory (Unicorn keeps its own copies of these two).
    ram: Box<[u8]>,
    rom: Vec<u8>,     // flash as the CPU sees it: `image` plus what the firmware programmed
    bus_fault: Option<(u32, bool)>, // unmapped access: address, is write
}

impl Devices {
    /// `Sound::wave_shots` on this machine's flash.
    pub fn wave_shots(&self, limit: usize) -> Vec<(usize, u32, f64, usize, Vec<i16>)> {
        self.sound.wave_shots(&self.image, &self.sram, limit)
    }

    fn read(&mut self, addr: u32) -> u32 {
        match addr {
            VIC_BASE..=0xFFFF_FFFF => self.vic.read(addr),
            a if (TIMER_BASE..TIMER_BASE + 0x30).contains(&a) => self.timers.read(addr),
            a if (Keys::BASE..Keys::BASE + 0x10).contains(&a) => self.keys.read(&mut self.vic, addr),
            a if (Adc::BASE..Adc::BASE + 0x10).contains(&a) => self.adc.read(addr, self.ports.mux()),
            a if (Ports::BASE..Ports::BASE + 0x10).contains(&a) => self.ports.read(addr),
            a if (self.panel.base..self.panel.base + 0x40).contains(&a) => self.panel.read(addr),
            a if (self.uart1.base..self.uart1.base + 0x40).contains(&a) => self.uart1.read(addr),
            a if (PORT..PORT + 0x80).contains(&a) || (SLOTS..SLOTS + 0x8000).contains(&a) => self.sound.read(addr),
            _ => {
                *self.unknown.entry((false, addr)).or_insert(0) += 1;
                0
            }
        }
    }

    fn write(&mut self, addr: u32, value: u32) {
        match addr {
            VIC_BASE..=0xFFFF_FFFF => self.vic.write(addr, value),
            a if (TIMER_BASE..TIMER_BASE + 0x30).contains(&a) => self.timers.write(&mut self.vic, addr, value),
            a if (Keys::BASE..Keys::BASE + 0x10).contains(&a) => self.keys.write(&mut self.vic, addr, value),
            a if (Adc::BASE..Adc::BASE + 0x10).contains(&a) => self.adc.write(addr, value),
            a if (Ports::BASE..Ports::BASE + 0x10).contains(&a) => self.ports.write(addr, value),
            a if (self.panel.base..self.panel.base + 0x40).contains(&a) => self.panel.write(addr, value),
            a if (self.uart1.base..self.uart1.base + 0x40).contains(&a) => self.uart1.write(addr, value),
            a if (PORT..PORT + 0x80).contains(&a) || (SLOTS..SLOTS + 0x8000).contains(&a) => {
                self.sound.write(addr, value)
            }
            _ => *self.unknown.entry((true, addr)).or_insert(0) += 1,
        }
    }
}

impl Devices {
    /// Directly readable memory at `addr`: the region and the offset in it.
    #[inline(always)]
    fn mem(&self, addr: u32) -> Option<(&[u8], usize)> {
        if addr < RAM_SIZE as u32 {
            Some((&self.ram, addr as usize))
        } else if addr.wrapping_sub(FLASH_BASE) < FLASH_SIZE as u32 {
            Some((&self.rom, (addr - FLASH_BASE) as usize))
        } else if addr.wrapping_sub(HIRAM_BASE) < HIRAM_SIZE as u32 {
            Some((&self.hiram, (addr - HIRAM_BASE) as usize))
        } else if addr.wrapping_sub(SRAM_BASE) < SRAM_SIZE as u32 {
            Some((&self.sram, (addr - SRAM_BASE) as usize))
        } else if addr.wrapping_sub(FLASH2_ALIAS.0) < FLASH2_ALIAS.2 as u32 {
            Some((&self.rom, (addr - FLASH2_ALIAS.0 + (FLASH2_ALIAS.1 - FLASH_BASE)) as usize))
        } else {
            None
        }
    }

    /// Plain RAM at `addr`.
    #[inline(always)]
    fn mem_mut(&mut self, addr: u32) -> Option<(&mut [u8], usize)> {
        if addr < RAM_SIZE as u32 {
            Some((&mut self.ram, addr as usize))
        } else if addr.wrapping_sub(HIRAM_BASE) < HIRAM_SIZE as u32 {
            Some((&mut self.hiram, (addr - HIRAM_BASE) as usize))
        } else if addr.wrapping_sub(SRAM_BASE) < SRAM_SIZE as u32 {
            Some((&mut self.sram, (addr - SRAM_BASE) as usize))
        } else {
            None
        }
    }

    #[inline(never)]
    fn io_read(&mut self, addr: u32) -> u32 {
        if MMIO.iter().any(|&(base, size)| addr.wrapping_sub(base) < size as u32) {
            self.read(addr)
        } else {
            self.bus_fault.get_or_insert((addr, false));
            0
        }
    }

    /// A write that is not to RAM: the flash's command interface or a peripheral.
    #[inline(never)]
    fn io_write(&mut self, addr: u32, value: u32, size: usize) {
        if addr.wrapping_sub(FLASH_BASE) < FLASH_SIZE as u32 {
            // As the Unicorn hook does: the model sees the write, then it lands.
            self.settle();
            let off = (addr - FLASH_BASE) as usize;
            self.flash.write(addr, value, self.rom[off..off + size].to_vec());
            self.rom[off..off + size].copy_from_slice(&value.to_le_bytes()[..size]);
            self.settle(); // before the firmware's status poll reads the cell
        } else if MMIO.iter().any(|&(base, size)| addr.wrapping_sub(base) < size as u32) {
            self.write(addr, value);
        } else {
            self.bus_fault.get_or_insert((addr, true));
        }
    }

    /// Apply the flash model's deferred effects to the interpreter's flash.
    fn settle(&mut self) {
        for (addr, old) in std::mem::take(&mut self.flash.undo) {
            let off = (addr - FLASH_BASE) as usize;
            self.rom[off..off + old.len()].copy_from_slice(&old);
        }
        for sector in std::mem::take(&mut self.flash.fill) {
            let off = (sector - FLASH_BASE) as usize;
            self.rom[off..off + SECTOR as usize].fill(0xFF);
            self.flash.erased.push(sector);
        }
    }
}

impl Bus for Devices {
    #[inline(always)]
    fn read8(&mut self, addr: u32) -> u32 {
        match self.mem(addr) {
            Some((mem, off)) => mem[off] as u32,
            None => self.io_read(addr) & 0xFF,
        }
    }

    #[inline(always)]
    fn read16(&mut self, addr: u32) -> u32 {
        match self.mem(addr) {
            Some((mem, off)) => u16::from_le_bytes(mem[off..off + 2].try_into().unwrap()) as u32,
            None => self.io_read(addr) & 0xFFFF,
        }
    }

    #[inline(always)]
    fn read32(&mut self, addr: u32) -> u32 {
        match self.mem(addr) {
            Some((mem, off)) => u32::from_le_bytes(mem[off..off + 4].try_into().unwrap()),
            None => self.io_read(addr),
        }
    }

    #[inline(always)]
    fn write8(&mut self, addr: u32, value: u32) {
        match self.mem_mut(addr) {
            Some((mem, off)) => mem[off] = value as u8,
            None => self.io_write(addr, value, 1),
        }
    }

    #[inline(always)]
    fn write16(&mut self, addr: u32, value: u32) {
        match self.mem_mut(addr) {
            Some((mem, off)) => mem[off..off + 2].copy_from_slice(&(value as u16).to_le_bytes()),
            None => self.io_write(addr, value, 2),
        }
    }

    #[inline(always)]
    fn write32(&mut self, addr: u32, value: u32) {
        match self.mem_mut(addr) {
            Some((mem, off)) => mem[off..off + 4].copy_from_slice(&value.to_le_bytes()),
            None => self.io_write(addr, value, 4),
        }
    }

    #[inline(always)]
    fn faulted(&self) -> bool {
        self.bus_fault.is_some()
    }
}

type Uc = Unicorn<'static, Devices>;

/// Which CPU core runs the firmware.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CpuKind {
    /// The ARM7TDMI interpreter in `arm.rs`, exact instruction budget.
    Native,
    /// Unicorn, the reference; see `Machine::set_block_stepping`.
    Unicorn,
}

enum Cpu {
    Native(Box<(Arm7, Devices)>),
    Unicorn(Uc),
}

impl Cpu {
    fn devices(&mut self) -> &mut Devices {
        match self {
            Cpu::Native(n) => &mut n.1,
            Cpu::Unicorn(uc) => uc.get_data_mut(),
        }
    }

    fn settle(&mut self) {
        match self {
            Cpu::Native(n) => n.1.settle(),
            Cpu::Unicorn(uc) => settle(uc),
        }
    }

    fn enter(&mut self, fiq: bool) -> bool {
        match self {
            Cpu::Native(n) => n.0.interrupt(fiq),
            Cpu::Unicorn(uc) => enter(uc, fiq),
        }
    }
}

/// The line output's frequency response, measured with white noise against
/// the model's flat output (rec/oscfilter_noise, filter off): a first-order
/// high-pass at 46.9 Hz (AC coupling) and a droop to -2.6 dB at 11 kHz and
/// -8.6 dB at 19 kHz, here a symmetric 5-tap FIR. Within 0.1 dB rms from
/// 30 Hz to 19 kHz.
#[derive(Default, Clone, Copy)]
pub struct OutputStage {
    x: [f32; 4],
    hp_x: f32,
    hp_y: f32,
}

impl OutputStage {
    const HP_R: f32 = 0.993_139; // exp(-2 pi 46.9 / 42818.1)
    const C1: f32 = 0.161_94;
    const C2: f32 = -0.018_85;

    pub fn run(&mut self, x: f32) -> f32 {
        let [x1, x2, x3, x4] = self.x;
        let fir = (1.0 - 2.0 * Self::C1 - 2.0 * Self::C2) * x2 + Self::C1 * (x1 + x3) + Self::C2 * (x + x4);
        self.x = [x, x1, x2, x3];
        let y = (1.0 + Self::HP_R) / 2.0 * (fir - self.hp_x) + Self::HP_R * self.hp_y;
        self.hp_x = fir;
        self.hp_y = y;
        y
    }
}

pub struct Machine {
    cpu: Cpu,
    start: Option<u32>,
    pub insns_per_sample: usize,
    pub irqs: u64,
    pub fiqs: u64,
    fiq_waiting: bool,
    pub fiq_lost: u64, // sample FIQs that never ran (masked for a whole sample)
    pub fault: Option<String>,
    pub out: Vec<f32>,     // left, right per audio sample, until the caller drains it
    pub rev_out: Vec<f32>, // the reverb send, one per audio sample (drained with `out`)
    pub reverb: Option<Reverb>, // None = dry output
    pub reverb_bank: Option<ReverbBank>, // measured responses by type and time; None = `reverb` stays as it is
    pub reverb_setting: Option<(u8, u8)>, // the bank entry in `reverb`; None = load on the next sample
    pub input: std::collections::VecDeque<f32>, // instrument input, one value per sample, full scale 1 (empty = silence)
    pub output_stage: Option<[OutputStage; 2]>, // the instrument's line output response; None = flat
    block_hook: bool,
    pub trail: Option<Vec<[u32; 17]>>, // registers after every instruction, for `xwp1-lockstep`
}

/// Apply the flash model's deferred effects (its hook runs before the write lands).
fn settle(uc: &mut Unicorn<'_, Devices>) {
    let undo = std::mem::take(&mut uc.get_data_mut().flash.undo);
    for (addr, old) in undo {
        let _ = uc.mem_write(addr as u64, &old);
    }
    let fill = std::mem::take(&mut uc.get_data_mut().flash.fill);
    for sector in fill {
        let _ = uc.mem_write(sector as u64, &vec![0xFF; SECTOR as usize]);
        uc.get_data_mut().flash.erased.push(sector);
    }
}

fn reg(uc: &Unicorn<'_, Devices>, r: RegisterARM) -> u32 {
    uc.reg_read(r).unwrap_or(0) as u32
}

/// Take the FIQ (vector 0x1c) or IRQ (0x18) exception unless it is masked.
fn enter(uc: &mut Unicorn<'_, Devices>, fiq: bool) -> bool {
    let cpsr = reg(uc, RegisterARM::CPSR);
    let (mask, mode, vector) = if fiq { (0x40, 0xD1, 0x1C) } else { (0x80, 0x92, 0x18) };
    if cpsr & mask != 0 {
        return false;
    }
    let pc = reg(uc, RegisterARM::PC);
    let _ = uc.reg_write(RegisterARM::CPSR, ((cpsr & !0x3F) | mode) as u64);
    let _ = uc.reg_write(RegisterARM::SPSR, cpsr as u64);
    let _ = uc.reg_write(RegisterARM::LR, pc.wrapping_add(4) as u64);
    let _ = uc.reg_write(RegisterARM::PC, vector);
    true
}

const IRQ_RETRY: usize = 8; // instructions between offers of a masked IRQ
// No IRQ is entered with fewer instructions than this left in the sample:
// the firmware's entry reads the VIC's vector 20 instructions in, and the
// sound source's line (refreshed at the sample boundary) may fall in
// between. A vector read with nothing pending answers 0, the reset vector.
const IRQ_LATEST: usize = 32;

impl Machine {
    /// `image` is flash in address order (`image::load`).
    pub fn new(image: Vec<u8>) -> Result<Machine, String> {
        Machine::with_cpu(image, CpuKind::Native)
    }

    pub fn with_cpu(image: Vec<u8>, kind: CpuKind) -> Result<Machine, String> {
        let e = |err| format!("unicorn: {err:?}");
        assert_eq!(image.len(), FLASH_SIZE);
        let mut sram = vec![0u8; SRAM_SIZE].into_boxed_slice();
        let sram_ptr = sram.as_mut_ptr();
        let mut hiram = vec![0u8; HIRAM_SIZE].into_boxed_slice();
        let hiram_ptr = hiram.as_mut_ptr();
        let alias = image[(FLASH2_ALIAS.1 - FLASH_BASE) as usize..][..FLASH2_ALIAS.2].to_vec();
        let devices = Devices {
            vic: Vic::new(), timers: Timers::new(), keys: Keys::new(), adc: Adc::new(), ports: Ports::new(),
            panel: Uart::new(0x2A00_3A00, 16, 17, CLOCK as i64 / 3125),
            uart1: Uart::new(0x2A00_3A40, 19, 20, CLOCK as i64 / 781),
            flash: Flash::default(), sound: Sound::new(), image, sram, hiram, unknown: HashMap::new(), budget: 0, modes: vec![0; MODE_CACHE],
            ram: Default::default(), rom: Vec::new(), bus_fault: None,
        };
        let insns_per_sample = (CLOCK / SAMPLE_RATE / CYCLES_PER_INSN).round() as usize;
        let machine = |cpu| {
            let mut m = Machine { cpu, start: None, insns_per_sample, irqs: 0, fiqs: 0, fiq_waiting: false, fiq_lost: 0,
                                  fault: None, out: Vec::new(), rev_out: Vec::new(), output_stage: Some(Default::default()), input: Default::default(),
                                  reverb: Some(Reverb::new(SAMPLE_RATE)), reverb_bank: None, reverb_setting: None, block_hook: false, trail: None };
            m.reset();
            m
        };
        if kind == CpuKind::Native {
            let mut devices = devices;
            devices.ram = vec![0u8; RAM_SIZE].into_boxed_slice();
            devices.rom = devices.image.clone();
            return Ok(machine(Cpu::Native(Box::new((Arm7::default(), devices)))));
        }
        let mut uc = Unicorn::new_with_data(Arch::ARM, Mode::ARM, devices).map_err(e)?;
        uc.mem_map(FLASH_BASE as u64, FLASH_SIZE as u64, Prot::ALL).map_err(e)?;
        let image = uc.get_data().image.clone();
        uc.mem_write(FLASH_BASE as u64, &image).map_err(e)?;
        uc.mem_map(FLASH2_ALIAS.0 as u64, FLASH2_ALIAS.2 as u64, Prot::ALL).map_err(e)?;
        uc.mem_write(FLASH2_ALIAS.0 as u64, &alias).map_err(e)?;
        uc.mem_map(0, RAM_SIZE as u64, Prot::ALL).map_err(e)?;
        // SAFETY: the buffer is boxed inside the engine's own data, so it
        // lives as long as the mapping and never moves.
        unsafe { uc.mem_map_ptr(SRAM_BASE as u64, SRAM_SIZE as u64, Prot::ALL, sram_ptr as _) }.map_err(e)?;
        unsafe { uc.mem_map_ptr(HIRAM_BASE as u64, HIRAM_SIZE as u64, Prot::ALL, hiram_ptr as _) }.map_err(e)?;
        for (base, size) in MMIO {
            uc.mmio_map(base as u64, size as u64,
                        Some(move |uc: &mut Unicorn<'_, Devices>, off: u64, _size: usize| {
                            uc.get_data_mut().read(base + off as u32) as u64
                        }),
                        Some(move |uc: &mut Unicorn<'_, Devices>, off: u64, _size: usize, value: u64| {
                            uc.get_data_mut().write(base + off as u32, value as u32)
                        }))
              .map_err(e)?;
        }
        uc.add_mem_hook(HookType::MEM_WRITE, FLASH_BASE as u64, FLASH_BASE as u64 + FLASH_SIZE as u64 - 1,
                        |uc, _kind, addr, size, value| {
                            settle(uc);
                            let old = uc.mem_read_as_vec(addr, size).unwrap_or_default();
                            uc.get_data_mut().flash.write(addr as u32, value as u32, old);
                            true
                        })
          .map_err(e)?;
        // The hook above runs before the write lands, so what the model wants in the cell is put there when the
        // firmware next reads the user area (its status poll, at once), the only flash it programs.
        uc.add_mem_hook(HookType::MEM_READ, USER_MEMORY.0 as u64, USER_MEMORY.0 as u64 + USER_MEMORY.1 as u64 - 1,
                        |uc, _kind, _addr, _size, _value| {
                            let flash = &uc.get_data().flash;
                            if !flash.undo.is_empty() || !flash.fill.is_empty() {
                                settle(uc);
                            }
                            true
                        })
          .map_err(e)?;
        Ok(machine(Cpu::Unicorn(uc)))
    }

    pub fn reset(&mut self) {
        match &mut self.cpu {
            Cpu::Native(n) => {
                n.0.set_cpsr(0xD3);
                n.0.pc = FLASH_BASE;
            }
            Cpu::Unicorn(uc) => {
                let _ = uc.reg_write(RegisterARM::CPSR, 0xD3);
                self.start = Some(FLASH_BASE);
            }
        }
    }

    pub fn cpu_kind(&self) -> CpuKind {
        match self.cpu {
            Cpu::Native(_) => CpuKind::Native,
            Cpu::Unicorn(_) => CpuKind::Unicorn,
        }
    }

    /// Count instructions per translated block instead of per instruction.
    /// Slices then end on block boundaries, so runs are no longer
    /// instruction-for-instruction the same as emu/machine.py, but the CPU
    /// runs much faster. Unicorn only: the interpreter is always exact.
    pub fn set_block_stepping(&mut self) -> Result<(), String> {
        let Cpu::Unicorn(uc) = &mut self.cpu else { return Ok(()) };
        if !self.block_hook {
            uc.add_block_hook(1, 0, |uc, addr, size| {
                // A block's instruction count is its size over 2 (Thumb) or
                // 4 (ARM). Reading CPSR costs more than the rest of the hook,
                // so the answer is cached by block address.
                let slot = (addr as usize >> 1) & (MODE_CACHE - 1);
                let known = uc.get_data().modes[slot];
                let width = if known >> 3 == addr as u32 >> 3 && known & 1 != 0 {
                    known >> 1 & 3
                } else {
                    let w = if reg(uc, RegisterARM::CPSR) & 0x20 != 0 { 1 } else { 2 };
                    uc.get_data_mut().modes[slot] = (addr as u32 & !7) | w << 1 | 1;
                    w
                };
                let d = uc.get_data_mut();
                d.budget -= (size >> width) as i64;
                if d.budget <= 0 {
                    let _ = uc.emu_stop();
                }
            }).map_err(|err| format!("unicorn: {err:?}"))?;
            self.block_hook = true;
        }
        Ok(())
    }

    pub fn devices(&mut self) -> &mut Devices {
        self.cpu.devices()
    }

    pub fn pc(&self) -> u32 {
        match &self.cpu {
            Cpu::Native(n) => n.0.pc,
            Cpu::Unicorn(uc) => reg(uc, RegisterARM::PC),
        }
    }

    /// r0..r15 of the current mode and the CPSR.
    pub fn regs(&self) -> [u32; 17] {
        let mut out = [0; 17];
        match &self.cpu {
            Cpu::Native(n) => {
                out[..15].copy_from_slice(&n.0.r[..15]);
                (out[15], out[16]) = (n.0.pc, n.0.cpsr());
            }
            Cpu::Unicorn(uc) => {
                use RegisterARM::*;
                for (o, r) in out.iter_mut().zip([R0, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12, SP, LR, PC, CPSR]) {
                    *o = reg(uc, r);
                }
            }
        }
        out
    }

    /// Put bytes into the work RAM at 0x1c000000 (the instrument's edit buffers: the step sequence being edited).
    /// Nothing else can be written this way. -> whether it was all inside.
    pub fn poke(&mut self, addr: u32, data: &[u8]) -> bool {
        let Some(at) = addr.checked_sub(SRAM_BASE).map(|a| a as usize).filter(|a| a + data.len() <= SRAM_SIZE) else { return false };
        self.devices().sram[at..at + data.len()].copy_from_slice(data); // Unicorn maps this very buffer
        true
    }

    /// Set mapped Solo Synth edit-buffer bytes for per-voice macro modulation.
    /// The ordinary panel POKE command remains limited to SRAM.
    pub(crate) fn poke_tone(&mut self, addr: u32, data: &[u8]) -> bool {
        let Some(at) = addr.checked_sub(HIRAM_BASE).map(|a| a as usize).filter(|a| a + data.len() <= HIRAM_SIZE) else { return false };
        self.devices().hiram[at..at + data.len()].copy_from_slice(data);
        true
    }

    pub(crate) fn tone_pair(&mut self, addr: u32) -> Option<[u8; 2]> {
        let at = addr.checked_sub(HIRAM_BASE)? as usize;
        let bytes = self.devices().hiram.get(at..at + 2)?;
        Some([bytes[0], bytes[1]])
    }

    /// Empty when any of it is not memory (as under Unicorn).
    pub fn mem_read(&self, addr: u32, size: usize) -> Vec<u8> {
        match &self.cpu {
            Cpu::Native(n) => {
                (0..size as u32).map(|i| n.1.mem(addr.wrapping_add(i)).map(|(mem, off)| mem[off])).collect::<Option<_>>()
                                .unwrap_or_default()
            }
            Cpu::Unicorn(uc) => uc.mem_read_as_vec(addr as u64, size).unwrap_or_default(),
        }
    }

    /// Counts of the events where the interpreter (ARMv4T) and Unicorn's
    /// ARMv7 core behave differently; all zero under Unicorn.
    pub fn quirks(&self) -> Vec<(&'static str, u64)> {
        match &self.cpu {
            Cpu::Native(n) => QUIRKS.iter().copied().zip(n.0.quirks).collect(),
            Cpu::Unicorn(_) => Vec::new(),
        }
    }

    /// Send MIDI bytes as the panel sub-CPU forwards them: each byte as
    /// 0x80 | (byte >> 7) followed by byte & 0x7f (demux at 0x180261f4).
    pub fn midi_in(&mut self, data: &[u8]) {
        let panel = &mut self.devices().panel;
        for &b in data {
            panel.feed(&[0x80 | b >> 7, b & 0x7F]);
        }
    }

    /// A key matrix event, as the keyboard controller queues it: bit 7 of the
    /// high byte with the key code, and a velocity byte (see FINDINGS for
    /// which events the firmware turns into notes).
    pub fn key(&mut self, code: u32, flag: bool, velocity: u32) {
        let d = self.devices();
        d.keys.key(&mut d.vic, code & 0x7F, flag, velocity & 0xFF);
    }

    /// The data dial, turned by `clicks` (the panel link's 0xc0 | sign, then
    /// the low seven bits of the signed count; handler 0x18025ca2).
    pub fn dial(&mut self, clicks: i8) {
        self.devices().panel.feed(&[0xC0 | (clicks < 0) as u8, clicks as u8 & 0x7F]);
    }

    /// A slider or knob (`front::control_input`) put where the firmware reads `position`.
    pub fn control(&mut self, control: usize, position: u8) {
        let (channel, input) = crate::front::control_input(control);
        self.devices().adc.muxed[channel - 5][input] = crate::front::control_reading(control, position);
    }

    /// Panel button event: 0xb1 = press, 0xb0 = release, then the code.
    pub fn button(&mut self, code: u8, down: bool) {
        self.devices().panel.feed(&[if down { 0xB1 } else { 0xB0 }, code & 0x7F]);
    }

    /// MIDI bytes in what the firmware sent to the panel from `from` on
    /// (0x80 | b7 then the low 7 bits; display traffic uses other status
    /// bytes). Returns the bytes and the offset to continue from.
    pub fn midi_out(&mut self, from: usize) -> (Vec<u8>, usize) {
        let raw = &self.devices().panel.sent;
        let (mut out, mut i) = (Vec::new(), from);
        while i + 1 < raw.len() {
            if (raw[i] == 0x80 || raw[i] == 0x81) && raw[i + 1] < 0x80 {
                out.push((raw[i] & 1) << 7 | raw[i + 1]);
                i += 2;
            } else {
                i += 1;
            }
        }
        (out, i)
    }

    /// Run `count` instructions; with `trail` set, one at a time, recording
    /// the registers after each.
    fn step(&mut self, count: usize) -> bool {
        if self.trail.is_none() {
            return self.step_cpu(count);
        }
        for _ in 0..count {
            if !self.step_cpu(1) {
                return false;
            }
            let regs = self.regs();
            self.trail.as_mut().unwrap().push(regs);
        }
        true
    }

    fn step_cpu(&mut self, count: usize) -> bool {
        let uc = match &mut self.cpu {
            Cpu::Native(n) => {
                let (cpu, dev) = &mut **n;
                cpu.run(dev, count);
                let what = match (cpu.fault.take(), dev.bus_fault.take()) {
                    (Some(what), _) => what,
                    (None, Some((addr, write))) => format!("unmapped {} at {addr:#010x}", if write { "write" } else { "read" }),
                    (None, None) => return true,
                };
                let regs: Vec<String> = cpu.r[..14].iter().map(|r| format!("{r:x}")).collect();
                self.fault = Some(format!("{what} at pc {:#010x} lr {:#010x} (r0..r13 {})", cpu.pc, cpu.r[14], regs.join(" ")));
                return false;
            }
            Cpu::Unicorn(uc) => uc,
        };
        let mut pc = self.start.take().unwrap_or_else(|| reg(uc, RegisterARM::PC));
        if reg(uc, RegisterARM::CPSR) & 0x20 != 0 {
            pc |= 1;
        }
        let count = if self.block_hook {
            uc.get_data_mut().budget += count as i64;
            0
        } else {
            count
        };
        if let Err(err) = uc.emu_start(pc as u64, 0xFFFF_FFF0, 0, count) {
            self.fault = Some(format!("{err:?} at pc {:#010x} lr {:#010x}", reg(uc, RegisterARM::PC), reg(uc, RegisterARM::LR)));
            return false;
        }
        true
    }

    /// Run `samples` audio samples: after each sample's worth of
    /// instructions the sound source is stepped, timers advance and pending
    /// FIQ / IRQ are delivered. Returns false on a fault.
    pub fn run(&mut self, samples: usize) -> bool {
        let insns = self.insns_per_sample;
        let late = insns * 3 / 4;
        let clocks = (CLOCK / SAMPLE_RATE) as i64; // CPU clocks per sample, for the UARTs
        for _ in 0..samples {
            let input = self.input.pop_front().unwrap_or(0.0);
            self.cpu.devices().sound.set_input((input.clamp(-1.0, 1.0) * 16383.0) as i16);
            // The sample FIQ is pending at every sample boundary, so IRQs
            // are offered part-way through the sample, after its handler
            // has normally returned (the CPU masks IRQ while in FIQ mode).
            if !self.step(late) {
                return false;
            }
            let mut taken = false;
            if self.cpu.devices().vic.fiq() {
                // still pending: it was masked at the boundary
                if self.cpu.enter(true) {
                    self.fiqs += 1;
                }
            } else if self.cpu.devices().vic.irq() && self.cpu.enter(false) {
                self.irqs += 1;
                taken = true;
            }
            // An IRQ that was masked just then (the kernel's critical
            // sections: during a patch load it was found in one at this
            // point for 15 samples in a row) is offered again until the end
            // of the sample, as the CPU would take it when the mask clears:
            // one offer per sample left the panel UART's byte unread until
            // the next one replaced it. Offering from the start of the
            // sample instead loses sample FIQs (FINDINGS, bug-074).
            let mut left = insns - late;
            while !taken && left >= IRQ_LATEST + IRQ_RETRY && self.cpu.devices().vic.irq() {
                if !self.step(IRQ_RETRY) {
                    return false;
                }
                left -= IRQ_RETRY;
                if self.cpu.devices().vic.irq() && self.cpu.enter(false) {
                    self.irqs += 1;
                    taken = true;
                }
            }
            if !self.step(left) {
                return false;
            }
            self.cpu.settle();
            let d = self.cpu.devices();
            d.panel.tick(&mut d.vic, clocks);
            d.uart1.tick(&mut d.vic, clocks);
            let sample = d.sound.tick(&d.image, &mut d.sram, &d.hiram);
            d.vic.set_line(VOICE_IRQ, d.sound.irq());
            d.timers.advance(&mut d.vic, TIMER_CLOCKS_PER_SAMPLE);
            let fiq = d.vic.fiq();
            let fx = d.sound.fx;
            if self.reverb.is_some() {
                if let Some(bank) = &self.reverb_bank {
                    // The firmware changes type and time only with the level
                    // ramped to 0, so the swap is silent.
                    if self.reverb_setting != Some((fx.reverb_type, fx.reverb_time)) {
                        if let Some(r) = bank.reverb(fx.reverb_type, fx.reverb_time) {
                            self.reverb = Some(r);
                        }
                        self.reverb_setting = Some((fx.reverb_type, fx.reverb_time));
                    }
                    if let Some(r) = &mut self.reverb {
                        r.gain = fx.reverb_level as f32 / 32.0;
                    }
                }
            }
            let wet = self.reverb.as_mut().map_or([0.0; 2], |r| r.tick(sample[2] as f32));
            let mut mixed = [sample[0] as f32 + wet[0], sample[1] as f32 + wet[1]];
            if let Some(stage) = &mut self.output_stage {
                mixed = [stage[0].run(mixed[0]), stage[1].run(mixed[1])];
            }
            self.out.extend_from_slice(&mixed);
            self.rev_out.push(sample[2] as f32);
            if fiq {
                if self.fiq_waiting {
                    self.fiq_lost += 1; // previous sample's FIQ was never taken
                }
                self.fiq_waiting = !self.cpu.enter(true);
                if !self.fiq_waiting {
                    self.fiqs += 1;
                }
            }
        }
        true
    }
}
