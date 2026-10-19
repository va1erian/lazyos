//! The U2a spike (docs/update-plan.md decision 6): a GRUB environment block on
//! the OS volume and the snippet that counts trial boots in it. Test mode
//! only (`--test-grubenv`); a normal install writes neither yet.

use crate::os_image;
use crate::populate::Extent;

/// The snippet; `@SYSTEM_FS_UUID@` is the system volume's ext2 UUID.
pub const TEMPLATE: &str = include_str!("../grubenv.in");
/// A GRUB environment block is exactly this long, inside one ext2 block.
pub const BLOCK: usize = 1024;
const HEADER: &str = "# GRUB Environment Block\n";

/// The env block holding `lazyos_boots=<value>`, `#`-padded as `grub-editenv`
/// writes it. `value` is any text without a newline (a corrupt one is a test).
pub fn block(value: &str) -> Result<Vec<u8>, String> {
    let body = format!("{HEADER}lazyos_boots={value}\n");
    if value.contains(['\n', '#']) || body.len() > BLOCK {
        return Err(format!("{value:?} does not fit a GRUB environment block"));
    }
    let mut bytes = body.into_bytes();
    bytes.resize(BLOCK, b'#');
    Ok(bytes)
}

/// The snippet for the system volume `uuid`.
pub fn snippet(uuid: [u8; 16]) -> String {
    TEMPLATE.replace("@SYSTEM_FS_UUID@", &os_image::format_uuid(uuid))
}

/// Create `/releases/grubenv` on `extent` with `lazyos_boots=value`.
pub fn create(extent: &Extent, value: &str) -> Result<(), String> {
    let bytes = block(value)?;
    let volume = os_image::open_cached(extent.io(true)?)?;
    let dir = fhs::boot::GRUBENV_PATH
        .rsplit_once('/')
        .map_or("/", |(dir, _)| dir);
    volume
        .mkdir_p(dir, 0o755, 0, 0)
        .map_err(|e| format!("{dir}: {e:?}"))?;
    volume
        .write_file(
            fhs::boot::GRUBENV_PATH,
            &bytes,
            0o644,
            0,
            0,
            os_image::now(),
        )
        .map_err(|e| format!("{}: {e:?}", fhs::boot::GRUBENV_PATH))?;
    volume
        .flush()
        .map_err(|e| os_image::volume_error("flush the system volume", e))
}

/// Overwrite the block of the existing file in place (a test makes it
/// corrupt, or sets the counter), never reallocating it.
pub fn overwrite(extent: &Extent, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() != BLOCK {
        return Err(format!("a GRUB environment block is {BLOCK} bytes"));
    }
    let volume = os_image::open_cached(extent.io(true)?)?;
    let at = volume.mapped_block(fhs::boot::GRUBENV_PATH, 0).ok();
    volume
        .write(fhs::boot::GRUBENV_PATH, 0, bytes)
        .map_err(|e| format!("{}: {e:?}", fhs::boot::GRUBENV_PATH))?;
    if volume.mapped_block(fhs::boot::GRUBENV_PATH, 0).ok() != at {
        return Err("the environment block moved".into());
    }
    volume
        .flush()
        .map_err(|e| os_image::volume_error("flush the system volume", e))
}

/// The file's bytes on the volume, read-only.
pub fn read(extent: &Extent) -> Result<Vec<u8>, String> {
    let volume = os_image::open_cached(extent.io(false)?)?;
    volume
        .read_file(fhs::boot::GRUBENV_PATH)
        .map_err(|e| format!("{}: {e:?}", fhs::boot::GRUBENV_PATH))
}
