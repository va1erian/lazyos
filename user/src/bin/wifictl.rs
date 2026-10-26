//! `wifictl` (`/system/bin/wifictl`): the Wi-Fi command line, a client of
//! `wlanmd` (`os.lazy.net.wifi.v1`, docs/architecture/wifi.md).
//!
//! ```text
//! wifictl [status] [IF]          the connection state
//! wifictl scan [IF]              scan, then list what was heard
//! wifictl list [IF]              the networks heard, strongest first
//! wifictl connect SSID [IF]      join; asks for the passphrase of a new
//!                                WPA2 network (not echoed) and remembers it
//! wifictl connect --id ID [IF]   join a remembered network by its id
//! wifictl connect --hidden [--open] SSID [IF]
//!                                remember a network that hides its name
//!                                (WPA2 unless --open) and join it
//! wifictl disconnect [IF]        leave, and stop auto-connecting
//! wifictl forget ID              delete a remembered network
//! ```
//!
//! `IF` defaults to the first wireless interface. A headless run passes the
//! passphrase in `WIFICTL_PASSPHRASE` instead of typing it; it is never an
//! argument. A passphrase is 8 to 63 octets of UTF-8 (or 64 hex digits).
//! Network names are printed as `wlanmd` sanitised them, quoted; `wlanmd`
//! matches a typed name against the raw names it heard first, so a network
//! whose name shows altered is still reachable, and `--id` names one
//! exactly when two show alike.
//! Lines: `WIFICTL:STATUS <if> <state> ...`, `WIFICTL:NET ...`,
//! `WIFICTL:FAIL <reason>`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::trace;

use user::central::Bus;
use user::messenger::wifi::{api, Client};
use user::messenger::Error;
use user::sys;
use wlansta::{ConnState, FailReason, Security};

/// The environment variable a scripted run passes a passphrase in.
const PASSPHRASE_VAR: &str = "WIFICTL_PASSPHRASE";
/// How long `scan` waits for the results, and `connect` for the outcome.
const SCAN_WAIT_TICKS: u64 = 2_000;
const CONNECT_WAIT_TICKS: u64 = 3_000;

const USAGE: &str =
    "usage: wifictl [status|scan|list|disconnect] [IF] | connect [--hidden [--open]] SSID [IF] \
                     | connect --id ID [IF] | forget ID\n";
/// Longest passphrase typed (a raw PSK is 64 hex digits).
const MAX_SECRET: usize = 64;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let args: Vec<&str> = sys::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(text) => {
            trace::print!(trace::Channel::Wifictl, "FAIL {text}");
            1
        }
    };
    sys::exit(code)
}

fn run(args: &[&str]) -> core::result::Result<(), String> {
    let (command, rest) = args
        .split_first()
        .map_or(("status", &[][..]), |(c, r)| (*c, r));
    if matches!(command, "-h" | "--help" | "help") {
        sys::write_str(USAGE);
        return Ok(());
    }
    let client =
        Client::connect().map_err(|e| format!("wlanmd is not running ({})", e.message()))?;
    let pick = |at: usize| interface(&client, rest.get(at).copied());
    match command {
        "status" => status(&client, rest.first().copied()),
        "scan" => scan(&client, &pick(0)?),
        "list" => list(&client, &pick(0)?),
        "connect" => {
            let (how, rest) = Target::parse(rest).ok_or_else(|| String::from(USAGE.trim_end()))?;
            let ifname = interface(&client, rest.first().copied())?;
            connect(&client, &ifname, how)
        }
        "disconnect" => {
            let ifname = pick(0)?;
            client.disconnect(&ifname).map_err(text)?;
            sys::write_str(&format!("{ifname}: disconnected\n"));
            Ok(())
        }
        "forget" => {
            let id = rest.first().ok_or_else(|| String::from(USAGE.trim_end()))?;
            client.forget(id).map_err(text)?;
            sys::write_str(&format!("forgot {id}\n"));
            Ok(())
        }
        _ => Err(String::from(USAGE.trim_end())),
    }
}

fn text(error: Error) -> String {
    String::from(error.message())
}

/// `wanted`, or the first interface `wlanmd` manages.
fn interface(client: &Client, wanted: Option<&str>) -> core::result::Result<String, String> {
    if let Some(name) = wanted {
        return Ok(String::from(name));
    }
    let names = client.interfaces().map_err(text)?;
    names
        .into_iter()
        .next()
        .ok_or_else(|| String::from("no wireless interface (disabled)"))
}

fn state_name(state: u32) -> &'static str {
    ConnState::from_wire(state).map_or("unknown", ConnState::name)
}

fn print_status(ifname: &str, s: &api::WifiState) {
    let mut line = format!("WIFICTL:STATUS {ifname} {}", state_name(s.state));
    if !s.ssid.is_empty() {
        line.push_str(&format!(" ssid=\"{}\"", s.ssid));
    }
    if let Ok(bssid) = <[u8; 6]>::try_from(s.bssid.as_slice()) {
        let [a, b, c, d, e, f] = bssid;
        line.push_str(&format!(
            " bssid={a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}"
        ));
    }
    if s.freq_mhz != 0 {
        line.push_str(&format!(" freq={} rssi={}", s.freq_mhz, s.rssi_dbm));
    }
    if !s.id.is_empty() {
        line.push_str(&format!(" id={}", s.id));
    }
    if let Some(reason) = FailReason::from_wire(s.reason).filter(|r| *r != FailReason::None) {
        line.push_str(&format!(" reason={}", reason.name()));
    }
    line.push('\n');
    sys::write_str(&line);
}

