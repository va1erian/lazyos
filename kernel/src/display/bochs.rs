//! Mode setting on the Bochs/QEMU "DISPI" adapter (QEMU's std VGA, PCI
//! `1234:1111`), so the kernel can leave the firmware's mode for a bigger one
//! (docs/hidpi-plan.md, D1).
//!
//! The BIOS stage of `bootloader` 0.11 never picks a mode above 1280x720, so a
//! 2560x1440 desktop is set here after boot: the adapter is reprogrammed
//! through its two index/data I/O ports, and the linear framebuffer is BAR0,
//! reached through the bootloader's physical-memory map (which covers the
//! first 4 GiB, where QEMU places it). Every request is checked against what
//! the adapter reports (its interface id, its maximum mode, its VRAM) and read
//! back after the switch, so an unexpected adapter keeps the firmware mode.

use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use x86_64::PhysAddr;

use crate::arch::io::{inw, outw};
use crate::dev::pci;

const VENDOR: u16 = 0x1234;
const DEVICE: u16 = 0x1111;

const INDEX_PORT: u16 = 0x01CE;
const DATA_PORT: u16 = 0x01CF;

const REG_ID: u16 = 0;
const REG_XRES: u16 = 1;
const REG_YRES: u16 = 2;
const REG_BPP: u16 = 3;
const REG_ENABLE: u16 = 4;
const REG_VIRT_WIDTH: u16 = 6;
const REG_VIRT_HEIGHT: u16 = 7;
const REG_X_OFFSET: u16 = 8;
const REG_Y_OFFSET: u16 = 9;
const REG_VIDEO_MEMORY_64K: u16 = 0x0A;

/// The DISPI interface id range (`0xB0C0` and up). A port pair that answers
/// with anything else is not this adapter; a zero VRAM count is refused by
/// [`check_mode`].
const ID_MIN: u16 = 0xB0C0;
const ID_MAX: u16 = 0xB0CF;

const ENABLED: u16 = 0x01;
const GET_CAPS: u16 = 0x02;
const LFB_ENABLED: u16 = 0x40;
/// Keep video memory on enable. Every enable here sets it: a refused mode
/// must leave the old framebuffer's pixels intact, and a kept one is
/// cleared by the new console anyway.
const NO_CLEAR_MEM: u16 = 0x80;

const BITS_PER_PIXEL: u16 = 32;
const BYTES_PER_PIXEL: usize = 4;

/// Why a mode was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModeError {
    /// No `1234:1111` function, or its ports do not answer with a DISPI id.
    NoAdapter,
    /// Larger than the adapter's advertised maximum.
    TooLarge { max: (u32, u32) },
    /// More bytes than the adapter's video memory.
    NoVram { needed: u64, vram: u64 },
    /// BAR0 is not a memory window below the physical map's 4 GiB.
    BadBar,
    /// The adapter did not keep the mode it was given.
    NotApplied,
}

impl core::fmt::Display for ModeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ModeError::NoAdapter => write!(f, "no Bochs DISPI adapter"),
            ModeError::TooLarge { max } => {
                write!(f, "above the adapter maximum {}x{}", max.0, max.1)
            }
            ModeError::NoVram { needed, vram } => write!(f, "needs {needed} bytes of {vram} VRAM"),
            ModeError::BadBar => write!(f, "framebuffer BAR unusable"),
            ModeError::NotApplied => write!(f, "the adapter did not keep the mode"),
        }
    }
}

/// A found adapter: its function, its framebuffer and its limits.
#[derive(Clone, Copy, Debug)]
pub struct Adapter {
    lfb: u64,
    vram: u64,
    max: (u32, u32),
}

/// A mode the adapter is now in, ready to hand to the console.
pub struct Mode {
    /// Kernel virtual address of the framebuffer.
    pub base: usize,
    pub info: FrameBufferInfo,
}

fn read(index: u16) -> u16 {
    // SAFETY: 0x1CE/0x1CF are the DISPI index/data pair; `find` has
    // verified an adapter answers there before any other access, and a
    // register read has no side effect.
    unsafe {
        outw(INDEX_PORT, index);
        inw(DATA_PORT)
    }
}

fn write(index: u16, value: u16) {
    // SAFETY: as in `read`; only the mode registers above are written, and
    // only by `set_mode`, which owns the console's framebuffer lock.
    unsafe {
        outw(INDEX_PORT, index);
        outw(DATA_PORT, value);
    }
}

/// The mode registers, saved before a switch so a mode the adapter does not
/// keep can be undone: the caller keeps the old framebuffer geometry, so the
/// scanout must go back to it too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Registers {
    xres: u16,
    yres: u16,
    bpp: u16,
    enable: u16,
    virt_width: u16,
    virt_height: u16,
    x_offset: u16,
    y_offset: u16,
}

impl Registers {
    /// Read the current mode registers (the enable flags without the
    /// write-only clear control).
    pub fn save() -> Registers {
        Registers {
            xres: read(REG_XRES),
            yres: read(REG_YRES),
            bpp: read(REG_BPP),
            enable: read(REG_ENABLE) & !NO_CLEAR_MEM,
            virt_width: read(REG_VIRT_WIDTH),
            virt_height: read(REG_VIRT_HEIGHT),
            x_offset: read(REG_X_OFFSET),
            y_offset: read(REG_Y_OFFSET),
        }
    }

