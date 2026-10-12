//! Test disks for `tools/install/run.py` (docs/grub-install-plan.md I3),
//! with no Linux disk tools and no root:
//!
//! ```text
//! testdisk disk OUT.img [--esp MIB] [--system MIB] [--home MIB]
//! testdisk esp-put DISK.img SRC DST       # a host file onto the image's ESP
//! testdisk esp-get DISK.img PATH OUT      # a file of the image's ESP to the host
//! testdisk unpack-stick STICK.img DIR     # the stick's FAT partition into DIR
//! testdisk check DISK.img                 # both LazyOS volumes: checker, contents
//! ```

use std::path::Path;

use lazyos_install::testdisk;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&args) {
        eprintln!("testdisk: {error}");
        std::process::exit(1);
    }
}

fn mib(args: &[String], flag: &str, default: u64) -> Result<u64, String> {
    match args.iter().position(|a| a == flag) {
        Some(at) => args
            .get(at + 1)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("{flag} needs a size in MiB")),
        None => Ok(default),
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("disk") => {
            let out = args.get(1).ok_or("disk OUT.img")?;
            let (parts, disk) = testdisk::linux_box(
                mib(args, "--esp", 100)?,
                16,
                mib(args, "--system", 2048)?,
                mib(args, "--home", 256)?,
            );
            testdisk::write(Path::new(out), &parts, disk)?;
            println!("{out}: {disk} MiB, GPT {parts:?}");
            Ok(())
        }
        Some("esp-put") => {
            let [_, disk, src, dst] = args else {
                return Err("esp-put DISK.img SRC DST".into());
            };
            let bytes = std::fs::read(src).map_err(|e| format!("{src}: {e}"))?;
            testdisk::esp_put(Path::new(disk), dst, &bytes)
        }
        Some("esp-get") => {
            let [_, disk, path, out] = args else {
                return Err("esp-get DISK.img PATH OUT".into());
            };
            let bytes = testdisk::esp_get(Path::new(disk), path)?
                .ok_or_else(|| format!("no {path} on the ESP"))?;
            std::fs::write(out, bytes).map_err(|e| format!("{out}: {e}"))
        }
        Some("check") => {
            let [_, disk] = args else {
                return Err("check DISK.img".into());
            };
            let report = testdisk::check(Path::new(disk))?;
            println!("{report}");
            Ok(())
        }
        Some("unpack-stick") => {
            let [_, image, dir] = args else {
                return Err("unpack-stick STICK.img DIR".into());
            };
            let count = testdisk::unpack_stick(Path::new(image), Path::new(dir))?;
            println!("{count} files into {dir}");
            Ok(())
        }
        _ => Err("usage: testdisk disk|esp-put|esp-get|unpack-stick|check ...".into()),
    }
}
