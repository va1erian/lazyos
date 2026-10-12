//! Step 8: the independent ext2 checker on both volumes, and the ESP files
//! read back and compared by SHA-256.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::esp::Esp;
use crate::plan::ESP_DIR;
use crate::populate::Extent;

/// Run `libs/ext2fs`'s checker over `extent` when it is at most `limit`
/// bytes (it reads the volume into memory); a larger one goes to `e2fsck
/// -fn` when that is installed. What was done, or the problems found.
pub fn check_volume(extent: &Extent, limit: u64) -> Result<String, String> {
    let name = extent.path.display();
    if extent.bytes() <= limit {
        let mut image = vec![0u8; extent.bytes() as usize];
        let mut file = File::open(&extent.path).map_err(|e| format!("{name}: {e}"))?;
        file.seek(SeekFrom::Start(extent.start_lba * 512))
            .and_then(|_| file.read_exact(&mut image))
            .map_err(|e| format!("read {name}: {e}"))?;
        let problems = ext2fs::check::fsck(&image);
        if !problems.is_empty() {
            return Err(format!(
                "the checker found problems on {name}:\n  {}",
                problems.join("\n  ")
            ));
        }
        return Ok("clean (ext2 checker)".into());
    }
    if extent.start_lba == 0 && e2fsck_present() {
        let status = std::process::Command::new("e2fsck")
            .args(["-fn"])
            .arg(&extent.path)
            .status()
            .map_err(|e| format!("e2fsck: {e}"))?;
        return match status.success() {
            true => Ok("clean (e2fsck -fn)".into()),
            false => Err(format!("e2fsck -fn {name} reported problems ({status})")),
        };
    }
    Ok(format!(
        "not checked: larger than --check-limit ({} MiB) and no e2fsck",
        limit >> 20
    ))
}

fn e2fsck_present() -> bool {
    #[cfg(unix)]
    return crate::linux::on_path("e2fsck");
    #[cfg(not(unix))]
    return false;
}

/// Read every ESP file back and compare it with what was written.
pub fn check_esp(esp: &mut dyn Esp, files: &[(&str, Vec<u8>)]) -> Result<(), String> {
    for (name, bytes) in files {
        let path = format!("{ESP_DIR}/{name}");
        let back = esp
            .read(&path)?
            .ok_or_else(|| format!("{path} is missing on the ESP"))?;
        if lazyos_crypto::sha256::sha256(&back) != lazyos_crypto::sha256::sha256(bytes) {
            return Err(format!("{path} on the ESP differs from what was written"));
        }
    }
    Ok(())
}