    /// Put these registers back. The adapter is disabled while the geometry
    /// changes, as for any mode switch, then re-enabled as it was without
    /// clearing video memory. Enabling resets the virtual size to the
    /// resolution, so the saved stride and offsets are written after it.
    fn restore(self) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            write(REG_ENABLE, 0);
            write(REG_XRES, self.xres);
            write(REG_YRES, self.yres);
            write(REG_BPP, self.bpp);
            let keep = if self.enable & ENABLED != 0 {
                NO_CLEAR_MEM
            } else {
                0
            };
            write(REG_ENABLE, self.enable | keep);
            write(REG_VIRT_WIDTH, self.virt_width);
            write(REG_VIRT_HEIGHT, self.virt_height);
            write(REG_X_OFFSET, self.x_offset);
            write(REG_Y_OFFSET, self.y_offset);
        });
    }
}

/// Program `width x height` at 32 bpp with the linear framebuffer on.
fn program(width: u32, height: u32) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        write(REG_ENABLE, 0);
        write(REG_XRES, width as u16);
        write(REG_YRES, height as u16);
        write(REG_BPP, BITS_PER_PIXEL);
        write(REG_ENABLE, ENABLED | LFB_ENABLED | NO_CLEAR_MEM);
        write(REG_X_OFFSET, 0);
        write(REG_Y_OFFSET, 0);
    });
}

/// Bytes a `width x height` mode at 32 bpp needs.
pub fn mode_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * BYTES_PER_PIXEL as u64
}

/// Whether `width x height` fits an adapter with `max` and `vram` (pure, for
/// the tests).
pub fn check_mode(width: u32, height: u32, max: (u32, u32), vram: u64) -> Result<(), ModeError> {
    if width > max.0 || height > max.1 {
        return Err(ModeError::TooLarge { max });
    }
    let needed = mode_bytes(width, height);
    if needed > vram {
        return Err(ModeError::NoVram { needed, vram });
    }
    Ok(())
}

/// Find the adapter and read its limits. Reading the maximum mode needs the
/// `GET_CAPS` bit, which is restored straight after.
pub fn find() -> Result<Adapter, ModeError> {
    let function = pci::find_any(VENDOR, &[DEVICE]).ok_or(ModeError::NoAdapter)?;
    let id = read(REG_ID);
    if !(ID_MIN..=ID_MAX).contains(&id) {
        return Err(ModeError::NoAdapter);
    }
    let raw = pci::bar_raw(function.address, 0);
    // A memory BAR (bit 0 clear), 32-bit or 64-bit below 4 GiB.
    if raw & 1 != 0 || raw & !0xF == 0 {
        return Err(ModeError::BadBar);
    }
    let vram = u64::from(read(REG_VIDEO_MEMORY_64K)) * 64 * 1024;
    let max = x86_64::instructions::interrupts::without_interrupts(|| {
        let enable = read(REG_ENABLE);
        write(REG_ENABLE, enable | GET_CAPS);
        let max = (u32::from(read(REG_XRES)), u32::from(read(REG_YRES)));
        write(REG_ENABLE, enable);
        max
    });
    Ok(Adapter {
        lfb: u64::from(raw & !0xF),
        vram,
        max,
    })
}

impl Adapter {
    /// The adapter's maximum mode and video memory (for the tests).
    #[cfg(lazyos_tests)]
    pub fn limits(&self) -> ((u32, u32), u64) {
        (self.max, self.vram)
    }

    /// Switch to `width x height` at 32 bpp and return the new framebuffer.
    /// The caller must hold the framebuffer: nothing may draw on the old
    /// geometry while the registers change. A mode the adapter does not keep
    /// exactly (QEMU rounds the width down to a multiple of 8) is undone, old
    /// pixels included, and refused.
    pub fn set_mode(&self, width: u32, height: u32) -> Result<Mode, ModeError> {
        check_mode(width, height, self.max, self.vram)?;
        let end = self.lfb + mode_bytes(width, height);
        if end > 1 << 32 {
            return Err(ModeError::BadBar);
        }
        let saved = Registers::save();
        program(width, height);
        let applied = (u32::from(read(REG_XRES)), u32::from(read(REG_YRES)));
        let virt_width = u32::from(read(REG_VIRT_WIDTH));
        if applied != (width, height) || read(REG_BPP) != BITS_PER_PIXEL || virt_width < width {
            // Half applied: the caller keeps the old geometry, so the
            // scanout goes back to it too.
            saved.restore();
            return Err(ModeError::NotApplied);
        }
        let _ = read(REG_VIRT_HEIGHT);
        let base = crate::mem::phys_to_virt(PhysAddr::new(self.lfb)).as_u64() as usize;
        Ok(Mode {
            base,
            info: FrameBufferInfo {
                byte_len: mode_bytes(virt_width, height) as usize,
                width: width as usize,
                height: height as usize,
                pixel_format: PixelFormat::Bgr,
                bytes_per_pixel: BYTES_PER_PIXEL,
                stride: virt_width as usize,
            },
        })
    }
}