fn status(client: &Client, wanted: Option<&str>) -> core::result::Result<(), String> {
    let names = match wanted {
        Some(name) => alloc::vec![String::from(name)],
        None => client.interfaces().map_err(text)?,
    };
    if names.is_empty() {
        trace::print!(trace::Channel::Wifictl, "STATUS none disabled");
        return Ok(());
    }
    for name in names {
        let state = client.status(&name).map_err(text)?;
        print_status(&name, &state);
    }
    Ok(())
}

/// Scan and wait for the scan topic to say it finished, then list.
fn scan(client: &Client, ifname: &str) -> core::result::Result<(), String> {
    wait_scan(client, ifname, Vec::new())?;
    list(client, ifname)
}

/// Ask for a scan probing for `ssids` and wait until one finishes.
fn wait_scan(
    client: &Client,
    ifname: &str,
    ssids: Vec<String>,
) -> core::result::Result<(), String> {
    let mut bus = Bus::connect().ok();
    let subscription = bus.as_mut().and_then(|bus| {
        let filter = api::name_system_net_wifi_scan(ifname).ok()?;
        bus.subscribe(&filter).ok()
    });
    match client.scan(ifname, ssids) {
        Ok(()) => {}
        // One already runs (auto-connect's, say), or this user just asked:
        // wait for that one.
        Err(Error::Errno(code))
            if code == -user::messenger::errno::EBUSY
                || code == -user::messenger::errno::EAGAIN => {}
        Err(error) => return Err(text(error)),
    }
    let deadline = sys::clock() + SCAN_WAIT_TICKS;
    match subscription {
        Some(sub) => {
            let mut buf = alloc::vec![0u8; 1024];
            let _ = sub.recv_with(&mut buf, Some(deadline));
            let _ = sub.unsubscribe();
        }
        // No broker: give the scan a few seconds.
        None => {
            let until = sys::clock() + 400;
            while sys::clock() < until {
                sys::nap();
            }
        }
    }
    Ok(())
}

fn list(client: &Client, ifname: &str) -> core::result::Result<(), String> {
    let networks = client.networks(ifname).map_err(text)?;
    for n in &networks {
        let security = Security::from_wire(n.security).map_or("?", Security::name);
        let name = if n.ssid.is_empty() {
            "(hidden)"
        } else {
            n.ssid.as_str()
        };
        trace::print!(
            trace::Channel::Wifictl,
            "NET \"{name}\" {security} rssi={} freq={} bss={}{}{}",
            n.rssi_dbm,
            n.freq_mhz,
            n.bss_count,
            if n.id.is_empty() {
                String::new()
            } else {
                format!(" id={}", n.id)
            },
            if n.connected { " connected" } else { "" }
        );
    }
    sys::write_str(&format!("{ifname}: {} network(s)\n", networks.len()));
    Ok(())
}

