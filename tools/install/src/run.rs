//! The commands: `install` (steps 1 to 9, nothing written before step 5),
//! `update` (step 5 in place, then 6 and 8), `uninstall` and `status`.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::args::{Command, Options};
use crate::payload::{self, Payload};
use crate::plan::{self, Content, Context, ESP_DIR};
use crate::target::{self, Found, GrubSite};
use crate::{format_uuid, grub, populate, verify};

/// Run `options.command`.
pub fn run(options: &Options) -> Result<(), String> {
    let stick = stick_dir(options);
    let mut found = match &options.image {
        Some(image) => target::in_image(image, options)?,
        None => target::on_linux(options, stick.as_deref().unwrap_or(Path::new("/")))?,
    };
    for fact in &found.facts {
        println!("{fact}");
    }
    match options.command {
        Command::Install | Command::Update => {
            let stick =
                stick.ok_or("no stick: pass --stick DIR (the mounted stick's FAT partition)")?;
            let payload = Payload::open(&stick)?;
            println!(
                "payload: {} (OS volume {} MiB)",
                stick.display(),
                payload.os_bytes() >> 20
            );
            write(options, &mut found, &payload)
        }
        Command::Uninstall => uninstall(&mut found),
        Command::Status => status(&mut found, stick.as_deref()),
    }
}

