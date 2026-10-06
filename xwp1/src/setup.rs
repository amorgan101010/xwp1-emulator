//! Local import of a user supplied XW-P1 1.11 updater. No updater program is run.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use crate::{image, machine::Machine, sound::SAMPLE_RATE};

pub const IMAGE_HASH: &str = "c94adf39918f627773844221d3dcf8c72e29ae66a061fb88994a2ab603475212";
pub const IMAGE_SIZE: u64 = 32_506_880;
const SCHEMA: u32 = 2;
const MEMBERS: [&str; 2] = ["XW-P1 Updater/p1-update.bin", "XW-P1 Updater.app/Contents/Resources/p1-update.bin"];

pub fn data_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("XWP1_DATA_HOME") { return PathBuf::from(dir); }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") { return PathBuf::from(dir).join("xwp1"); }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/xwp1")
}

pub fn image_path() -> PathBuf { data_root().join("firmware/p1-update.bin") }
pub fn generated_dir() -> PathBuf { data_root().join("assets").join(format!("1.11-{IMAGE_HASH}-v{SCHEMA}")) }

/// Static panel files installed beside the executable, with a checkout override for development.
pub fn panel_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XWP1_PANEL_DIR") { return dir.into(); }
    let installed = if let Some(dir) = std::env::var_os("XWP1_PREFIX") { PathBuf::from(dir) }
        else if let Some(dir) = std::env::var_os("XDG_DATA_HOME") { PathBuf::from(dir).join("xwp1-app") }
        else { PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/xwp1-app") };
    let panel = installed.join("share/xwp1/panel");
    if panel.join("index.html").exists() { return panel; }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(Path::parent) {
            let installed = prefix.join("share/xwp1/panel");
            if installed.join("index.html").exists() { return installed; }
        }
    }
    if let Ok(here) = std::env::current_dir() {
        let dev = here.join("xwp1/panel");
        if dev.join("index.html").exists() { return dev; }
    }
    PathBuf::from("xwp1/panel")
}

fn invalid(message: impl Into<String>) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message.into()) }
fn hash_file(path: &Path) -> io::Result<String> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buf = [0u8; 65536];
    loop { let n = input.read(&mut buf)?; if n == 0 { break; } digest.update(&buf[..n]); }
    Ok(format!("{:x}", digest.finalize()))
}
fn sync_dir(path: &Path) -> io::Result<()> { File::open(path)?.sync_all() }
pub(crate) fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub(crate) fn private_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

pub fn validate() -> io::Result<()> {
    let path = image_path();
    if fs::metadata(&path)?.len() != IMAGE_SIZE || hash_file(&path)? != IMAGE_HASH {
        return Err(invalid("installed firmware is not the supported XW-P1 1.11 image"));
    }
    let dir = generated_dir();
    let manifest: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    if manifest["image"] != IMAGE_HASH || manifest["schema"] != SCHEMA || manifest["complete"] != true {
        return Err(invalid("asset manifest does not match installed firmware"));
    }
    let files = manifest["files"].as_object().ok_or_else(|| invalid("asset manifest has no files"))?;
    for (name, expected) in files {
        if !["data.json", "hex.json", "drawbar.json", "pcm.json", "mem.json", "hex_mem.json",
              "draw_mem.json", "pcm_mem.json", "perf_mem.json"].contains(&name.as_str()) {
            return Err(invalid("asset manifest contains an unknown file"));
        }
        if expected.as_str() != Some(&hash_file(&dir.join(name))?) { return Err(invalid(format!("damaged generated asset: {name}"))); }
    }
    if files.len() != 9 { return Err(invalid("incomplete generated assets")); }
    for (name, field, count) in [("data.json", "presets", 100), ("hex.json", "presets", 50),
                                  ("drawbar.json", "presets", 50), ("pcm.json", "tones", 429)] {
        let data: Value = serde_json::from_slice(&fs::read(dir.join(name))?)?;
        if data[field].as_array().map(Vec::len) != Some(count) || data["params"].as_array().is_none() {
            return Err(invalid(format!("invalid generated {name}")));
        }
    }
    for (name, count) in [("mem.json", 407), ("hex_mem.json", 123), ("draw_mem.json", 16),
                          ("pcm_mem.json", 10), ("perf_mem.json", 173)] {
        let data: Value = serde_json::from_slice(&fs::read(dir.join(name))?)?;
        let cells = data["cells"].as_array().ok_or_else(|| invalid(format!("invalid {name}")))?;
        if cells.len() != count || cells.iter().any(|row| row.as_array().is_none_or(|r| r.len() != 8)) {
            return Err(invalid(format!("invalid {name} cells")));
        }
    }
    Ok(())
}

