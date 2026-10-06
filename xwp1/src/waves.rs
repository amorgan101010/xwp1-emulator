//! Optional wave preview builder. Each completed wave is a checkpoint, so Ctrl-C and retry resume.
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::{image, machine::Machine, setup, sound::{SAMPLE_RATE, WaveSpec}};

const N: usize = 256;
const LIMIT: usize = 4_000_000;
const WORK_VERSION: u32 = 2;

#[derive(Clone, Serialize, Deserialize)]
struct Picture { data: Vec<u8>, row: Vec<Value> }
#[derive(Serialize, Deserialize)]
struct WaveWork { splits: Vec<[usize; 2]>, pictures: Vec<Picture>, specs: Vec<WaveSpec> }
#[derive(Clone)]
struct Job { block: &'static str, inst: u8, wave: usize, base: u16 }

fn invalid(s: impl Into<String>) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, s.into()) }
fn set_param(m: &mut Machine, pid: u16, inst: u8, value: u16, bytes: usize) -> io::Result<()> {
    let mut address = [0u8; 18];
    address[0] = 9; address[10] = inst; address[12] = pid as u8 & 127; address[13] = (pid >> 7) as u8;
    let mut msg = vec![0xF0, 0x44, 0x16, 0x03, 0x7F, 1];
    msg.extend(address);
    for i in 0..bytes { msg.push(((value as u32 >> (7 * i)) & 127) as u8); }
    msg.push(0xF7);
    setup::send(m, &msg, 0.02)
}
fn prepare(m: &mut Machine) -> io::Result<()> {
    for inst in 0..6 { set_param(m, 0, inst, 0, 1)?; }
    // A neutral sounding oscillator, independent of the currently selected preset.
    for inst in [0, 2, 5] {
        for (pid, value) in [(42, 100), (44, 96), (48, 0), (49, 0), (50, 127),
                              (51, 0), (52, 127), (25, 15), (35, 127),
                              (53, 0), (54, 0), (55, 0), (56, 0)] { // release at once: a probe must find one voice
            set_param(m, pid, inst, value, 1)?;
        }
        // Pitch as the key alone gives it (the split a key plays and the picture's cycle depend on it): no glide,
        // offset, detune, LFO or envelope, key follow 100 from note 60. Wire values, with their byte counts.
        for (pid, value, bytes) in [(4, 0, 1), (5, 10, 1), (6, 0, 1), (8, 64, 1), (9, 0, 3), (10, 512, 2), (11, 64, 1),
                                    (12, 0, 1), (13, 64, 1), (14, 0, 1), (15, 64, 1), (16, 0, 1), (17, 64, 1), (18, 0, 1),
                                    (19, 64, 1), (20, 0, 1), (21, 64, 1), (23, 192, 2), (24, 60, 1)] {
            set_param(m, pid, inst, value, bytes)?;
        }
    }
    Ok(())
}
fn picture(samples: &[i16], loop_start: usize, inc: f64, key: u8) -> Picture {
    let length = samples.len();
    let peak = samples.iter().map(|s| (*s as i32).unsigned_abs()).max().unwrap_or(1).max(1) as f64;
    let mut data = Vec::with_capacity(3 * N);
    for high in [false, true] {
        for i in 0..N {
            let a = i * length / N;
            let b = ((i + 1) * length / N).max(a + 1).min(length);
            let value = if high { *samples[a..b].iter().max().unwrap() } else { *samples[a..b].iter().min().unwrap() };
            data.push(((value as f64 / peak * 127.0).round().clamp(-127.0, 127.0) as i8) as u8);
        }
    }
    let mut shape = vec![0.0; N];
    let mut span = 0usize;
    if loop_start < length {
        span = length - loop_start;
        let frequency = 440.0 * 2.0f64.powf((key as f64 - 69.0) / 12.0);
        let period = SAMPLE_RATE * inc / frequency;
        if span as f64 > 4.5 * period { span = (3.0 * period).round().max(2.0) as usize; }
        span = span.min(length - loop_start);
        // the loop closes on its own first sample; a part of it ends on the sample that follows
        let ring = |j: usize| samples[if j == span && span == length - loop_start { loop_start } else { loop_start + j }] as f64;
        for (i, value) in shape.iter_mut().enumerate() {
            let position = i as f64 * span as f64 / (N - 1) as f64;
            let low = (position.floor() as usize).min(span);
            let high = (low + 1).min(span);
            *value = ring(low) + (ring(high) - ring(low)) * (position - low as f64);
        }
    }
    let shape_peak = shape.iter().fold(1.0f64, |a, b| a.max(b.abs()));
    data.extend(shape.iter().map(|x| ((x / shape_peak * 127.0).round().clamp(-127.0, 127.0) as i8) as u8));
    Picture { data, row: vec![json!(length), json!(loop_start), json!(peak as u32), json!(span),
                              json!((inc * 1_000_000.0).round() / 1_000_000.0), json!(key)] }
}
fn probe(m: &mut Machine, key: u8, at: &mut BTreeMap<u8, Option<(u32, usize)>>,
         pictures: &mut Vec<Picture>, specs: &mut Vec<WaveSpec>, known: &mut HashMap<(u32, usize), usize>) -> io::Result<()> {
    setup::send(m, &[0x90, key, 100], 0.03)?;
    let shots = m.devices().wave_shots(LIMIT);
    let spec = shots.first().and_then(|shot| m.devices().sound.wave_spec(shot.0, key));
    setup::send(m, &[0x80, key, 0], 0.05)?;
    if shots.len() != 1 || shots[0].4.is_empty() { at.insert(key, None); return Ok(()); }
    let (_, address, inc, loop_start, samples) = &shots[0];
    let id = (*address, samples.len());
    at.insert(key, Some(id));
    if let Some(&i) = known.get(&id) {
        let row = &mut pictures[i].row;
        if row.len() == 6 {
            let original_key = row[5].as_u64().unwrap_or(key as u64) as i32;
            if original_key != key as i32 {
                let original_inc = row[4].as_f64().unwrap_or(*inc);
                let track = 12.0 * (inc / original_inc).log2() / (key as i32 - original_key) as f64;
                row.push(json!((track * 100.0).round() / 100.0));
            }
        }
    } else {
        known.insert(id, pictures.len());
        pictures.push(picture(samples, *loop_start, *inc, key));
        specs.push(spec.ok_or_else(|| invalid("missing wave registers"))?);
    }
    Ok(())
}
fn narrow(m: &mut Machine, low: u8, high: u8, at: &mut BTreeMap<u8, Option<(u32, usize)>>,
          pictures: &mut Vec<Picture>, specs: &mut Vec<WaveSpec>, known: &mut HashMap<(u32, usize), usize>) -> io::Result<()> {
    if at[&low] == at[&high] || high - low < 2 { return Ok(()); }
    let mid = (low + high) / 2;
    probe(m, mid, at, pictures, specs, known)?;
    narrow(m, low, mid, at, pictures, specs, known)?;
    narrow(m, mid, high, at, pictures, specs, known)
}
fn one(m: &mut Machine, job: &Job) -> io::Result<WaveWork> {
    set_param(m, 3, job.inst, job.wave as u16 + job.base, 3)?;
    let (mut at, mut pictures, mut specs, mut known) = (BTreeMap::new(), Vec::new(), Vec::new(), HashMap::new());
    for key in (12u8..=108).step_by(6) { probe(m, key, &mut at, &mut pictures, &mut specs, &mut known)?; }
    for low in (12u8..=102).step_by(6) { narrow(m, low, low + 6, &mut at, &mut pictures, &mut specs, &mut known)?; }
    let mut splits = Vec::new();
    let mut prior = None;
    for (key, id) in at {
        if let Some(id) = id {
            if prior != Some(id) { splits.push([key as usize, known[&id]]); prior = Some(id); }
        }
    }
    Ok(WaveWork { splits, pictures, specs })
}
fn checkpoint(work: &Path, index: usize, value: &WaveWork) -> io::Result<()> {
    let temp = work.join(format!("{index:04}.part"));
    let dest = work.join(format!("{index:04}.json"));
    let mut file = setup::private_file(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    file.sync_all()?;
    fs::rename(temp, dest)
}
fn jobs() -> io::Result<Vec<Job>> {
    let data: Value = serde_json::from_str(include_str!("../assets/data.json"))?;
    let mut all = Vec::new();
    for (block, inst, base) in [("synth", 0, 1), ("pcm", 2, 326), ("noise", 5, 312)] {
        let count = data["waves"][block].as_array().ok_or_else(|| invalid("missing wave names"))?.len();
        all.extend((0..count).map(|wave| Job { block, inst, wave, base }));
    }
    Ok(all)
}
fn existing(path: &Path) -> bool { fs::read(path).ok().and_then(|b| serde_json::from_slice::<WaveWork>(&b).ok()).is_some() }
fn assemble(all: &[Job], work: &Path, target: &Path) -> io::Result<()> {
    let mut blocks: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut morph_blocks: serde_json::Map<String, Value> = serde_json::Map::new();
    for job in all { blocks.entry(job.block).or_insert_with(|| json!([])); }
    for job in all { morph_blocks.entry(job.block).or_insert_with(|| json!([])); }
    let (mut rows, mut payload) = (Vec::<Vec<Value>>::new(), Vec::<u8>::new());
    let mut specs = Vec::<WaveSpec>::new();
    let mut seen = HashMap::<(String, Vec<u8>, String), usize>::new();
    for (index, job) in all.iter().enumerate() {
        let mut wave: WaveWork = serde_json::from_slice(&fs::read(work.join(format!("{index:04}.json")))?)?;
        for (picture, spec) in wave.pictures.iter().zip(&mut wave.specs) {
            spec.track = picture.row.get(6).and_then(Value::as_f64).unwrap_or(1.0);
        }
        let mut splits = Vec::new();
        let mut morph_splits = Vec::new();
        for [key, local] in wave.splits {
            let pic = wave.pictures.get(local).ok_or_else(|| invalid("bad wave checkpoint"))?;
            morph_splits.push([key, specs.len() + local]);
            let identity = (job.block.to_string(), pic.data.clone(), serde_json::to_string(&pic.row)?);
            let shot = if let Some(&id) = seen.get(&identity) { id } else {
                let id = rows.len();
                if pic.data.len() != 3 * N { return Err(invalid("bad wave picture length")); }
                payload.extend(&pic.data);
                rows.push(pic.row.clone());
                seen.insert(identity, id);
                id
            };
            splits.push([key, shot]);
        }
        blocks.get_mut(job.block).unwrap().as_array_mut().unwrap().push(json!(splits));
        morph_blocks.get_mut(job.block).unwrap().as_array_mut().unwrap().push(json!(morph_splits));
        specs.extend(wave.specs);
    }
    let bin_temp = target.join("waves.bin.part");
    let json_temp = target.join("waves.json.part");
    let morph_temp = target.join("wave_morph.json.part");
    let _ = fs::remove_file(&bin_temp);
    let _ = fs::remove_file(&json_temp);
    let _ = fs::remove_file(&morph_temp);
    let mut bin = setup::private_file(&bin_temp)?;
    bin.write_all(&payload)?; bin.sync_all()?;
    let mut index = setup::private_file(&json_temp)?;
    serde_json::to_writer(&mut index, &json!({"n": N, "blocks": blocks, "shots": rows}))?;
    index.sync_all()?;
    let mut morph_index = setup::private_file(&morph_temp)?;
    serde_json::to_writer(&mut morph_index, &json!({"blocks": morph_blocks, "shots": specs}))?;
    morph_index.sync_all()?;
    fs::rename(bin_temp, target.join("waves.bin"))?;
    fs::rename(json_temp, target.join("waves.json"))?;
    fs::rename(morph_temp, target.join("wave_morph.json"))?;
    eprintln!("wave previews: {} shots, {} bytes", rows.len(), payload.len());
    Ok(())
}

/// Build optional previews. Each wave is stored separately until the final pair is ready.
pub fn build(workers: usize) -> io::Result<()> {
    setup::validate()?;
    let lock = OpenOptions::new().create(true).write(true).open(setup::data_root().join(".waves.lock"))?;
    lock.lock()?;
    let all = Arc::new(jobs()?);
    let work = setup::data_root().join(format!(".waves-{}-v{WORK_VERSION}", setup::IMAGE_HASH));
    setup::private_dir(&work)?;
    for entry in fs::read_dir(&work)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().ends_with(".part") { fs::remove_file(entry.path())?; }
    }
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(Mutex::new(None::<String>));
    std::thread::scope(|scope| {
        for _ in 0..workers.clamp(1, 8) {
            let (all, work, next, done, failed) = (all.clone(), work.clone(), next.clone(), done.clone(), failed.clone());
            scope.spawn(move || {
                let worker = || -> io::Result<()> {
                    let flash = image::load(&setup::image_path())?;
                    let mut machine = Machine::new(flash).map_err(invalid)?;
                    setup::run(&mut machine, 7.0)?;
                    setup::send(&mut machine, &[0xB0, 0, 98, 0xB0, 0x20, 0, 0xC0, 0], 1.0)?;
                    prepare(&mut machine)?;
                    let mut active = None;
                    loop {
                        if failed.lock().unwrap().is_some() { break; }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= all.len() { break; }
                        let path = work.join(format!("{index:04}.json"));
                        if !existing(&path) {
                            let job = &all[index];
                            if active != Some(job.inst) {
                                if let Some(inst) = active { set_param(&mut machine, 0, inst, 0, 1)?; }
                                set_param(&mut machine, 0, job.inst, 1, 1)?;
                                active = Some(job.inst);
                            }
                            checkpoint(&work, index, &one(&mut machine, job)?)?;
                        }
                        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if n % 25 == 0 || n == all.len() { eprintln!("wave previews: {n}/{}", all.len()); }
                    }
                    Ok(())
                };
                if let Err(e) = worker() { *failed.lock().unwrap() = Some(e.to_string()); }
            });
        }
    });
    if let Some(error) = failed.lock().unwrap().take() { return Err(invalid(error)); }
    assemble(&all, &work, &setup::generated_dir())?;
    fs::remove_dir_all(work)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sound::{WaveLibrary, WaveMorph};

    #[test]
    #[ignore = "needs user-supplied firmware and generated wave index"]
    fn morph_uses_the_firmware_waves() {
        let index: WaveLibrary = serde_json::from_slice(&fs::read(setup::generated_dir().join("wave_morph.json")).unwrap()).unwrap();
        let library = Arc::new(index);
        let play = |inst: u8, a: u16, b: u16, amount: u8| {
            let mut m = Machine::new(image::load(&setup::image_path()).unwrap()).unwrap();
            setup::run(&mut m, 7.0).unwrap();
            setup::send(&mut m, &[0xB0, 0, 98, 0xB0, 0x20, 0, 0xC0, 0], 1.0).unwrap();
            prepare(&mut m).unwrap();
            set_param(&mut m, 0, inst, 1, 1).unwrap();
            set_param(&mut m, 3, inst, a + [1, 1, 326, 326, 0, 312][inst as usize], 3).unwrap();
            let mut first = [0; 5]; let mut second = [0; 5];
            first[inst as usize] = a; second[inst as usize] = b;
            m.devices().sound.set_wave_library(library.clone());
            m.devices().sound.set_wave_morph(Some(WaveMorph { amount, a: first, b: second }));
            m.devices().sound.set_morph_note(60);
            m.midi_in(&[0x90, 60, 100]);
            assert!(m.run(4096));
            m.out.clone()
        };
        for (inst, a, b) in [(0, 0, 1), (2, 0, 1)] {
            let x = play(inst, a, b, 0);
            let y = play(inst, a, b, 100);
            let change: f32 = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).sum();
            assert!(change > 0.01, "block {inst}: the alternate wave did not change the sound ({change})");
        }
    }
    #[test]
    #[ignore = "needs user-supplied firmware"]
    fn first_synth_wave() {
        let file = std::env::var_os("XWP1_TEST_IMAGE").map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("../firmware/win/XW-P1 Updater/p1-update.bin"));
        let flash = image::load(&file).unwrap();
        let mut machine = Machine::new(flash).unwrap();
        setup::run(&mut machine, 7.0).unwrap();
        setup::send(&mut machine, &[0xB0, 0, 98, 0xB0, 0x20, 0, 0xC0, 0], 1.0).unwrap();
        prepare(&mut machine).unwrap();
        set_param(&mut machine, 0, 0, 1, 1).unwrap();
        let output = one(&mut machine, &Job { block: "synth", inst: 0, wave: 0, base: 1 }).unwrap();
        assert!(!output.splits.is_empty());
        assert!(output.pictures.iter().all(|p| p.data.len() == 3 * N));
        set_param(&mut machine, 0, 0, 0, 1).unwrap();
        for (inst, block, base) in [(2, "pcm", 326), (5, "noise", 312)] {
            set_param(&mut machine, 0, inst, 1, 1).unwrap();
            let output = one(&mut machine, &Job { block, inst, wave: 0, base }).unwrap();
            assert!(!output.splits.is_empty(), "{block}");
            assert!(output.pictures.iter().all(|p| p.data.len() == 3 * N));
            set_param(&mut machine, 0, inst, 0, 1).unwrap();
        }
    }
}
