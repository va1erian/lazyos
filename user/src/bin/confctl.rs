//! `confctl` (`CONFCTL.ELF`): the configuration-registry command line (issue #260).
//!
//! A thin Messenger client of `confd`, run from the shell as
//! `run CONFCTL.ELF <command>`:
//!
//! ```text
//! confctl get <path>
//! confctl set <path> <type> <value>     type: bool | i64 | u64 | str | bytes
//! confctl delete <path>
//! confctl list [prefix]
//! confctl watch [filter]                default: system/confd/changed/sys/#
//! ```
//!
//! `bytes` values are hex on both sides. `watch` blocks, printing one line per
//! change, and is the manual check for the `(path, deleted)` topic payload.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use confd::Value as StoreValue;
use lazyos_crypto::hex;
use user::messenger::{self, confd as wire};
use user::sys;

/// The topic `watch` subscribes to when the caller gives no filter.
const DEFAULT_FILTER: &str = "system/confd/changed/sys/#";

const USAGE: &str = "usage: confctl <get|set|delete|list|watch> [args]\n\
    get <path>\n\
    set <path> <bool|i64|u64|str|bytes> <value>\n\
    delete <path>\n\
    list [prefix]\n\
    watch [filter]\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let args = args();
    let status = match run(&args) {
        Ok(()) => 0,
        Err(message) => {
            sys::write_str(&message);
            sys::write_str("\n");
            1
        }
    };
    sys::exit(status)
}

/// The argument string `init` passed on the spawn line.
fn args() -> Vec<String> {
    let mut buffer = [0u8; 256];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    text.split_whitespace().map(String::from).collect()
}

/// A line to the console.
fn say(text: &str) {
    sys::write_str(text);
    sys::write_str("\n");
}

/// Run one command against the service.
fn run(args: &[String]) -> Result<(), String> {
    let client = wire::Client::connect_retry(20).map_err(|error| error.message().to_string())?;
    match args.first().map(String::as_str) {
        Some("get") => {
            let path = arg(args, 1, "get <path>")?;
            match client.get(path) {
                Ok(Some(value)) => {
                    say(&format!("{path} = {}", render(&value)));
                    Ok(())
                }
                Ok(None) => {
                    say(&format!("{path}: not found"));
                    Ok(())
                }
                Err(error) => Err(format!("{path}: {}", error.message())),
            }
        }
        Some("set") => {
            let path = arg(args, 1, "set <path> <type> <value>")?;
            let kind = arg(args, 2, "set <path> <type> <value>")?;
            let raw = args
                .get(3..)
                .filter(|rest| !rest.is_empty())
                .ok_or_else(|| String::from("set <path> <type> <value>"))?;
            let value = parse_value(kind, &raw.join(" "))?;
            client
                .set(path, &value)
                .map_err(|error| format!("{path}: {}", error.message()))?;
            say(&format!("{path} set"));
            Ok(())
        }
        Some("delete") | Some("del") => {
            let path = arg(args, 1, "delete <path>")?;
            client
                .delete(path)
                .map_err(|error| format!("{path}: {}", error.message()))?;
            say(&format!("{path} deleted"));
            Ok(())
        }
        Some("list") | Some("ls") => {
            let prefix = args.get(1).map(String::as_str).unwrap_or("");
            let paths = client
                .list(prefix)
                .map_err(|error| error.message().to_string())?;
            if paths.is_empty() {
                say("(empty)");
            }
            for path in paths {
                say(&path);
            }
            Ok(())
        }
        Some("watch") => {
            let filter = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| String::from(DEFAULT_FILTER));
            watch(&client, &filter)
        }
        // `confd` spawns this at boot (`demo=1`) to prove the whole path over
        // the real Messenger transport and VFS; the kernel suite covers the
        // store logic itself.
        Some("demo") => selftest(&client),
        _ => Err(String::from(USAGE)),
    }
}

/// A boot self-test over the live service: set/get/list/replace/delete in the
/// caller's own subtree, then print the machine-parseable verdict.
fn selftest(client: &wire::Client) -> Result<(), String> {
    let result = run_selftest(client);
    match result {
        Ok(()) => {
            say("CONFCTL:SELFTEST:PASS");
            Ok(())
        }
        Err(detail) => {
            say(&format!("CONFCTL:SELFTEST:FAIL:{detail}"));
            Err(detail)
        }
    }
}

