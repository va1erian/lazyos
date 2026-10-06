//! `devctl` (`/system/bin/devctl`): what the device layer looks like right now
//! (issue #481). Read-only on purpose: the driver class rules are compiled
//! into the kernel and installed at boot, so there is nothing to edit here.
//!
//! ```text
//! devctl [devices]   every device: class, PCI ids, owner uid and its rights
//! devctl rules       the driver class rules the kernel enforces
//! devctl denials     refused claims still in the audit ring (CAP_AUDIT_READ)
//! devctl drivers     what devd matched each device to and its state (issue #497)
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;

use devinspect::{class_name, method_name, reason_name, Uid};
use user::dev::{errno, inspect};
use user::sys;

const USAGE: &str = "usage: devctl [devices|rules|denials|drivers]";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut buffer = [0u8; 64];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let status = match run(text.split_whitespace().next().unwrap_or("devices")) {
        Ok(()) => 0,
        Err(message) => {
            sys::write_str(&message);
            sys::write_str("\n");
            1
        }
    };
    sys::exit(status)
}

/// A refusal in words a shell user can act on.
fn explain(errno: i64) -> String {
    match errno {
        errno::EPERM => String::from(
            "devctl: denials needs CAP_AUDIT_READ;\n\
             this session runs without capabilities",
        ),
        errno::EACCES => {
            String::from("devctl: the Messenger policy refuses os.kernel.dev to this task")
        }
        other => format!("devctl: device syscall failed (errno {other})"),
    }
}

fn run(command: &str) -> Result<(), String> {
    match command {
        "devices" => devices(),
        "rules" => rules(),
        "denials" => denials(),
        "drivers" => drivers(),
        "help" | "-h" | "--help" => {
            sys::write_str(USAGE);
            sys::write_str("\n");
            Ok(())
        }
        _ => Err(String::from(USAGE)),
    }
}

fn devices() -> Result<(), String> {
    let devices = inspect::inventory().map_err(explain)?;
    sys::write_str("ID  CLASS       PCI       VENDOR:DEV  OWNER       RIGHTS\n");
    for device in &devices {
        let owner = device
            .owner
            .map_or(String::from("-"), |uid| format!("{}", Uid(uid)));
        sys::write_str(&format!(
            "{:<3} {:<11} {:02x}/{:02x}/{:02x}  {:04x}:{:04x}   {:<11} {}\n",
            device.id,
            device.class_name(),
            device.class,
            device.subclass,
            device.prog_if,
            device.vendor,
            device.device,
            owner,
            device.rights
        ));
    }
    sys::write_str(&format!("{} devices\n", devices.len()));
    Ok(())
}

fn rules() -> Result<(), String> {
    let Some(rules) = inspect::policy().map_err(explain)? else {
        sys::write_str(
            "no class policy installed: every claim is judged by the Messenger policy alone\n",
        );
        return Ok(());
    };
    sys::write_str("UID         CLASS       METHOD  VERDICT\n");
    for rule in &rules {
        sys::write_str(&format!(
            "{:<11} {:<11} {:<7} {}\n",
            format!("{}", Uid(rule.actor)),
            class_name(rule.interface_id),
            method_name(rule.method),
            if rule.allow { "allow" } else { "deny" }
        ));
    }
    sys::write_str(&format!(
        "{} rules; any other non-root uid is refused every class\n",
        rules.len()
    ));
    Ok(())
}

fn denials() -> Result<(), String> {
    let denials = inspect::denials().map_err(explain)?;
    if denials.is_empty() {
        sys::write_str("no refused claims in the audit ring\n");
        return Ok(());
    }
    sys::write_str("TIME       UID         CLASS       DEVICE  REASON\n");
    for denial in &denials {
        sys::write_str(&format!(
            "{:>6}.{:02}s {:<11} {:<11} {:<7} {}\n",
            denial.ticks / 100,
            denial.ticks % 100,
            format!("{}", Uid(denial.uid)),
            class_name(denial.class_id),
            denial.device,
            reason_name(denial.reason)
        ));
    }
    Ok(())
}

/// `devd`'s view: each device's match, driver, state and claim holder.
fn drivers() -> Result<(), String> {
    let client = user::messenger::devd::Client::connect()
        .map_err(|_| String::from("devctl: devd is not running (an image without drivers?)"))?;
    let devices = client
        .devices()
        .map_err(|error| format!("devctl: devd failed: {}", error.message()))?;
    sys::write_str("ID  VENDOR:DEV CLASS    DRIVER  MODEL         STATE     OWNER\n");
    for device in &devices {
        let owner = if device.owner == u32::MAX {
            String::from("-")
        } else {
            format!("{}", Uid(device.owner))
        };
        let dash = |text: &str| String::from(if text.is_empty() { "-" } else { text });
        sys::write_str(&format!(
            "{:<3} {:04x}:{:04x}  {:<8} {:<7} {:<13} {:<9} {}\n",
            device.id,
            device.vendor,
            device.device,
            device.class,
            dash(&device.driver),
            dash(&device.model),
            device.state,
            owner
        ));
    }
    Ok(())
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
