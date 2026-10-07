//! The SD card slot: the SoC's second serial unit (0x2a003940, used as an SPI master), the two DMA channels
//! that move whole blocks through it (0x2e002400) and a card in SPI mode over an image of its sectors. The
//! firmware brings its own FAT driver, so Card Save / Card Load / Format and the music player work on the image
//! as they do on a card (docs/FINDINGS.md, "SD card").
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

pub const SERIAL: u32 = 0x2A00_3940;
pub const SERIAL_SIZE: u32 = 0x20;
pub const SERIAL_IRQ: u32 = 28;
pub const DMA: u32 = 0x2E00_2400;
pub const DMA_SIZE: u32 = 0x60;
/// The channels' completion interrupts (the firmware's masks 0x40 and 0x80).
pub const DMA_IRQ: [u32; 2] = [6, 7];
/// Port 2 bit 5 is the slot's switch: low with a card in it.
pub const DETECT: (usize, u32) = (2, 1 << 5);
/// Port 1 bit 2 is the card's write-protect tab: high when it is set.
pub const PROTECT: (usize, u32) = (1, 1 << 2);
pub const BLOCK: usize = 512;

/// The sectors of a card.
pub enum Store {
    File(File),
    Memory(Vec<u8>),
}

impl Store {
    fn len(&self) -> u64 {
        match self {
            Store::File(f) => f.metadata().map(|m| m.len()).unwrap_or(0),
            Store::Memory(m) => m.len() as u64,
        }
    }

    fn read(&self, lba: u64, block: &mut [u8]) {
        match self {
            Store::File(f) => {
                if f.read_exact_at(block, lba * BLOCK as u64).is_err() {
                    block.fill(0);
                }
            }
            Store::Memory(m) => block.copy_from_slice(&m[lba as usize * BLOCK..][..BLOCK]),
        }
    }

    fn write(&mut self, lba: u64, block: &[u8]) -> bool {
        match self {
            Store::File(f) => f.write_all_at(block, lba * BLOCK as u64).is_ok(),
            Store::Memory(m) => {
                m[lba as usize * BLOCK..][..BLOCK].copy_from_slice(block);
                true
            }
        }
    }
}