/// `--stick`, else the directory above this program when it runs from the
/// stick's `install/`.
fn stick_dir(options: &Options) -> Option<PathBuf> {
    if let Some(dir) = &options.stick {
        return Some(dir.clone());
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let named = dir.file_name()?.to_str()?.eq_ignore_ascii_case("install");
    named.then(|| dir.parent().map(Path::to_path_buf)).flatten()
}

fn on_path(program: &str) -> bool {
    #[cfg(unix)]
    return crate::linux::on_path(program);
    #[cfg(not(unix))]
    return {
        let _ = program;
        false
    };
}

fn ext2_uuid(content: &Content) -> Option<[u8; 16]> {
    match content {
        Content::Ext2 { uuid, .. } => Some(*uuid),
        _ => None,
    }
}

/// Bytes the ESP files need beyond the ones they replace, plus room for the
/// largest one's temporary copy.
fn esp_need(found: &mut Found, files: &[(&str, Vec<u8>)]) -> Result<u64, String> {
    let mut need = 0u64;
    let mut largest = 0u64;
    for (name, bytes) in files {
        let old = found
            .esp_access
            .read(&format!("{ESP_DIR}/{name}"))?
            .map_or(0, |b| b.len() as u64);
        need += (bytes.len() as u64).saturating_sub(old);
        largest = largest.max(bytes.len() as u64);
    }
    Ok(need + largest)
}

/// `install` and `update`.
fn write(options: &Options, found: &mut Found, payload: &Payload) -> Result<(), String> {
    let installing = options.command == Command::Install;
    let source = payload.os_volume()?;
    let (dirs, files) = populate::tree(&source)?;
    let keep_home = installing
        && !options.format_home
        && matches!(&found.home.content, Content::Ext2 { label, system: false, .. } if label == crate::usb_stick::HOME_LABEL);
    let system_uuid = match installing {
        true => crate::os_image::new_uuid(),
        false => ext2_uuid(&found.system.content).unwrap_or_default(),
    };
    let home_uuid = match installing && !keep_home {
        true => crate::os_image::new_uuid(),
        false => ext2_uuid(&found.home.content).unwrap_or_default(),
    };
    let cfg = plan::lazyos_cfg(&payload.cfg, system_uuid, home_uuid);
    let esp_files = payload.esp_files(payload::config_ramdisk(&cfg)?);
    let ctx = Context {
        running: found.running.clone(),
        stick_disk: found.stick_disk.clone(),
        payload_os_bytes: payload.os_bytes(),
        esp_free: found.esp_access.free_bytes()?,
        esp_need: esp_need(found, &esp_files)?,
    };
    let warnings = plan::validate(
        options.command,
        &found.system,
        &found.home,
        &found.esp,
        &ctx,
        options.format_home,
    )?;
    print_plan(
        options,
        found,
        keep_home,
        system_uuid,
        home_uuid,
        &cfg,
        dirs.len(),
        files.len(),
    );
    for warning in &warnings {
        println!("warning: {warning}");
    }
    if options.dry_run {
        println!("--dry-run: nothing written");
        return Ok(());
    }
    if installing && !options.yes {
        confirm(&found.system.device)?;
    }
    // Step 5.
    if installing {
        println!("formatting and populating {} ...", found.system.device);
        populate::install_system(&found.system_extent, system_uuid, &dirs, &files)?;
        if !keep_home {
            println!("formatting {} ...", found.home.device);
            let accounts = populate::accounts(&source);
            populate::install_home(
                &found.home_extent,
                home_uuid,
                &accounts,
                options.home_from.as_deref(),
            )?;
        }
    } else {
        println!("updating {} in place ...", found.system.device);
        let uuid = populate::update_system(&found.system_extent, &dirs, &files)?;
        if uuid != system_uuid {
            return Err("the system volume's UUID changed under the update".into());
        }
    }
    // Step 6, the kernel last.
    for (name, bytes) in &esp_files {
        found.esp_access.write_atomic(ESP_DIR, name, bytes)?;
    }
    println!("wrote {} files to {ESP_DIR} on the ESP", esp_files.len());
    // Step 7 (an update keeps the entry it has).
    if installing {
        println!("{}", register(found)?);
    } else if !registered(found)? {
        println!("warning: GRUB has no LazyOS entry; run install again or add it by hand");
    }
    // Step 8.
    println!(
        "system: {}",
        verify::check_volume(&found.system_extent, options.check_limit)?
    );
    println!(
        "home: {}",
        verify::check_volume(&found.home_extent, options.check_limit)?
    );
    verify::check_esp(found.esp_access.as_mut(), &esp_files)?;
    println!("ESP files verified (SHA-256)");
    // Step 9.
    match (&found.grub, options.trial_boot) {
        (GrubSite::Linux, true) => println!("{}", grub::trial_boot(&on_path)?),
        (GrubSite::Linux, false) => {
            println!("to try it once: sudo grub-reboot LazyOS && sudo reboot")
        }
        (GrubSite::Image { .. }, _) => {}
    }
    println!("done");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_plan(
    options: &Options,
    found: &Found,
    keep_home: bool,
    system_uuid: [u8; 16],
    home_uuid: [u8; 16],
    cfg: &str,
    dirs: usize,
    files: usize,
) {
    let part = |p: &plan::Part| {
        format!(
            "{} (partition {}, {:?}, {} MiB, now {})",
            p.device,
            p.index,
            p.name,
            p.bytes() >> 20,
            plan::describe(&p.content)
        )
    };
    println!("\nplan:");
    match options.command {
        Command::Install => {
            println!(
                "  system: FORMAT {} as ext2 {}",
                part(&found.system),
                format_uuid(system_uuid)
            );
            println!("          and write {files} files, {dirs} directories");
            match keep_home {
                true => println!("  home:   keep {}", part(&found.home)),
                false => println!(
                    "  home:   FORMAT {} as ext2 {}",
                    part(&found.home),
                    format_uuid(home_uuid)
                ),
            }
        }
        _ => println!(
            "  system: update {} in place from its manifest",
            part(&found.system)
        ),
    }
    println!(
        "  ESP:    {} gets {ESP_DIR}/ (serial {})",
        part(&found.esp),
        found.esp_serial
    );
    println!("  lazyos.cfg:");
    for line in cfg.lines() {
        println!("    {line}");
    }
    if let Ok(entry) = plan::menu_entry(&found.esp_serial) {
        println!("  GRUB entry:");
        for line in entry.lines() {
            println!("    {line}");
        }
    }
    println!();
}

/// Step 4: the person types the system partition's name back.
fn confirm(device: &str) -> Result<(), String> {
    print!("Everything on {device} will be erased. Type {device} to go on: ");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.trim() != device {
        return Err("not confirmed; nothing was written".into());
    }
    Ok(())
}

fn register(found: &mut Found) -> Result<String, String> {
    match &found.grub {
        GrubSite::Linux => grub::Linux { root: "/".into() }.register(&found.esp_serial, &on_path),
        GrubSite::Image { dir } => {
            let esp = found
                .image_esp
                .as_mut()
                .ok_or("test mode without an image ESP")?;
            grub::register_image(esp, dir, &found.esp_serial)
        }
    }
}

fn registered(found: &mut Found) -> Result<bool, String> {
    Ok(match &found.grub {
        GrubSite::Linux => grub::Linux { root: "/".into() }.registered(),
        GrubSite::Image { dir } => {
            let esp = found
                .image_esp
                .as_mut()
                .ok_or("test mode without an image ESP")?;
            let text =
                crate::esp::Esp::read(esp, &format!("{dir}/custom.cfg"))?.unwrap_or_default();
            String::from_utf8_lossy(&text).contains(plan::CHAINLOAD)
        }
    })
}

fn uninstall(found: &mut Found) -> Result<(), String> {
    let removed = found.esp_access.remove_dir(ESP_DIR)?;
    println!(
        "{}",
        if removed {
            format!("removed {ESP_DIR} from the ESP")
        } else {
            format!("no {ESP_DIR} on the ESP")
        }
    );
    let done = match &found.grub {
        GrubSite::Linux => grub::Linux { root: "/".into() }.unregister(&on_path)?,
        GrubSite::Image { dir } => {
            let esp = found
                .image_esp
                .as_mut()
                .ok_or("test mode without an image ESP")?;
            grub::unregister_image(esp, dir)?
        }
    };
    println!("{done}");
    println!(
        "the partitions {} and {} are left as they are",
        found.system.device, found.home.device
    );
    Ok(())
}

fn status(found: &mut Found, stick: Option<&Path>) -> Result<(), String> {
    for (role, part) in [
        ("system", &found.system),
        ("home", &found.home),
        ("ESP", &found.esp),
    ] {
        println!(
            "{role}: {} ({} MiB): {}",
            part.device,
            part.bytes() >> 20,
            plan::describe(&part.content)
        );
    }
    let payload = stick.and_then(|dir| Payload::open(dir).ok());
    for name in plan::ESP_FILES {
        let path = format!("{ESP_DIR}/{name}");
        let state = match (found.esp_access.read(&path)?, &payload) {
            (None, _) => "missing".to_string(),
            (Some(bytes), Some(payload)) if name == "kernel-x86_64" => {
                if bytes == payload.kernel {
                    "same as the stick".into()
                } else {
                    "differs from the stick".into()
                }
            }
            (Some(bytes), _) => format!("{} bytes", bytes.len()),
        };
        println!("{path}: {state}");
    }
    println!(
        "GRUB entry: {}",
        if registered(found)? {
            "present"
        } else {
            "absent"
        }
    );
    Ok(())
}
