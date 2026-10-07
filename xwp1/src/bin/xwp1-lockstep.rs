//! Runs the firmware on the interpreter and on Unicorn (exact stepping)
//! side by side through one scenario and compares registers and audio after
//! every sample, and RAM now and then. On a difference it replays that
//! sample one instruction at a time and prints the first instruction after
//! which the two disagree.
//!
//! Usage: xwp1-lockstep [--image FILE] [--syx FILE] [--seconds S] (from the project root)
use std::path::PathBuf;

use xwp1::machine::{CpuKind, Machine};
use xwp1::sound::SAMPLE_RATE;

const NAMES: [&str; 17] = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp", "lr",
                           "pc", "cpsr"];
const RAMS: [(u32, usize); 3] = [(0, 0x1_0000), (0x1FFE_8000, 0x8000), (0x1C00_0000, 0x8_0000)];

enum Ev {
    Run(f64),
    Midi(Vec<u8>),
    Key(u32, bool, u32),
    Button(u8, bool),
}

fn scenario(syx: &[Vec<u8>], scale: f64) -> Vec<Ev> {
    let mut ev = vec![Ev::Run(7.0)];
    let mut send = |ev: &mut Vec<Ev>, data: &[u8], wait: f64| {
        ev.push(Ev::Midi(data.to_vec()));
        ev.push(Ev::Run(data.len() as f64 * 2.0 / 3125.0 + 0.01 + wait * scale));
    };
    let notes = |ev: &mut Vec<Ev>, send: &mut dyn FnMut(&mut Vec<Ev>, &[u8], f64)| {
        send(ev, &[0x90, 60, 100], 0.4);
        send(ev, &[0xE0, 0, 0x50, 0xB0, 1, 90], 0.3);
        send(ev, &[0x90, 67, 127, 0x90, 48, 30], 0.4);
        send(ev, &[0x80, 60, 0, 0x80, 67, 0, 0x80, 48, 0, 0xE0, 0, 0x40, 0xB0, 1, 0], 0.5);
    };
    // Solo Synth preset, the patch, another preset; Hex Layer; Drawbar Organ; a PCM tone
    send(&mut ev, &[0xB0, 0, 98, 0xB0, 0x20, 0, 0xC0, 0], 1.0);
    for msg in syx {
        let dsp = msg.len() > 6 && msg[6] == 0x13;
        send(&mut ev, msg, if dsp { 0.3 } else { 0.005 });
    }
    notes(&mut ev, &mut send);
    for (bank, program) in [(98, 41), (97, 20), (96, 3), (0, 5)] {
        send(&mut ev, &[0xB0, 0, bank, 0xB0, 0x20, 0, 0xC0, program], 1.0);
        notes(&mut ev, &mut send);
    }
    // the key matrix and a panel button (0x22 starts the step sequencer)
    ev.extend([Ev::Key(36, false, 200), Ev::Run(0.5 * scale), Ev::Key(36, true, 0), Ev::Run(0.3 * scale)]);
    ev.extend([Ev::Button(0x22, true), Ev::Run(0.1), Ev::Button(0x22, false), Ev::Run(2.0 * scale)]);
    ev
}

fn apply(m: &mut Machine, ev: &Ev) -> usize {
    match ev {
        Ev::Run(seconds) => return (seconds * SAMPLE_RATE).ceil() as usize,
        Ev::Midi(data) => m.midi_in(data),
        Ev::Key(code, flag, velocity) => m.key(*code, *flag, *velocity),
        Ev::Button(code, down) => m.button(*code, *down),
    }
    0
}

fn pair(image: &[u8]) -> [Machine; 2] {
    [CpuKind::Native, CpuKind::Unicorn].map(|kind| Machine::with_cpu(image.to_vec(), kind).expect("machine"))
}

/// Replay up to sample `at`, then that sample by single instructions.
fn locate(image: &[u8], events: &[Ev], at: usize) {
    let mut ms = pair(image);
    let mut done = 0;
    'events: for ev in events {
        let samples = ms.each_mut().map(|m| apply(m, ev))[0];
        let before = samples.min(at - done);
        for m in &mut ms {
            assert!(m.run(before), "{:?}", m.fault);
        }
        done += before;
        if done == at && samples > before {
            break 'events;
        }
    }
    let start = ms[0].regs();
    for m in &mut ms {
        m.trail = Some(Vec::new());
        m.run(1);
    }
    let [a, b] = ms.each_mut().map(|m| m.trail.take().unwrap());
    for i in 0..a.len().max(b.len()) {
        if a.get(i) != b.get(i) {
            let prev = if i == 0 { start } else { a[i - 1] };
            let (pc, thumb) = (prev[15], prev[16] & 0x20 != 0);
            let code = ms[0].mem_read(pc & !1, 4);
            println!("instruction {i} of the sample, at {pc:#010x} ({}): bytes {code:02x?}",
                     if thumb { "Thumb" } else { "ARM" });
            println!("  before: {prev:08x?}");
            for (name, t) in [("native ", &a), ("unicorn", &b)] {
                println!("  {name}: {:08x?}", t.get(i));
            }
            return;
        }
    }
    println!("registers agree through sample {at} when single-stepped: the difference is outside the CPU \
              (devices or timing); faults: {:?} / {:?}", ms[0].fault, ms[1].fault);
}

