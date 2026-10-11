//! `wifictl` (`/system/bin/wifictl`): the Wi-Fi command line, a client of
//! `wlanmd` (`os.lazy.net.wifi.v1`, docs/architecture/wifi.md).
//!
//! ```text
//! wifictl [status] [IF]          the connection state
//! wifictl scan [IF]              scan, then list what was heard
//! wifictl list [IF]              the networks heard, strongest first
//! wifictl connect SSID [IF]      join; asks for the passphrase of a new
//!                                WPA2 network (not echoed) and remembers it
//! wifictl disconnect [IF]        leave, and stop auto-connecting
//! wifictl forget ID              delete a remembered network
//! ```
//!
//! `IF` defaults to the first wireless interface. A headless run passes the
//! passphrase in `WIFICTL_PASSPHRASE` instead of typing it; it is never an
//! argument. Network names are printed as `wlanmd` sanitised them, quoted.
//! Lines: `WIFICTL:STATUS <if> <state> ...`, `WIFICTL:NET ...`,
//! `WIFICTL:FAIL <reason>`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

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

const USAGE: &str = "usage: wifictl [status|scan|list|disconnect] [IF] | connect SSID [IF] | forget ID\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let args: Vec<&str> = sys::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(text) => {
            sys::write_str(&format!("WIFICTL:FAIL {text}\n"));
            1
        }
    };
    sys::exit(code)
}

fn run(args: &[&str]) -> core::result::Result<(), String> {
    let (command, rest) = args.split_first().map_or(("status", &[][..]), |(c, r)| (*c, r));
    if matches!(command, "-h" | "--help" | "help") {
        sys::write_str(USAGE);
        return Ok(());
    }
    let client = Client::connect().map_err(|e| format!("wlanmd is not running ({})", e.message()))?;
    let pick = |at: usize| interface(&client, rest.get(at).copied());
    match command {
        "status" => status(&client, rest.first().copied()),
        "scan" => scan(&client, &pick(0)?),
        "list" => list(&client, &pick(0)?),
        "connect" => {
            let ssid = rest.first().ok_or_else(|| String::from(USAGE.trim_end()))?;
            connect(&client, &pick(1)?, ssid)
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
        line.push_str(&format!(" bssid={a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}"));
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
        sys::write_str("WIFICTL:STATUS none disabled\n");
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
    let mut bus = Bus::connect().ok();
    let subscription = bus.as_mut().and_then(|bus| {
        let filter = api::name_system_net_wifi_scan(ifname).ok()?;
        bus.subscribe(&filter).ok()
    });
    match client.scan(ifname, Vec::new()) {
        Ok(()) => {}
        // One already runs (auto-connect's, say): wait for that one.
        Err(Error::Errno(code)) if code == -user::messenger::errno::EBUSY => {}
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
    list(client, ifname)
}

fn list(client: &Client, ifname: &str) -> core::result::Result<(), String> {
    let networks = client.networks(ifname).map_err(text)?;
    for n in &networks {
        let security = Security::from_wire(n.security).map_or("?", Security::name);
        let name = if n.ssid.is_empty() { "(hidden)" } else { n.ssid.as_str() };
        sys::write_str(&format!(
            "WIFICTL:NET \"{name}\" {security} rssi={} freq={} bss={}{}{}\n",
            n.rssi_dbm,
            n.freq_mhz,
            n.bss_count,
            if n.id.is_empty() { String::new() } else { format!(" id={}", n.id) },
            if n.connected { " connected" } else { "" },
        ));
    }
    sys::write_str(&format!("{ifname}: {} network(s)\n", networks.len()));
    Ok(())
}

/// Join `ssid`: a known network directly, a new one after remembering it.
fn connect(client: &Client, ifname: &str, ssid: &str) -> core::result::Result<(), String> {
    let networks = client.networks(ifname).map_err(text)?;
    let heard = networks.iter().find(|n| n.ssid == ssid);
    let id = match heard {
        Some(n) if !n.id.is_empty() => n.id.clone(),
        Some(n) => add(client, ssid, n.security)?,
        None => return Err(format!("\"{ssid}\" was not heard; run `wifictl scan` first")),
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

/// Remember a network for this user; asks for the passphrase of a WPA2 one.
fn add(client: &Client, ssid: &str, security: u32) -> core::result::Result<String, String> {
    let passphrase = match Security::from_wire(security) {
        Some(Security::Open) => None,
        Some(Security::Wpa2Psk) => Some(passphrase(ssid)),
        _ => return Err(format!("\"{ssid}\" uses a security this system cannot join")),
    };
    let profile = api::Profile {
        id: String::new(),
        ssid: String::from(ssid),
        security,
        hidden: false,
        autoconnect: true,
        priority: 0,
        scope: api::SCOPE_USER,
    };
    let result = client.add_network(profile, passphrase);
    result.map_err(|error| match error {
        Error::Errno(code) if code == -user::messenger::errno::EINVAL => {
            String::from("refused: a passphrase is 8 to 63 printable characters or 64 hex digits")
        }
        other => text(other),
    })
}

/// The passphrase: the environment for a scripted run, else a silent prompt.
fn passphrase(ssid: &str) -> String {
    if let Some(secret) = sys::getenv(PASSPHRASE_VAR) {
        return String::from(secret);
    }
    sys::write_str(&format!("Passphrase for \"{ssid}\": "));
    let mut line = String::new();
    loop {
        match sys::read_char() as u8 {
            0 | b'\n' | b'\r' => break,
            8 | 0x7F => {
                line.pop();
            }
            byte if (0x20..0x7F).contains(&byte) && line.len() < 64 => line.push(char::from(byte)),
            _ => {}
        }
    }
    sys::write_str("\n");
    line
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("WIFICTL:FAIL panic\n");
    sys::exit(1)
}
