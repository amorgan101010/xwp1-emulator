//! MX29GL128E NOR flash (AMD command set), enough for the firmware's own
//! program and sector-erase routines. Flash stays ordinary memory; a write
//! hook follows the unlock sequence. Command-cycle writes are undone,
//! program and write-buffer cycles leave old AND new (a cell only goes
//! from 1 to 0), and a sector erase fills the sector with 0xff. The caller
//! applies all of that before the firmware next reads the cell: its status
//! poll reads the address it wrote last, at once, and takes anything but
//! the data for a failed write (docs/FINDINGS.md).
pub const SECTOR: u32 = 0x2_0000;

#[derive(Default)]
pub struct Flash {
    state: u8, // position in the unlock sequence
    erase: bool,
    left: u32,
    pub undo: Vec<(u32, Vec<u8>)>, // what belongs in the cells written: the old content, or old AND data
    pub fill: Vec<u32>,            // sectors to erase
    pub programmed: u64,
    pub erased: Vec<u32>,
}

impl Flash {
    /// One bus write; `old` is the memory content before it lands. The
    /// caller applies `undo` and `fill` (the hook runs before the write).
    pub fn write(&mut self, addr: u32, value: u32, old: Vec<u8>) {
        let off = addr & 0xFFF;
        let programmed = || old.iter().zip(value.to_le_bytes()).map(|(o, n)| o & n).collect::<Vec<u8>>();
        let value = value & 0xFF;
        match self.state {
            3 => {
                // program data cycle
                self.undo.push((addr, programmed()));
                self.state = 0;
                self.programmed += 1;
                return;
            }
            4 => {
                // write buffer: word count - 1
                self.undo.push((addr, old));
                self.left = value + 1;
                self.state = 5;
                return;
            }
            5 => {
                // write buffer: data words
                self.undo.push((addr, programmed()));
                self.programmed += 1;
                self.left -= 1;
                if self.left == 0 {
                    self.state = 6;
                }
                return;
            }
            6 => {
                // write buffer: confirm (0x29)
                self.undo.push((addr, old));
                self.state = 0;
                return;
            }
            _ => {}
        }
        self.undo.push((addr, old));
        if value == 0xF0 {
            self.state = 0;
            self.erase = false;
        } else if self.state == 0 && off == 0xAAA && value == 0xAA {
            self.state = 1;
        } else if self.state == 1 && off == 0x554 && value == 0x55 {
            self.state = 2;
        } else if self.state == 2 {
            self.state = 0;
            if self.erase && value == 0x30 {
                self.fill.push(addr & !(SECTOR - 1));
                self.erase = false;
            } else if off == 0xAAA && value == 0xA0 {
                self.state = 3;
            } else if off == 0xAAA && value == 0x80 {
                self.erase = true;
            } else if value == 0x25 {
                self.state = 4;
            }
        } else {
            self.state = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(flash: &mut Flash, addr: u32, value: u32, old: &[u8]) {
        flash.write(addr, value, old.to_vec());
    }

    fn unlock(flash: &mut Flash) {
        write(flash, 0x1800_0AAA, 0xAA, &[0xFF]);
        write(flash, 0x1800_0554, 0x55, &[0xFF]);
    }

    #[test]
    fn program_only_clears_bits_and_preserves_other_bytes() {
        let mut flash = Flash::default();
        unlock(&mut flash);
        write(&mut flash, 0x1800_0AAA, 0xA0, &[0xFF]);
        write(&mut flash, 0x1801_2340, 0x12_34_F0_0F, &[0x55, 0x0F, 0xFF, 0x80]);
        assert_eq!(flash.undo.last().unwrap(), &(0x1801_2340, vec![0x05, 0x00, 0x34, 0x00]));
        assert_eq!(flash.programmed, 1);
        assert!(flash.fill.is_empty());
        // The data cycle ended; a subsequent write without unlocking is a command write.
        write(&mut flash, 0x1801_2340, 0x00, &[0xAA]);
        assert_eq!(flash.undo.last().unwrap().1, vec![0xAA]);
        assert_eq!(flash.programmed, 1);
    }

    #[test]
    fn erase_requires_both_unlocks_and_uses_the_sector_base() {
        let mut flash = Flash::default();
        unlock(&mut flash);
        write(&mut flash, 0x1800_0AAA, 0x80, &[0xFF]);
        write(&mut flash, 0x1803_4567, 0x30, &[0x00]);
        assert!(flash.fill.is_empty());
        unlock(&mut flash);
        write(&mut flash, 0x1803_4567, 0x30, &[0x00]);
        assert_eq!(flash.fill, vec![0x1802_0000]);
        assert_eq!(flash.erased.len(), 0); // the caller records completion
        assert!(flash.undo.iter().all(|(_, old)| old == &[0xFF] || old == &[0x00]));
    }

    #[test]
    fn buffered_program_counts_words_and_confirmation_is_not_data() {
        let mut flash = Flash::default();
        unlock(&mut flash);
        write(&mut flash, 0x1802_0000, 0x25, &[0xFF]);
        write(&mut flash, 0x1802_0000, 1, &[0xFF]); // two words
        write(&mut flash, 0x1802_0004, 0x0F, &[0xF0]);
        write(&mut flash, 0x1802_0008, 0xF0, &[0x0F]);
        write(&mut flash, 0x1802_0000, 0x29, &[0xFF]);
        assert_eq!(flash.programmed, 2);
        assert_eq!(flash.undo[4].1, vec![0]);
        assert_eq!(flash.undo[5].1, vec![0]);
        assert_eq!(flash.undo[6].1, vec![0xFF]);
    }

    #[test]
    fn reset_cancels_an_incomplete_command() {
        let mut flash = Flash::default();
        write(&mut flash, 0x1800_0AAA, 0xAA, &[0xFF]);
        write(&mut flash, 0x1800_0554, 0xF0, &[0xFF]);
        write(&mut flash, 0x1800_0AAA, 0xA0, &[0xFF]);
        write(&mut flash, 0x1800_1234, 0, &[0xFF]);
        assert_eq!(flash.programmed, 0);
        assert!(flash.fill.is_empty());
    }
}
