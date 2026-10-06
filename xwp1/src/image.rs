//! Loads the 1.11 update image and returns flash in address order (the file
//! is not in flash order; see tools/xwimg.py for the layout and evidence).
use std::{fs, io, path::Path};

pub const FLASH_BASE: u32 = 0x1800_0000;
pub const FLASH_SIZE: usize = 0x0200_0000;
const HEADER: usize = 0x200;
const CODE_END: usize = 0x0C_0000;
const RW_SIZE: usize = 0x2_0000; // RW-init image: one sector, at flash 0xee0000
const USER_AREA: usize = 0x10_0000;

pub fn load(path: &Path) -> io::Result<Vec<u8>> {
    let data = fs::read(path)?;
    if data.len() != 2 * HEADER + FLASH_SIZE - USER_AREA || !data.starts_with(b"CASIO") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not the XW-P1 1.11 update image"));
    }
    let b = &data[HEADER..data.len() - HEADER];
    let mut flash = Vec::with_capacity(FLASH_SIZE);
    flash.extend_from_slice(&b[..CODE_END]);
    flash.extend_from_slice(&b[CODE_END + RW_SIZE..0xF0_0000]);
    flash.extend_from_slice(&b[CODE_END..CODE_END + RW_SIZE]);
    flash.resize(flash.len() + USER_AREA, 0xFF);
    flash.extend_from_slice(&b[0xF0_0000..]);
    Ok(flash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Temporary(PathBuf);
    use std::path::PathBuf;

    impl Temporary {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                "xwp1-image-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
    }

    #[test]
    fn rejects_wrong_size_and_header() {
        let path = Temporary::new();
        fs::write(&path.0, b"CASIO").unwrap();
        assert_eq!(load(&path.0).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut data = vec![0; 2 * HEADER + FLASH_SIZE - USER_AREA];
        fs::write(&path.0, &data).unwrap();
        assert_eq!(load(&path.0).unwrap_err().kind(), io::ErrorKind::InvalidData);
        data[..5].copy_from_slice(b"CASIO");
        fs::write(&path.0, &data).unwrap();
        assert_eq!(load(&path.0).unwrap().len(), FLASH_SIZE);
    }

    #[test]
    fn places_code_rw_user_area_and_remaining_flash_at_expected_offsets() {
        let path = Temporary::new();
        let mut data = vec![0; 2 * HEADER + FLASH_SIZE - USER_AREA];
        data[..5].copy_from_slice(b"CASIO");
        let end = data.len() - HEADER;
        let body = &mut data[HEADER..end];
        body[..CODE_END].fill(0x11);
        body[CODE_END..CODE_END + RW_SIZE].fill(0x22);
        body[CODE_END + RW_SIZE..0xF0_0000].fill(0x33);
        body[0xF0_0000..].fill(0x44);
        fs::write(&path.0, &data).unwrap();
        let flash = load(&path.0).unwrap();
        assert_eq!(flash.len(), FLASH_SIZE);
        for (at, value) in [
            (0, 0x11), (CODE_END - 1, 0x11),
            (CODE_END, 0x33), (0xEE_0000 - 1, 0x33),
            (0xEE_0000, 0x22), (0xF0_0000 - 1, 0x22),
            (0xF0_0000, 0xFF), (0x100_0000 - 1, 0xFF),
            (0x100_0000, 0x44), (FLASH_SIZE - 1, 0x44),
        ] {
            assert_eq!(flash[at], value, "flash offset {at:#x}");
        }
    }
}