fn crc7(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        for bit in (0..8).rev() {
            crc <<= 1;
            if (byte >> bit & 1) ^ (crc >> 7 & 1) != 0 {
                crc ^= 0x09;
            }
        }
    }
    (crc & 0x7F) << 1 | 1
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { crc << 1 ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

enum Incoming {
    Token,
    Data(Vec<u8>),
}

/// An SD card answering in SPI mode.
pub struct Card {
    store: Store,
    blocks: u64,
    high: bool,
    read_only: bool,
    idle: bool,
    app: bool,
    command: Vec<u8>,
    out: VecDeque<u8>,
    reading: Option<u64>,                  // multiple-block read: the next block
    writing: Option<(u64, bool, Incoming)>, // block, multiple, what is expected
    /// Blocks written since the card went in (for those who keep a copy elsewhere).
    pub written: u64,
    /// Commands the model does not know, as the firmware sent them.
    pub unknown: Vec<u8>,
    /// When set: every command and its argument.
    pub log: Option<Vec<(u8, u32)>>,
}

impl Card {
    pub fn new(store: Store, read_only: bool) -> Card {
        let mut blocks = store.len() / BLOCK as u64;
        // Always a high-capacity card (block addresses), whatever the size: as a standard-capacity one the
        // firmware's writes arrived at 512 times the address (FINDINGS, open).
        let high = true;
        blocks -= blocks % 1024; // what the card's own description can express
        Card { store, blocks, high, read_only, idle: true, app: false, command: Vec::new(), out: VecDeque::new(),
               reading: None, writing: None, written: 0, unknown: Vec::new(), log: None }
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn into_store(self) -> Store {
        self.store
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    fn csd(&self) -> [u8; 16] {
        let mut csd = if self.high {
            let c = (self.blocks / 1024 - 1) as u32;
            [0x40, 0x0E, 0x00, 0x32, 0x5B, 0x59, 0x00, (c >> 16) as u8 & 0x3F, (c >> 8) as u8, c as u8, 0x7F, 0x80, 0x0A,
             0x40, 0x00, 0]
        } else {
            let c = (self.blocks / 512 - 1) as u32; // block length 512, multiplier 512
            [0x00, 0x26, 0x00, 0x32, 0x5F, 0x59, 0x80 | (c >> 10) as u8 & 3, (c >> 2) as u8, (c as u8 & 3) << 6 | 0x2D,
             0xDB, 0xFF, 0x80, 0x0A, 0x40, 0x00, 0]
        };
        csd[15] = crc7(&csd[..15]);
        csd
    }

    fn cid(&self) -> [u8; 16] {
        let mut cid = *b"\x7dXWXWP1E\x10\x00\x00\x00\x01\x01\x4a\x00";
        cid[15] = crc7(&cid[..15]);
        cid
    }

    fn data(&mut self, data: &[u8]) {
        self.out.extend([0xFF, 0xFE]);
        self.out.extend(data);
        self.out.extend(crc16(data).to_be_bytes());
    }

    fn block(&mut self, lba: u64) {
        let mut block = [0u8; BLOCK];
        self.store.read(lba, &mut block);
        self.data(&block);
    }

    fn address(&self, arg: u32) -> Option<u64> {
        let lba = if self.high { arg as u64 } else { arg as u64 / BLOCK as u64 };
        (lba < self.blocks).then_some(lba)
    }

    fn execute(&mut self) {
        let (cmd, arg) = (self.command[0] & 0x3F, u32::from_be_bytes(self.command[1..5].try_into().unwrap()));
        let app = std::mem::take(&mut self.app);
        if let Some(log) = &mut self.log {
            log.push((cmd | (app as u8) << 7, arg));
        }
        let r1 = self.idle as u8;
        self.out.clear();
        self.out.push_back(0xFF);
        match (app, cmd) {
            (_, 0) => {
                self.idle = true;
                (self.reading, self.writing) = (None, None);
                self.out.push_back(1);
            }
            (_, 8) => self.out.extend([r1, 0, 0, 1, arg as u8]),
            (_, 55) => {
                self.app = true;
                self.out.push_back(r1);
            }
            (true, 41) | (false, 1) => {
                self.idle = false;
                self.out.push_back(0);
            }
            (_, 58) => {
                let ocr = 0x00FF_8000 | (!self.idle as u32) << 31 | (self.high as u32) << 30;
                self.out.push_back(r1);
                self.out.extend(ocr.to_be_bytes());
            }
            (_, 9) => {
                self.out.push_back(r1);
                self.data(&self.csd());
            }
            (_, 10) => {
                self.out.push_back(r1);
                self.data(&self.cid());
            }
            (true, 13) => {
                self.out.extend([r1, 0]);
                self.data(&[0; 64]);
            }
            (true, 51) => {
                self.out.push_back(r1);
                self.data(&[if self.high { 0x02 } else { 0x01 }, 0x25, 0, 0, 0, 0, 0, 0]);
            }
            (false, 13) => self.out.extend([r1, 0]),
            (false, 12) => {
                self.reading = None;
                self.out.extend([0xFF, r1]); // a stuff byte comes before this answer
            }
            (_, 16) | (_, 59) | (true, 23) | (true, 42) => self.out.push_back(r1),
            (false, 17) | (false, 18) => match self.address(arg) {
                Some(lba) => {
                    self.out.push_back(r1);
                    self.block(lba);
                    self.reading = (cmd == 18).then_some(lba + 1);
                }
                None => self.out.push_back(r1 | 0x40),
            },
            (false, 24) | (false, 25) => match self.address(arg) {
                Some(lba) => {
                    self.out.push_back(r1);
                    self.writing = Some((lba, cmd == 25, Incoming::Token));
                }
                None => self.out.push_back(r1 | 0x40),
            },
            _ => {
                self.unknown.push(cmd | (app as u8) << 7);
                self.out.push_back(r1 | 0x04);
            }
        }
    }

    /// One byte each way.
    pub fn exchange(&mut self, tx: u8) -> u8 {
        let rx = self.out.pop_front().unwrap_or(0xFF);
        if let Some((lba, multiple, incoming)) = &mut self.writing {
            match incoming {
                Incoming::Token => match tx {
                    0xFE | 0xFC => *incoming = Incoming::Data(Vec::with_capacity(BLOCK + 2)),
                    0xFD => {
                        self.writing = None;
                        self.out.extend([0xFF, 0x00]); // busy for a byte, then released
                    }
                    _ => {}
                },
                Incoming::Data(data) => {
                    data.push(tx);
                    if data.len() == BLOCK + 2 {
                        let stored = !self.read_only && *lba < self.blocks && self.store.write(*lba, &data[..BLOCK]);
                        self.written += stored as u64;
                        // the data response (accepted / write error), one busy byte
                        self.out.extend([if stored { 0x05 } else { 0x0D }, 0x00]);
                        if *multiple {
                            *lba += 1;
                            *incoming = Incoming::Token;
                        } else {
                            self.writing = None;
                        }
                    }
                }
            }
            return rx;
        }
        if !self.command.is_empty() || tx & 0xC0 == 0x40 {
            self.command.push(tx);
            if self.command.len() == 6 {
                self.execute();
                self.command.clear();
            }
        } else if self.out.is_empty() {
            if let Some(lba) = self.reading {
                if lba < self.blocks {
                    self.block(lba);
                    self.reading = Some(lba + 1);
                } else {
                    self.reading = None;
                }
            }
        }
        rx
    }
}

#[derive(Default, Clone, Copy)]
struct Channel {
    src: u32,
    dst: u32,
    count: u32,
    config: u32,
    control: u32,
}

/// A block transfer the DMA has been told to start: `count` bytes from `src` to `dst`, each address fixed or
/// counting up.
pub struct Transfer {
    pub channel: usize,
    pub src: u32,
    pub dst: u32,
    pub count: u32,
    pub src_fixed: bool,
    pub dst_fixed: bool,
}

/// The serial unit, the DMA channels' registers and the card in the slot.
#[derive(Default)]
pub struct Slot {
    pub card: Option<Card>,
    control: u32,
    mode: u32,
    request: u32,
    rx: u8,
    channels: [Channel; 2],
    /// A transfer through the serial unit waits for the byte that sets it going: the first byte sent, or the
    /// first read of the receive register.
    pub waiting: Option<Transfer>,
    /// Bytes exchanged with the card or the empty slot (a card being used shows here).
    pub exchanged: u64,
}

impl Slot {
    fn exchange(&mut self, tx: u8) {
        self.exchanged += 1;
        self.rx = self.card.as_mut().map_or(0xFF, |card| card.exchange(tx));
    }

    /// -> the value, and whether a byte went over the line (the unit's interrupt).
    pub fn read(&mut self, addr: u32) -> (u32, bool) {
        match addr - SERIAL {
            0x00 => (self.control & !1, false), // bit 0: a byte under way; here it is over at once
            0x04 => (self.mode, false),
            0x08 => {
                let value = self.rx as u32;
                // enabled and receiving: taking a byte clocks the next one in
                let next = self.control & 0xC0 == 0x80;
                if next {
                    self.exchange(0xFF);
                }
                (value, next)
            }
            0x10 => (self.rx as u32, false), // the byte again, nothing clocked: the last of a block is taken here
            0x1C => (self.request, false),
            _ => (0, false),
        }
    }

    pub fn write(&mut self, addr: u32, value: u32) -> bool {
        match addr - SERIAL {
            0x00 => self.control = value & 0xFF,
            0x04 => self.mode = value & 0xFF,
            0x0C => {
                self.exchange(value as u8);
                return true;
            }
            0x1C => self.request = value & 0xFFFF,
            _ => {}
        }
        false
    }

    pub fn dma_read(&self, addr: u32) -> u32 {
        let off = addr - DMA;
        let channel = |n: u32| &self.channels[(n & 1) as usize];
        match off {
            0x00 | 0x08 => channel(off >> 3).src,
            0x04 | 0x0C => channel(off >> 3).dst,
            0x20 | 0x24 => channel(off >> 2).count,
            0x30 | 0x34 => channel(off >> 2).config,
            0x50 | 0x54 => channel(off >> 2).control & !0x10, // bit 4: transferring; never seen so
            _ => 0,
        }
    }

    /// -> the transfer this write starts.
    pub fn dma_write(&mut self, addr: u32, value: u32) -> Option<Transfer> {
        let off = addr - DMA;
        match off {
            0x00 | 0x08 => self.channels[(off >> 3) as usize].src = value,
            0x04 | 0x0C => self.channels[(off >> 3) as usize].dst = value,
            0x20 | 0x24 => self.channels[(off >> 2 & 1) as usize].count = value,
            0x30 | 0x34 => self.channels[(off >> 2 & 1) as usize].config = value,
            0x50 | 0x54 => {
                let n = (off >> 2 & 1) as usize;
                let c = &mut self.channels[n];
                let start = value & 1 != 0 && c.control & 1 == 0;
                c.control = value;
                if start {
                    c.control |= 0x80; // finished (the firmware's handler clears it)
                    return Some(Transfer { channel: n, src: c.src, dst: c.dst, count: (c.count & 0xFFFF) + 1,
                                           src_fixed: c.config >> 14 & 3 == 2, dst_fixed: c.config >> 12 & 3 == 2 });
                }
            }
            _ => {}
        }
        None
    }
}

/// The folder the instrument keeps its files in.
pub const FOLDER: &str = "MUSICDAT";
/// A new card image: 1 GB, of which the file takes what is written (it is sparse).
pub const NEW_SIZE: u64 = 1 << 30;
const PARTITION: u64 = 2048; // first block of the one partition of an image made here

/// The image's first partition as a volume (or the whole image, when it has no partition table).
struct Volume {
    file: File,
    start: u64,
    len: u64,
    at: u64,
}

impl Read for Volume {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.len - self.at.min(self.len)) as usize;
        let n = self.file.read_at(&mut buf[..n], self.start + self.at)?;
        self.at += n as u64;
        Ok(n)
    }
}

impl Write for Volume {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.len - self.at.min(self.len)) as usize;
        let n = self.file.write_at(&buf[..n], self.start + self.at)?;
        self.at += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for Volume {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let at = match to {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.len as i64 + n,
            SeekFrom::Current(n) => self.at as i64 + n,
        };
        self.at = u64::try_from(at).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "before the volume"))?;
        Ok(self.at)
    }
}

