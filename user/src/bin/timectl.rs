//! `timectl` (`TIMECTL.ELF`): the `timed` command line and boot self-test
//! (issue #369).
//!
//! ```text
//! timectl now              print the instant, offset, zone and DST flag
//! timectl zone [name]      show, or set, the system time zone
//! timectl set <unix>       step the wall clock (needs CAP_SYS_TIME)
//! timectl selftest         drive every method over Messenger (boot evidence)
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;

use user::messenger::timed::Client;
use user::messenger::{errno, Error};
use user::sys;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = String::from(core::str::from_utf8(&buffer[..len]).unwrap_or(""));
    let mut words = text.split_whitespace();
    let status = match run(words.next().unwrap_or("now"), words.next()) {
        Ok(()) => 0,
        Err(message) => {
            sys::write_str(&message);
            sys::write_str("\n");
            1
        }
    };
    sys::exit(status)
}

fn text(error: Error) -> String {
    String::from(error.message())
}

fn run(command: &str, arg: Option<&str>) -> Result<(), String> {
    let client = Client::connect().map_err(text)?;
    match (command, arg) {
        ("now", _) => {
            let now = client.now().map_err(text)?;
            sys::write_str(&format!(
                "unix_ms={} offset={} zone={} dst={}\n",
                now.unix_ms, now.tz_offset_s, now.tz_name, now.dst
            ));
            Ok(())
        }
        ("zone", None) => {
            sys::write_str(&format!("{}\n", client.get_zone().map_err(text)?));
            Ok(())
        }
        ("zone", Some(name)) => client.set_zone(name).map_err(text),
        ("set", Some(secs)) => {
            let secs: i64 = secs.parse().map_err(|_| String::from("bad unix seconds"))?;
            client.set_time(secs).map_err(text)
        }
        ("selftest", _) => selftest(&client),
        _ => Err(String::from(
            "usage: timectl <now|zone [name]|set <unix>|selftest>",
        )),
    }
}

/// Every method once, over the real fabric, restoring UTC at the end.
fn selftest(client: &Client) -> Result<(), String> {
    let fail = |what: &str| Err(format!("TIMECTL:SELFTEST:FAIL {what}"));
    let now = client.now().map_err(text)?;
    if now.unix_ms < 1_700_000_000_000 {
        return fail("Now() is before 2023");
    }
    if client.get_zone().map_err(text)? != "UTC" || now.tz_offset_s != 0 || now.dst {
        return fail("the default zone is not UTC");
    }
    match client.set_zone("Mars/Base") {
        Err(Error::Errno(code)) if code == -errno::EINVAL => {}
        _ => return fail("an unknown zone was not refused with EINVAL"),
    }
    client.set_zone("Europe/Paris").map_err(text)?;
    if client.get_zone().map_err(text)? != "Europe/Paris" {
        return fail("GetZone did not follow SetZone");
    }
    let paris = client.now().map_err(text)?;
    if !(paris.tz_offset_s == 3600 && !paris.dst || paris.tz_offset_s == 7200 && paris.dst) {
        return fail("Paris offset and DST flag disagree");
    }
    client.set_time(now.unix_ms / 1000).map_err(text)?;
    match client.set_time(-5) {
        Err(Error::Errno(code)) if code == -errno::EINVAL => {}
        _ => return fail("a negative time was not refused with EINVAL"),
    }
    client.set_zone("UTC").map_err(text)?;
    sys::write_str(&format!(
        "TIMECTL:SELFTEST:PASS paris_offset={}\n",
        paris.tz_offset_s
    ));
    Ok(())
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