fn main() {
    let mut image = PathBuf::from("firmware/win/XW-P1 Updater/p1-update.bin");
    let mut syx = Some(PathBuf::from("patches/hw_blank.syx"));
    let mut scale = 1.0;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().expect("value");
        match arg.as_str() {
            "--image" => image = value().into(),
            "--syx" => syx = Some(value().into()),
            "--scale" => scale = value().parse().expect("--scale"),
            other => panic!("unknown argument {other}"),
        }
    }
    let image = xwp1::image::load(&image).expect("firmware image");
    let syx: Vec<Vec<u8>> = syx.map(|p| std::fs::read(p).expect("syx")).unwrap_or_default()
        .split_inclusive(|&b| b == 0xF7).map(|m| m.to_vec()).collect();
    let events = scenario(&syx, scale);
    let mut ms = pair(&image);
    let (mut done, mut peak, mut checks) = (0usize, 0f32, 0u32);
    let ram = |ms: &[Machine; 2]| RAMS.iter().find(|&&(base, size)| ms[0].mem_read(base, size) != ms[1].mem_read(base, size)).copied();
    for ev in &events {
        let samples = ms.each_mut().map(|m| apply(m, ev))[0];
        for _ in 0..samples {
            let ok = ms.each_mut().map(|m| m.run(1));
            let regs = ms.each_ref().map(|m| m.regs());
            if ok != [true; 2] || regs[0] != regs[1] || ms[0].out != ms[1].out {
                println!("sample {done} ({:.3} s): native {} / unicorn {}", done as f64 / SAMPLE_RATE,
                         ms[0].fault.clone().unwrap_or("ok".into()), ms[1].fault.clone().unwrap_or("ok".into()));
                for (i, name) in NAMES.iter().enumerate().filter(|&(i, _)| regs[0][i] != regs[1][i]) {
                    println!("  {name}: native {:#010x} unicorn {:#010x}", regs[0][i], regs[1][i]);
                }
                println!("  audio: native {:?} unicorn {:?}", ms[0].out, ms[1].out);
                println!("  quirks: {:?}", ms[0].quirks());
                locate(&image, &events, done);
                std::process::exit(1);
            }
            peak = ms[0].out.iter().fold(peak, |p, x| p.max(x.abs()));
            for m in &mut ms {
                m.out.clear();
                m.rev_out.clear();
            }
            done += 1;
            if done % 16384 == 0 {
                checks += 1;
                if let Some((base, _)) = ram(&ms) {
                    println!("RAM at {base:#010x} differs by sample {done} with equal registers");
                    std::process::exit(1);
                }
            }
        }
    }
    assert!(ram(&ms).is_none(), "RAM differs at the end");
    assert!(peak > 0.0, "silent: nothing was compared");
    // Flash as each CPU sees it: what the firmware programmed or erased must agree too.
    let flash = ms.each_ref().map(|m| m.mem_read(xwp1::image::FLASH_BASE, xwp1::image::FLASH_SIZE));
    assert!(flash[0].len() == xwp1::image::FLASH_SIZE && flash[0] == flash[1], "flash differs at the end");
    let wrote = ms.each_mut().map(|m| (m.devices().flash.programmed, m.devices().flash.erased.len()));
    assert!(wrote[0] == wrote[1], "flash writes differ: {wrote:?}");
    let erased = &ms[0].devices().flash.erased;
    println!("flash: {} words programmed, {} sectors erased in {:#010x}..={:#010x} (equal on both, contents equal)", wrote[0].0,
             wrote[0].1, erased.iter().min().unwrap_or(&0), erased.iter().max().unwrap_or(&0));
    let sent = ms.each_mut().map(|m| m.devices().panel.sent.clone());
    assert!(sent[0] == sent[1], "panel traffic differs");
    println!("identical: {done} samples ({:.1} s), {} instructions, registers and audio after every sample, RAM {} times; \
              peak {peak:.4}; {} bytes to the panel; IRQs {} FIQs {}", done as f64 / SAMPLE_RATE,
             done * ms[0].insns_per_sample, checks + 1, sent[0].len(), ms[0].irqs, ms[0].fiqs);
    println!("quirks: {:?}", ms[0].quirks());
}