fn volume(path: &Path) -> io::Result<Volume> {
    let file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut first = [0u8; BLOCK];
    file.read_exact_at(&mut first, 0)?;
    let entry = &first[0x1BE..0x1CE];
    let (start, count) = (u32::from_le_bytes(entry[8..12].try_into().unwrap()) as u64, u32::from_le_bytes(entry[12..16].try_into().unwrap()) as u64);
    let blocks = file.metadata()?.len() / BLOCK as u64;
    // a boot sector begins with a jump; a partition table does not
    let whole = matches!(first[0], 0xEB | 0xE9) || entry[4] == 0 || start == 0 || start + count > blocks;
    let (start, len) = if whole { (0, blocks) } else { (start, count) };
    Ok(Volume { file, start: start * BLOCK as u64, len: len * BLOCK as u64, at: 0 })
}

fn files(path: &Path) -> io::Result<fatfs::FileSystem<Volume>> {
    fatfs::FileSystem::new(volume(path)?, fatfs::FsOptions::new())
}

/// Make a card image at `path`: one FAT partition with the instrument's folder in it, as its own Format leaves a
/// card.
pub fn create(path: &Path, bytes: u64) -> io::Result<()> {
    let blocks = bytes / BLOCK as u64 / 1024 * 1024;
    if blocks <= PARTITION * 2 || blocks > u32::MAX as u64 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a card image is between 2 MB and 2 TB"));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
    file.set_len(blocks * BLOCK as u64)?;
    let count = blocks - PARTITION;
    let fat32 = count > 4 << 20; // above 2 GB
    let mut table = [0u8; BLOCK];
    table[0x1BE..0x1CE].copy_from_slice(&[0, 0xFE, 0xFF, 0xFF, if fat32 { 0x0C } else { 0x06 }, 0xFE, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0]);
    table[0x1C6..0x1CA].copy_from_slice(&(PARTITION as u32).to_le_bytes());
    table[0x1CA..0x1CE].copy_from_slice(&(count as u32).to_le_bytes());
    table[510..].copy_from_slice(&[0x55, 0xAA]);
    file.write_all_at(&table, 0)?;
    let mut volume = Volume { file, start: PARTITION * BLOCK as u64, len: count * BLOCK as u64, at: 0 };
    let options = fatfs::FormatVolumeOptions::new().volume_label(*b"XW-P1      ")
        .fat_type(if fat32 { fatfs::FatType::Fat32 } else { fatfs::FatType::Fat16 });
    fatfs::format_volume(&mut volume, options)?;
    volume.at = 0;
    let fs = fatfs::FileSystem::new(volume, fatfs::FsOptions::new())?;
    fs.root_dir().create_dir(FOLDER)?;
    fs.unmount()
}

