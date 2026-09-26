//! ATA PIO driver for the primary IDE channel (reads the QEMU disk image).

use x86_64::instructions::port::Port;

const DATA: u16 = 0x1F0;
const SECTORS: u16 = 0x1F2;
const LBA_LO: u16 = 0x1F3;
const LBA_MID: u16 = 0x1F4;
const LBA_HI: u16 = 0x1F5;
const DRIVE: u16 = 0x1F6;
const STATUS: u16 = 0x1F7;
const ALT_STATUS: u16 = 0x3F6;

/// 400ns delay: reading the alternate status port four times.
fn delay_400ns() {
    for _ in 0..4 {
        let _: u8 = unsafe { Port::<u8>::new(ALT_STATUS).read() };
    }
}

fn wait_not_busy() -> bool {
    for _ in 0..1_000_000 {
        let status: u8 = unsafe { Port::<u8>::new(STATUS).read() };
        if status & 0x80 == 0 {
            return true;
        }
    }
    false
}

fn wait_for_data() -> bool {
    for _ in 0..1_000_000 {
        let status: u8 = unsafe { Port::<u8>::new(STATUS).read() };
        if status & 0x08 != 0 {
            return true;
        }
        if status & 0x01 != 0 {
            return false; // error
        }
    }
    false
}

/// Read one 512-byte sector from the primary master drive (28-bit LBA).
pub fn read_sector(lba: u32, buf: &mut [u8; 512]) -> bool {
    // Safety: port I/O on the primary IDE channel.
    unsafe {
        Port::<u8>::new(DRIVE).write(0xE0 | ((lba >> 24) & 0x0F) as u8);
    }
    delay_400ns();
    // Safety: port I/O.
    unsafe {
        Port::<u8>::new(SECTORS).write(1);
        Port::<u8>::new(LBA_LO).write(lba as u8);
        Port::<u8>::new(LBA_MID).write((lba >> 8) as u8);
        Port::<u8>::new(LBA_HI).write((lba >> 16) as u8);
        Port::<u8>::new(STATUS).write(0x20); // READ SECTORS
    }

    if !wait_not_busy() || !wait_for_data() {
        return false;
    }

    let mut data = Port::<u16>::new(DATA);
    for i in 0..256 {
        // Safety: data port I/O within the sector.
        let word: u16 = unsafe { data.read() };
        buf[i * 2] = word as u8;
        buf[i * 2 + 1] = (word >> 8) as u8;
    }
    true
}