fn copy_input(input: &Path, target: &mut File) -> io::Result<()> {
    let is_zip = input.extension().is_some_and(|s| s.eq_ignore_ascii_case("zip"));
    if is_zip {
        let mut zip = zip::ZipArchive::new(File::open(input)?).map_err(|e| invalid(format!("invalid updater ZIP: {e}")))?;
        let mut found = None;
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i).map_err(|e| invalid(format!("invalid ZIP member: {e}")))?;
            if MEMBERS.contains(&entry.name()) || entry.name().ends_with("/p1-update.bin") || entry.name() == "p1-update.bin" {
                if found.is_some() { return Err(invalid("updater ZIP contains duplicate image members")); }
                if !MEMBERS.contains(&entry.name()) { return Err(invalid("unsupported updater ZIP image path")); }
                found = Some(i);
            }
        }
        let i = found.ok_or_else(|| invalid("updater ZIP has no supported p1-update.bin member"))?;
        let mut entry = zip.by_index(i).map_err(|e| invalid(format!("cannot read ZIP member: {e}")))?;
        if entry.size() != IMAGE_SIZE { return Err(invalid("unsupported updater image size")); }
        let copied = io::copy(&mut (&mut entry).take(IMAGE_SIZE + 1), target)?;
        if copied != IMAGE_SIZE { return Err(invalid("truncated updater image")); }
        if entry.read(&mut [0u8; 1])? != 0 { return Err(invalid("oversized updater image")); }
    } else {
        if fs::metadata(input)?.len() != IMAGE_SIZE { return Err(invalid("unsupported raw updater image size")); }
        io::copy(&mut File::open(input)?, target)?;
    }
    Ok(())
}

pub(crate) fn run(m: &mut Machine, seconds: f64) -> io::Result<()> {
    if !m.run((seconds * SAMPLE_RATE).ceil() as usize) {
        return Err(invalid(format!("firmware boot failed: {}", m.fault.as_deref().unwrap_or("CPU fault"))));
    }
    m.out.clear(); m.rev_out.clear();
    Ok(())
}
pub(crate) fn send(m: &mut Machine, bytes: &[u8], settle: f64) -> io::Result<()> {
    m.midi_in(bytes);
    run(m, bytes.len() as f64 * 2.0 / 3125.0 + 0.01 + settle)
}
fn request_name(m: &mut Machine, mark: &mut usize) -> io::Result<String> {
    let mut name = String::new();
    for index in 0..16u8 {
        let mut address = [0u8; 18];
        address[0] = 3; address[12] = 7; address[14] = index;
        let mut query = vec![0xF0, 0x44, 0x16, 0x03, 0x7F, 0];
        query.extend(address);
        query.push(0xF7);
        let mut value = None;
        for _ in 0..6 {
            let (_, at) = m.midi_out(*mark);
            send(m, &query, 0.03)?;
            let (answer, next) = m.midi_out(at);
            *mark = next;
            if let Some(row) = answer.windows(26).find(|r| r[..6] == [0xF0, 0x44, 0x16, 0x03, 0x7F, 1] && r[6] == 3 && r[18] == 7 && r[20] == index && r[25] == 0xF7) {
                value = Some(row[24]); break;
            }
        }
        let ch = value.ok_or_else(|| invalid(format!("firmware did not answer name character {index}")))?;
        name.push(if (32..127).contains(&ch) { ch as char } else { ' ' });
    }
    Ok(name.trim().to_string())
}
fn select(m: &mut Machine, bank: u8, program: u8) -> io::Result<()> {
    send(m, &[0xB0, 0, bank, 0xB0, 0x20, 0, 0xC0, program], 1.0)
}
fn names(m: &mut Machine, mark: &mut usize, bank: u8, count: u8) -> io::Result<Vec<String>> {
    let mut found = Vec::new();
    for program in 0..count {
        if program % 10 == 0 { eprintln!("reading bank {bank} names: {program}/{count}"); }
        select(m, bank, program)?;
        run(m, 0.3)?;
        found.push(request_name(m, mark)?);
    }
    Ok(found)
}
fn pcm_names(m: &mut Machine, mark: &mut usize) -> io::Result<Vec<String>> {
    let mut found = Vec::new();
    for tone in 200u16..629 {
        if tone % 20 == 0 { eprintln!("reading PCM tone names: {}/429", tone - 200); }
        let mut address = [0u8; 18]; address[0] = 2; address[12] = 0x69 & 127; address[13] = 0x69 >> 7;
        let mut cmd = vec![0xF0, 0x44, 0x16, 0x03, 0x7F, 1];
        cmd.extend(address); cmd.extend([tone as u8 & 127, (tone >> 7) as u8]); cmd.push(0xF7);
        send(m, &cmd, 0.6)?;
        found.push(request_name(m, mark)?);
    }
    Ok(found)
}
fn write_assets(m: &mut Machine, stage: &Path) -> io::Result<()> {
    let mut mark = m.midi_out(0).1;
    let definitions = [
        ("data.json", include_str!("../assets/data.json")),
        ("hex.json", include_str!("../assets/hex.json")),
        ("drawbar.json", include_str!("../assets/drawbar.json")),
        ("pcm.json", include_str!("../assets/pcm.json")),
    ];
    let mut files = serde_json::Map::new();
    for (name, source) in definitions {
        let mut data: Value = serde_json::from_str(source)?;
        match name {
            "data.json" => data["presets"] = json!(names(m, &mut mark, 98, 100)?),
            "hex.json" => data["presets"] = json!(names(m, &mut mark, 97, 50)?),
            "drawbar.json" => data["presets"] = json!(names(m, &mut mark, 96, 50)?),
            _ => data["tones"] = json!(pcm_names(m, &mut mark)?),
        }
        let path = stage.join(name);
        let mut file = private_file(&path)?;
        serde_json::to_writer(&mut file, &data)?;
        file.sync_all()?;
        files.insert(name.into(), json!(hash_file(&path)?));
    }
    for (name, source) in [
        ("mem.json", include_str!("../assets/mem.json")),
        ("hex_mem.json", include_str!("../assets/hex_mem.json")),
        ("draw_mem.json", include_str!("../assets/draw_mem.json")),
        ("pcm_mem.json", include_str!("../assets/pcm_mem.json")),
        ("perf_mem.json", include_str!("../assets/perf_mem.json")),
    ] {
        let path = stage.join(name);
        let mut file = private_file(&path)?;
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        files.insert(name.into(), json!(hash_file(&path)?));
    }
    let mut file = private_file(&stage.join("manifest.json"))?;
    serde_json::to_writer(&mut file, &json!({"image": IMAGE_HASH, "schema": SCHEMA, "complete": true, "files": files}))?;
    file.sync_all()?;
    sync_dir(stage)
}