/// The self-test body; any `?` failure becomes the printed verdict.
fn run_selftest(client: &wire::Client) -> Result<(), String> {
    let mut cred = sys::Cred::default();
    let uid = sys::cred_get(None, &mut cred)
        .map(|_| cred.uid)
        .map_err(|error| format!("could not read own credentials: {error}"))?;
    // A non-root caller may only write its own subtree, so root uses sys/ and
    // everyone else uses user/<uid>/.
    let scope = if uid == 0 {
        String::from("sys/confctl-demo")
    } else {
        format!("user/{uid}/confctl-demo")
    };
    let path = format!("{scope}/value");

    let fail = |step: &str, error: String| format!("{step}: {error}");
    client
        .set(&path, &StoreValue::U64(42))
        .map_err(|error| fail("set", error.message().to_string()))?;
    let got = client
        .get(&path)
        .map_err(|error| fail("get", error.message().to_string()))?;
    if got != Some(StoreValue::U64(42)) {
        return Err(format!("get returned {got:?}, expected 42"));
    }
    let paths = client
        .list(&scope)
        .map_err(|error| fail("list", error.message().to_string()))?;
    if !paths.iter().any(|known| known == &path) {
        return Err(format!("list did not include {path}"));
    }
    client
        .set(&path, &StoreValue::Str(String::from("hello")))
        .map_err(|error| fail("replace", error.message().to_string()))?;
    client
        .delete(&path)
        .map_err(|error| fail("delete", error.message().to_string()))?;
    let gone = client
        .get(&path)
        .map_err(|error| fail("get-after-delete", error.message().to_string()))?;
    if gone.is_some() {
        return Err(String::from("delete left the value behind"));
    }
    Ok(())
}

/// The `index`-th argument or a usage error.
fn arg<'a>(args: &'a [String], index: usize, usage: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| String::from(usage))
}

/// Parse a typed value argument.
fn parse_value(kind: &str, raw: &str) -> Result<StoreValue, String> {
    match kind {
        "bool" => match raw {
            "true" | "1" | "yes" | "on" => Ok(StoreValue::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(StoreValue::Bool(false)),
            _ => Err(format!("not a bool: {raw}")),
        },
        "i64" => raw
            .parse::<i64>()
            .map(StoreValue::I64)
            .map_err(|_| format!("not an i64: {raw}")),
        "u64" => raw
            .parse::<u64>()
            .map(StoreValue::U64)
            .map_err(|_| format!("not a u64: {raw}")),
        "str" | "string" => Ok(StoreValue::Str(String::from(raw))),
        "bytes" => decode_hex(raw).map(StoreValue::Bytes),
        _ => Err(format!("unknown type: {kind}")),
    }
}

/// Render a value for the console (`bytes` as hex).
fn render(value: &StoreValue) -> String {
    match value {
        StoreValue::Bool(flag) => flag.to_string(),
        StoreValue::I64(number) => number.to_string(),
        StoreValue::U64(number) => number.to_string(),
        StoreValue::Str(text) => text.clone(),
        StoreValue::Bytes(bytes) => format!("0x{}", hex::encode(bytes)),
    }
}

/// Parse a hex string into bytes (an empty string is an empty value).
fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    let digits = text.strip_prefix("0x").unwrap_or(text);
    if !digits.len().is_multiple_of(2) {
        return Err(format!("odd-length hex: {text}"));
    }
    let mut bytes = Vec::with_capacity(digits.len() / 2);
    for pair in digits.as_bytes().as_chunks::<2>().0 {
        let high = hex_digit(pair[0]).ok_or_else(|| format!("bad hex: {text}"))?;
        let low = hex_digit(pair[1]).ok_or_else(|| format!("bad hex: {text}"))?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Print every change event until the process is killed.
fn watch(client: &wire::Client, filter: &str) -> Result<(), String> {
    let subscription = client
        .watch(filter)
        .map_err(|error| format!("watch {filter}: {}", error.message()))?;
    say(&format!("watching {filter}"));
    let mut buffer = vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        match subscription.recv_with(&mut buffer, None) {
            Ok(Some(event)) => {
                let change = wire::decode_system_confd_changed(&event.payload)
                    .map_err(|error| format!("bad change payload: {}", error.message()))?;
                let action = if change.deleted { "deleted" } else { "set" };
                say(&format!("{action} {}", change.path));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("watch {filter}: {}", error.message())),
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
