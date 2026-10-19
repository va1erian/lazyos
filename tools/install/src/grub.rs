//! Registering with GRUB (step 7): `/etc/grub.d/40_lazyos` and the distro's
//! regenerate command, read back from the generated `grub.cfg`; or, with no
//! `/etc/grub.d`, the entry between markers in `custom.cfg` beside
//! `grub.cfg` (upstream's `41_custom` sources it). Test mode writes that
//! `custom.cfg` on the image's ESP.

use std::path::{Path, PathBuf};

use crate::esp::{Esp, ImageEsp};
use crate::plan::{self, CHAINLOAD};

/// The snippet's place.
pub const SNIPPET: &str = "etc/grub.d/40_lazyos";
/// Where distros keep `grub.cfg`, relative to `/`.
const GRUB_CFGS: [&str; 2] = ["boot/grub/grub.cfg", "boot/grub2/grub.cfg"];

/// The running system's GRUB, under `root` (`/`; a temp dir in tests).
pub struct Linux {
    pub root: PathBuf,
}

impl Linux {
    /// The generated `grub.cfg`.
    pub fn grub_cfg(&self) -> Result<PathBuf, String> {
        GRUB_CFGS
            .iter()
            .map(|cfg| self.root.join(cfg))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                "no /boot/grub/grub.cfg or /boot/grub2/grub.cfg: is GRUB installed?".into()
            })
    }

    fn uses_grub_d(&self) -> bool {
        self.root.join("etc/grub.d").is_dir()
    }

    /// Put the entry for the ESP serial `serial` in; what was done.
    pub fn register(&self, serial: &str, on_path: &dyn Fn(&str) -> bool) -> Result<String, String> {
        let cfg = self.grub_cfg()?;
        if self.uses_grub_d() {
            let snippet = self.root.join(SNIPPET);
            write_script(&snippet, &plan::grub_script(serial)?)?;
            self.regenerate(&cfg, on_path)?;
            let text =
                std::fs::read_to_string(&cfg).map_err(|e| format!("{}: {e}", cfg.display()))?;
            if !text.contains(CHAINLOAD) {
                return Err(format!(
                    "{} has no LazyOS entry after regenerating; is {} executable and run by your distro?",
                    cfg.display(),
                    snippet.display()
                ));
            }
            return Ok(format!(
                "wrote {} and regenerated {}",
                snippet.display(),
                cfg.display()
            ));
        }
        let custom = cfg.with_file_name("custom.cfg");
        let old = std::fs::read_to_string(&custom).unwrap_or_default();
        write_text(&custom, &plan::with_block(&old, &plan::menu_entry(serial)?))?;
        let sourced = std::fs::read_to_string(&cfg)
            .unwrap_or_default()
            .contains("custom.cfg");
        let mut done = format!("no /etc/grub.d: added the entry to {}", custom.display());
        if !sourced {
            done += &format!(
                "\nwarning: {} does not mention custom.cfg; add this entry by hand:\n{}",
                cfg.display(),
                plan::menu_entry(serial)?
            );
        }
        Ok(done)
    }

    /// Take the entry out again; what was done.
    pub fn unregister(&self, on_path: &dyn Fn(&str) -> bool) -> Result<String, String> {
        let mut done = Vec::new();
        let snippet = self.root.join(SNIPPET);
        if snippet.exists() {
            std::fs::remove_file(&snippet).map_err(|e| format!("{}: {e}", snippet.display()))?;
            let cfg = self.grub_cfg()?;
            self.regenerate(&cfg, on_path)?;
            done.push(format!(
                "removed {} and regenerated {}",
                snippet.display(),
                cfg.display()
            ));
        }
        if let Ok(cfg) = self.grub_cfg() {
            let custom = cfg.with_file_name("custom.cfg");
            if let Ok(text) = std::fs::read_to_string(&custom) {
                let (rest, found) = plan::without_block(&text);
                if found {
                    write_text(&custom, &rest)?;
                    done.push(format!("removed the entry from {}", custom.display()));
                }
            }
        }
        Ok(if done.is_empty() {
            "no GRUB entry to remove".into()
        } else {
            done.join("\n")
        })
    }

    /// Whether `grub.cfg` (or `custom.cfg`) carries the entry.
    pub fn registered(&self) -> bool {
        self.grub_cfg().is_ok_and(|cfg| {
            let text = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
            text(&cfg).contains(CHAINLOAD)
                || text(&cfg.with_file_name("custom.cfg")).contains(CHAINLOAD)
        })
    }

    fn regenerate(&self, cfg: &Path, on_path: &dyn Fn(&str) -> bool) -> Result<(), String> {
        let command = plan::regenerate_command(on_path, &cfg.to_string_lossy())
            .ok_or("neither update-grub nor grub-mkconfig/grub2-mkconfig is on PATH; regenerate grub.cfg by hand")?;
        let status = std::process::Command::new(&command[0])
            .args(&command[1..])
            .status()
            .map_err(|e| format!("{}: {e}", command[0]))?;
        if !status.success() {
            return Err(format!("{} failed ({status})", command.join(" ")));
        }
        Ok(())
    }
}

/// Test mode: the entry in `<dir>/custom.cfg` on the image's ESP.
///
/// `counting` is the boot-counting snippet (U2a), put before the entry.
pub fn register_image(
    esp: &mut ImageEsp,
    dir: &str,
    serial: &str,
    counting: Option<&str>,
) -> Result<String, String> {
    let path = format!("{dir}/custom.cfg");
    let old = esp
        .read(&path)?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let entry = format!(
        "{}{}",
        counting.unwrap_or_default(),
        plan::menu_entry(serial)?
    );
    esp.write_file(&path, plan::with_block(&old, &entry).as_bytes())?;
    Ok(format!("added the entry to {path} on the image's ESP"))
}

pub fn unregister_image(esp: &mut ImageEsp, dir: &str) -> Result<String, String> {
    let path = format!("{dir}/custom.cfg");
    let Some(old) = esp.read(&path)? else {
        return Ok("no GRUB entry to remove".into());
    };
    let (rest, found) = plan::without_block(&String::from_utf8_lossy(&old));
    if !found {
        return Ok("no GRUB entry to remove".into());
    }
    esp.write_file(&path, rest.as_bytes())?;
    Ok(format!("removed the entry from {path} on the image's ESP"))
}

/// Step 9: boot LazyOS once at the next restart.
pub fn trial_boot(on_path: &dyn Fn(&str) -> bool) -> Result<String, String> {
    let tool = plan::reboot_command(on_path).ok_or("grub-reboot is not on PATH")?;
    let status = std::process::Command::new(tool)
        .arg("LazyOS")
        .status()
        .map_err(|e| format!("{tool}: {e}"))?;
    if !status.success() {
        return Err(format!("{tool} LazyOS failed ({status})"));
    }
    Ok(
        "the next boot starts LazyOS once; the one after is Linux again. \
        (A hidden GRUB menu shows with Shift or Esc at power-on.)"
            .into(),
    )
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    let temp = path.with_extension("lazyos-new");
    std::fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write an executable script (mode 0755).
fn write_script(path: &Path, text: &str) -> Result<(), String> {
    write_text(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(())
}