/// The image at `path`, made first if it is not there (a new one is `NEW_SIZE`, formatted). None, and a line
/// on stderr, when that fails.
pub fn ready(path: &Path) -> Option<std::path::PathBuf> {
    if !path.exists() {
        if let Err(e) = create(path, NEW_SIZE) {
            eprintln!("card: {} was not made: {e}", path.display());
            return None;
        }
        eprintln!("card: made {}", path.display());
    }
    Some(path.to_path_buf())
}

/// A file name as the instrument writes them: eight characters, a dot, three; upper case.
pub fn file_name(name: &str) -> Option<String> {
    let name = name.trim().to_ascii_uppercase();
    let (stem, ext) = name.rsplit_once('.')?;
    let fits = |part: &str, most: usize| (1..=most).contains(&part.len())
        && part.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"$&_'()-^{}@~`".contains(&b));
    (fits(stem, 8) && fits(ext, 3)).then_some(name)
}

/// The files in the instrument's folder: (name, bytes), by name.
pub fn list(path: &Path) -> io::Result<Vec<(String, u64)>> {
    let fs = files(path)?;
    let dir = match fs.root_dir().open_dir(FOLDER) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in dir.iter() {
        let entry = entry?;
        if entry.is_file() {
            out.push((entry.file_name().to_ascii_uppercase(), entry.len()));
        }
    }
    out.sort();
    Ok(out)
}