/// Import and build into a private staging directory; the old install remains intact on error.
pub fn install(input: &Path) -> io::Result<()> {
    let root = data_root(); private_dir(&root)?; private_dir(&root.join("firmware"))?; private_dir(&root.join("assets"))?;
    let lock_path = root.join(".setup.lock");
    let lock = OpenOptions::new().create(true).write(true).open(&lock_path)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        lock.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    lock.lock()?;
    let already_valid = validate().is_ok();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".setup-") && entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        }
    }
    let stage = root.join(format!(".setup-{}", std::process::id()));
    private_dir(&stage)?;
    let result = (|| {
        let temporary = stage.join("p1-update.bin");
        let mut file = private_file(&temporary)?;
        copy_input(input, &mut file)?;
        file.sync_all()?;
        drop(file);
        if hash_file(&temporary)? != IMAGE_HASH { return Err(invalid("unsupported updater/image: expected XW-P1 firmware 1.11")); }
        if already_valid { return Ok(()); }
        let flash = image::load(&temporary)?;
        let mut machine = Machine::new(flash).map_err(invalid)?;
        run(&mut machine, 7.0)?;
        select(&mut machine, 98, 0)?;
        let mut mark = machine.midi_out(0).1;
        let name = request_name(&mut machine, &mut mark)?;
        if name.is_empty() { return Err(invalid("firmware boot test returned no panel data")); }
        eprintln!("firmware boot test: Solo 0 = {name}");
        let assets = stage.join("assets"); private_dir(&assets)?;
        write_assets(&mut machine, &assets)?;
        let active = generated_dir();
        let old = stage.join("old-assets");
        if active.exists() { fs::rename(&active, &old)?; }
        if let Err(e) = fs::rename(&assets, &active) {
            if old.exists() { let _ = fs::rename(&old, &active); }
            return Err(e);
        }
        sync_dir(&root.join("assets"))?;
        let destination = image_path();
        fs::rename(&temporary, &destination)?; sync_dir(&root.join("firmware"))?;
        validate()
    })();
    let _ = fs::remove_dir_all(&stage);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Temporary(PathBuf);
    impl Temporary {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "xwp1-setup-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    fn archive(path: &Path, members: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for &(name, bytes) in members {
            zip.start_file(name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn updater_zip_rejects_missing_duplicate_and_unexpected_image_paths() {
        let dir = Temporary::new();
        let input = dir.0.join("update.ZIP");
        let output = dir.0.join("image.bin");
        let cases: &[(&[(&str, &[u8])], &str)] = &[
            (&[("readme.txt", b"hello")], "no supported"),
            (&[("other/p1-update.bin", b"bad")], "unsupported updater ZIP image path"),
            (&[(MEMBERS[0], b"one"), (MEMBERS[1], b"two")], "duplicate image members"),
            (&[(MEMBERS[0], b"short")], "unsupported updater image size"),
        ];
        for &(members, message) in cases {
            archive(&input, members);
            let mut target = File::create(&output).unwrap();
            let error = copy_input(&input, &mut target).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains(message), "{error}");
        }
    }

    #[test]
    fn raw_image_requires_exact_size() {
        let dir = Temporary::new();
        let input = dir.0.join("p1-update.bin");
        let output = dir.0.join("copied.bin");
        let source = File::create(&input).unwrap();
        source.set_len(IMAGE_SIZE - 1).unwrap();
        let mut target = File::create(&output).unwrap();
        assert_eq!(copy_input(&input, &mut target).unwrap_err().kind(), io::ErrorKind::InvalidData);
        source.set_len(IMAGE_SIZE).unwrap();
        copy_input(&input, &mut target).unwrap();
        assert_eq!(fs::metadata(output).unwrap().len(), IMAGE_SIZE);
    }
}