/// What `connect` was asked to join.
enum Target<'a> {
    /// A network heard (or remembered) by this name.
    Named(&'a str),
    /// A remembered network by its profile id.
    Id(&'a str),
    /// A network that hides its name.
    Hidden { ssid: &'a str, open: bool },
}

impl<'a> Target<'a> {
    /// The target and the arguments after it (the interface).
    fn parse(args: &'a [&'a str]) -> Option<(Target<'a>, &'a [&'a str])> {
        match args {
            ["--id", id, rest @ ..] => Some((Target::Id(id), rest)),
            ["--hidden", "--open", ssid, rest @ ..] | ["--open", "--hidden", ssid, rest @ ..] => {
                Some((Target::Hidden { ssid, open: true }, rest))
            }
            ["--hidden", ssid, rest @ ..] => Some((Target::Hidden { ssid, open: false }, rest)),
            [flag, ..] if flag.starts_with("--") => None,
            [ssid, rest @ ..] => Some((Target::Named(ssid), rest)),
            [] => None,
        }
    }
}

/// Join a network: a known one directly, a new one after remembering it,
/// then wait for `Connected` or `Failed`.
fn connect(client: &Client, ifname: &str, how: Target<'_>) -> core::result::Result<(), String> {
    let id = match how {
        Target::Id(id) => String::from(id),
        Target::Named(ssid) => named(client, ifname, ssid)?,
        Target::Hidden { ssid, open } => hidden(client, ifname, ssid, open)?,
    };
    client.connect_network(ifname, &id).map_err(text)?;
    let deadline = sys::clock() + CONNECT_WAIT_TICKS;
    loop {
        let state = client.status(ifname).map_err(text)?;
        let done = matches!(
            ConnState::from_wire(state.state),
            Some(ConnState::Connected | ConnState::Failed)
        );
        if done || sys::clock() >= deadline {
            print_status(ifname, &state);
            return match ConnState::from_wire(state.state) {
                Some(ConnState::Connected) => Ok(()),
                _ => Err(String::from("not connected")),
            };
        }
        sys::sleep_ns(250_000_000);
    }
}

/// The id of the heard network shown as `ssid`, remembering it first if
/// it is new. Several networks shown alike are refused unless exactly one
/// is remembered: `--id` names one of the others.
fn named(client: &Client, ifname: &str, ssid: &str) -> core::result::Result<String, String> {
    let networks = client.networks(ifname).map_err(text)?;
    let heard: Vec<&api::Network> = networks.iter().filter(|n| n.ssid == ssid).collect();
    let known: Vec<&&api::Network> = heard.iter().filter(|n| !n.id.is_empty()).collect();
    match (heard.as_slice(), known.as_slice()) {
        ([], _) => Err(format!(
            "\"{ssid}\" was not heard; run `wifictl scan` first"
        )),
        (_, [one]) => Ok(one.id.clone()),
        // New (several shown alike included: `wlanmd` takes the exact name
        // first and refuses a guess between look-alikes).
        ([first, ..], []) => add(client, ssid, first.security, false),
        (_, _) => {
            let ids: Vec<&str> = known.iter().map(|n| n.id.as_str()).collect();
            Err(format!(
                "several networks show as \"{ssid}\": use `wifictl connect --id ID` ({})",
                ids.join(", ")
            ))
        }
    }
}

/// Remember a hidden network (or find it remembered) and return its id.
fn hidden(
    client: &Client,
    ifname: &str,
    ssid: &str,
    open: bool,
) -> core::result::Result<String, String> {
    let security = if open {
        Security::Open
    } else {
        Security::Wpa2Psk
    };
    match add(client, ssid, security as u32, true) {
        Ok(id) => return Ok(id),
        Err(why) if why != EXISTS => return Err(why),
        Err(_) => {}
    }
    // Remembered already: probe for it so the scan names it, and take its id.
    wait_scan(client, ifname, alloc::vec![String::from(ssid)])?;
    let networks = client.networks(ifname).map_err(text)?;
    networks
        .iter()
        .find(|n| n.ssid == ssid && !n.id.is_empty())
        .map(|n| n.id.clone())
        .ok_or_else(|| format!("\"{ssid}\" is remembered but did not answer a probe"))
}

/// What `add` says when the network is remembered already.
const EXISTS: &str = "already remembered";

/// Remember a network for this user; asks for the passphrase of a WPA2 one.
fn add(
    client: &Client,
    ssid: &str,
    security: u32,
    hidden: bool,
) -> core::result::Result<String, String> {
    let passphrase = match Security::from_wire(security) {
        Some(Security::Open) => None,
        Some(Security::Wpa2Psk) => Some(passphrase(ssid)?),
        _ => {
            return Err(format!(
                "\"{ssid}\" uses a security this system cannot join"
            ))
        }
    };
    let profile = api::Profile {
        id: String::new(),
        ssid: String::from(ssid),
        security,
        hidden,
        autoconnect: true,
        priority: 0,
        scope: api::SCOPE_USER,
    };
    // `add_network` wipes its copies of the passphrase.
    let result = client.add_network(profile, passphrase);
    result.map_err(|error| match error {
        Error::Errno(code) if code == -user::messenger::errno::EINVAL => String::from(
            "refused: a passphrase is 8 to 63 octets of text or 64 hex digits, \
             and the name must match one network",
        ),
        Error::Errno(code) if code == -user::messenger::errno::EEXIST => String::from(EXISTS),
        other => text(other),
    })
}

/// The passphrase: the environment for a scripted run, else a silent
/// prompt (UTF-8, at most [`MAX_SECRET`] octets). The line buffer is
/// wiped; the returned text is wiped by `Client::add_network`.
fn passphrase(ssid: &str) -> core::result::Result<String, String> {
    if let Some(secret) = sys::getenv(PASSPHRASE_VAR) {
        return Ok(String::from(secret));
    }
    sys::write_str(&format!("Passphrase for \"{ssid}\": "));
    let mut line = [0u8; MAX_SECRET];
    let mut len = 0;
    loop {
        match sys::read_char() as u8 {
            0 | b'\n' | b'\r' => break,
            8 | 0x7F => {
                // Remove one character: its continuation octets too.
                while len > 0 {
                    len -= 1;
                    if line[len] & 0xC0 != 0x80 {
                        break;
                    }
                }
            }
            byte if byte >= 0x20 && len < MAX_SECRET => {
                line[len] = byte;
                len += 1;
            }
            _ => {}
        }
    }
    sys::write_str("\n");
    let text = core::str::from_utf8(&line[..len]).map(String::from);
    for byte in line.iter_mut() {
        // SAFETY: `byte` is a valid, aligned, exclusive reference.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    text.map_err(|_| String::from("the passphrase is not valid UTF-8"))
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    trace::print!(trace::Channel::Wifictl, "FAIL panic");
    sys::exit(1)
}