pub fn read(path: &Path, name: &str) -> io::Result<Vec<u8>> {
    let fs = files(path)?;
    let mut data = Vec::new();
    fs.root_dir().open_dir(FOLDER)?.open_file(name)?.read_to_end(&mut data)?;
    Ok(data)
}

/// Store a file in the instrument's folder, in place of one of that name.
pub fn write(path: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    let fs = files(path)?;
    {
        let root = fs.root_dir();
        let dir = match root.open_dir(FOLDER) {
            Ok(dir) => dir,
            Err(_) => root.create_dir(FOLDER)?,
        };
        let mut file = dir.create_file(name)?;
        file.truncate()?;
        file.write_all(data)?;
        file.flush()?;
    }
    fs.unmount()
}

pub fn remove(path: &Path, name: &str) -> io::Result<()> {
    let fs = files(path)?;
    fs.root_dir().open_dir(FOLDER)?.remove(name)?;
    fs.unmount()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(card: &mut Card, cmd: u8, arg: u32) -> u8 {
        let mut frame = vec![0x40 | cmd];
        frame.extend(arg.to_be_bytes());
        frame.push(0x95);
        for byte in frame {
            card.exchange(byte);
        }
        (0..8).map(|_| card.exchange(0xFF)).find(|&b| b != 0xFF).expect("no answer")
    }

    #[test]
    fn a_block_written_is_read_back() {
        let mut card = Card::new(Store::Memory(vec![0; 64 << 20]), false);
        assert_eq!(command(&mut card, 0, 0), 1);
        assert_eq!(command(&mut card, 55, 0), 1);
        assert_eq!(command(&mut card, 41, 0), 0);
        assert_eq!(command(&mut card, 24, 7 * 512), 0);
        card.exchange(0xFF);
        card.exchange(0xFE);
        let block: Vec<u8> = (0..512).map(|i| (i * 7) as u8).collect();
        for &byte in block.iter().chain(&[0, 0]) {
            card.exchange(byte);
        }
        assert_eq!(card.exchange(0xFF) & 0x1F, 0x05);
        assert_eq!(command(&mut card, 17, 7 * 512), 0);
        while card.exchange(0xFF) != 0xFE {}
        let back: Vec<u8> = (0..514).map(|_| card.exchange(0xFF)).collect();
        assert_eq!(back[..512], block[..]);
        assert_eq!(back[512..], crc16(&block).to_be_bytes());
        assert_eq!(card.written, 1);
    }

    #[test]
    fn files_on_a_new_image() {
        let path = std::env::temp_dir().join(format!("xwp1-card-test-{}.img", std::process::id()));
        let _ = std::fs::remove_file(&path);
        create(&path, 64 << 20).unwrap();
        assert!(create(&path, 64 << 20).is_err(), "an image that is there was made again");
        assert_eq!(list(&path).unwrap(), []);
        let data: Vec<u8> = (0..70_000).map(|i| (i * 31) as u8).collect();
        write(&path, "MYTONE.ZSY", &data).unwrap();
        write(&path, "A.ZPF", b"old and long").unwrap();
        write(&path, "A.ZPF", b"new").unwrap();
        assert_eq!(list(&path).unwrap(), [("A.ZPF".to_string(), 3), ("MYTONE.ZSY".to_string(), 70_000)]);
        assert_eq!(read(&path, "MYTONE.ZSY").unwrap(), data);
        assert_eq!(read(&path, "A.ZPF").unwrap(), b"new");
        remove(&path, "A.ZPF").unwrap();
        assert_eq!(list(&path).unwrap().len(), 1);
        // the card the firmware is given reads the same blocks
        let card = Card::new(Store::File(File::open(&path).unwrap()), true);
        assert_eq!(card.blocks, (64 << 20) / 512);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(file_name("my-tone.zsy").as_deref(), Some("MY-TONE.ZSY"));
        assert_eq!(file_name("toolongname.zsy"), None);
        assert_eq!(file_name("../x.zsy"), None);
    }

    #[test]
    fn checksums() {
        assert_eq!(crc7(&[0x40, 0, 0, 0, 0]), 0x95);
        assert_eq!(crc16(&[0xFF; 512]), 0x7FA1);
    }
}
