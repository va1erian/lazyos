//! `wifiprobe` (`/system/bin/wifiprobe`): a test client of
//! `os.lazy.net.wifi.hw.v1` for images without the station manager
//! (`LAZYOS_WIFI_SIM=1`; `wifisim probe=<mode>` starts it, so it runs as
//! `_wifisim` and may own the radio). It is the evidence that the radio side
//! works end to end against `tools/wifi/fake_ap.py`:
//!
//! * `scan` prints every BSS a wildcard scan of 2.4 GHz heard
//!   (`WIFIPROBE:SCAN:BSS ...`, `WIFIPROBE:SCAN:DONE count=N`);
//! * `open:<ssid>` joins that open network and opens the port, so `netd`
//!   takes a DHCP lease on `wlan0`;
//! * `wpa2:<ssid>:<passphrase>` does the same through the 4-way handshake
//!   (`libs/eapol`, the PMK computed here, not by `keyd`);
//! * `suite:<open>:<wpa2>:<passphrase>:<wrong>:<hidden>` is the harness's
//!   run (`tools/wifi/sim_run.py`): a wildcard scan, a directed scan that
//!   finds the hidden network, a WPA2 join with the wrong passphrase that
//!   must fail, the open join held for [`HOLD_MS`] (DHCP and the AP's pings
//!   on `wlan0`), a leave, then the WPA2 join, kept.
//!
//! After a join it stays: it answers group rekeys and reports beacon loss
//! and deauthentication (`WIFIPROBE:EVENT:*`). It is not a station manager:
//! no retry, no roaming, no profile.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use ieee80211::frame::Mgmt;
use ieee80211::{Bss, Security};
use user::sys;
use user::wifi::hw::wire;

#[path = "wifiprobe/client.rs"]
mod client;
#[path = "wifiprobe/join.rs"]
mod join;

use client::{Event, HwClient};

const IFNAME: &str = "wlan0";
/// The regulatory domain the probe asks for (channels 1-13 active).
const COUNTRY: &str = "FR";
/// Scans before the probe gives up looking for its SSID.
const SCANS: usize = 4;
/// How long the suite stays on the open network (a DHCP lease and the AP's
/// five pings fit).
const HOLD_MS: u64 = 15_000;
/// "Deauthenticated because sending STA is leaving".
const REASON_LEAVING: u32 = 3;

fn mac_text(mac: &[u8]) -> String {
    let parts: Vec<String> = mac.iter().map(|b| format!("{b:02x}")).collect();
    parts.join(":")
}

/// One scan of 2.4 GHz, probing for `ssids` (wildcard when empty); every
/// BSS is printed.
fn scan(client: &mut HwClient, ssids: &[&str]) -> Result<Vec<wire::ScanEntry>, String> {
    let scan_id = client.scan(wire::ScanRequest {
        channels: Vec::new(),
        band: wire::BAND_GHZ2,
        ssids: ssids.iter().map(|s| s.as_bytes().to_vec()).collect(),
        active: true,
        dwell_ms: 0,
    })?;
    let deadline = sys::monotonic_ms() + 10_000;
    loop {
        match client.next_event(deadline)? {
            Some(Event::ScanDone {
                scan_id: id,
                aborted,
            }) if id == scan_id => {
                if aborted {
                    return Err(String::from("the radio aborted the scan"));
                }
                break;
            }
            Some(_) => {}
            None => return Err(String::from("the scan never finished")),
        }
    }
    let entries = client.scan_results(scan_id)?;
    let now = sys::monotonic_ms();
    for entry in &entries {
        let Ok(frame) = Mgmt::parse(&entry.frame) else {
            continue;
        };
        let Some(bss) = Bss::from_frame(&frame, entry.channel as u8, entry.rssi_dbm as i8, now)
        else {
            continue;
        };
        let security = match bss.security {
            Security::Open => "open",
            Security::Rsn(_) => "wpa2",
            _ => "other",
        };
        sys::write_str(&format!(
            "WIFIPROBE:SCAN:BSS bssid={} ssid={} ch={} rssi={} sec={security} age={} kind={}\n",
            mac_text(&bss.bssid),
            bss.ssid_lossy(),
            entry.channel,
            entry.rssi_dbm,
            entry.age_ms,
            if bss.from_probe_response {
                "probe"
            } else {
                "beacon"
            }
        ));
    }
    sys::write_str(&format!("WIFIPROBE:SCAN:DONE count={}\n", entries.len()));
    Ok(entries)
}

/// Scan until `ssid` shows up.
fn find(client: &mut HwClient, ssid: &str) -> Result<Bss, String> {
    for _ in 0..SCANS {
        let entries = scan(client, &[])?;
        if let Some(bss) = join::pick(&entries, ssid, sys::monotonic_ms()) {
            return Ok(bss);
        }
    }
    Err(format!("no BSS of {ssid} heard"))
}

/// After a join: report what the radio says, answer group rekeys.
fn stay(
    client: &mut HwClient,
    sta: &[u8; 6],
    bss: &Bss,
    aid: u16,
    mut sup: Option<join::Sup>,
) -> ! {
    loop {
        let event = client.next_event(sys::monotonic_ms() + 60_000);
        match event {
            Ok(Some(Event::BeaconLoss)) => sys::write_str("WIFIPROBE:EVENT:BEACONLOSS\n"),
            Ok(Some(Event::Deauthenticated { reason })) => {
                sys::write_str(&format!("WIFIPROBE:EVENT:DEAUTH reason={reason}\n"))
            }
            Ok(Some(Event::RxMgmt { kind, frame, .. })) if kind == wire::FRAME_KIND_EAPOL => {
                let Some(sup) = sup.as_mut() else { continue };
                match sup.input(frame.get(14..).unwrap_or(&[])) {
                    Ok(actions) => match join::perform(client, sta, bss, aid, actions) {
                        Ok(_) => sys::write_str("WIFIPROBE:EVENT:REKEY\n"),
                        Err(why) => sys::write_str(&format!("WIFIPROBE:EVENT:REKEY:FAIL {why}\n")),
                    },
                    Err(error) => sys::write_str(&format!("WIFIPROBE:EAPOL:REFUSED {error:?}\n")),
                }
            }
            Ok(_) => {}
            Err(why) => {
                sys::write_str(&format!("WIFIPROBE:FAIL events: {why}\n"));
                sys::exit(1);
            }
        }
    }
}

/// Wait out `ms`, printing what the radio reports meanwhile.
fn hold(client: &mut HwClient, ms: u64) -> Result<(), String> {
    let until = sys::monotonic_ms() + ms;
    while let Some(event) = client.next_event(until)? {
        match event {
            Event::BeaconLoss => sys::write_str("WIFIPROBE:EVENT:BEACONLOSS\n"),
            Event::Deauthenticated { reason } => {
                sys::write_str(&format!("WIFIPROBE:EVENT:DEAUTH reason={reason}\n"))
            }
            _ => {}
        }
    }
    Ok(())
}

/// The harness's run (see the module notes).
fn suite(client: &mut HwClient, sta: &[u8; 6], args: &[String]) -> ! {
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let (open, wpa2, passphrase, wrong, hidden) = (arg(0), arg(1), arg(2), arg(3), arg(4));
    let outcome = (|| -> Result<(Bss, u16, join::Sup), String> {
        scan(client, &[])?;
        let directed = scan(client, &[hidden])?;
        match join::pick(&directed, hidden, sys::monotonic_ms()) {
            Some(bss) => sys::write_str(&format!(
                "WIFIPROBE:HIDDEN:FOUND ssid={hidden} ch={}\n",
                bss.channel
            )),
            None => sys::write_str(&format!("WIFIPROBE:HIDDEN:MISSING ssid={hidden}\n")),
        }
        let bss = find(client, wrong)?;
        match join::join_wpa2(client, sta, &bss, passphrase) {
            Ok(_) => sys::write_str(&format!("WIFIPROBE:WRONG:ACCEPTED ssid={wrong}\n")),
            Err(why) => sys::write_str(&format!("WIFIPROBE:WRONG:REFUSED ssid={wrong} ({why})\n")),
        }
        client.leave(REASON_LEAVING)?;
        let bss = find(client, open)?;
        let aid = join::join_open(client, sta, &bss)?;
        sys::write_str(&format!(
            "WIFIPROBE:CONNECTED ssid={open} security=open aid={aid}\n"
        ));
        hold(client, HOLD_MS)?;
        client.leave(REASON_LEAVING)?;
        sys::write_str(&format!("WIFIPROBE:LEAVE ssid={open}\n"));
        let bss = find(client, wpa2)?;
        let (aid, sup) = join::join_wpa2(client, sta, &bss, passphrase)?;
        sys::write_str(&format!(
            "WIFIPROBE:CONNECTED ssid={wpa2} security=wpa2 aid={aid}\n"
        ));
        Ok((bss, aid, sup))
    })();
    match outcome {
        Ok((bss, aid, sup)) => stay(client, sta, &bss, aid, Some(sup)),
        Err(why) => {
            sys::write_str(&format!("WIFIPROBE:FAIL {why}\n"));
            sys::exit(1)
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut client = HwClient::connect(IFNAME, 30_000)?;
    let info = client.info()?;
    let sta: [u8; 6] = info.mac.as_slice().try_into().map_err(|_| "bad MAC")?;
    sys::write_str(&format!(
        "WIFIPROBE:INFO if={} mac={} bands={:#x} ciphers={:#x} max_mgmt={}\n",
        info.interface,
        mac_text(&sta),
        info.bands,
        info.ciphers,
        info.max_mgmt
    ));
    client.set_country(COUNTRY)?;
    match args.first().map(String::as_str) {
        Some("scan") | None => {
            scan(&mut client, &[])?;
            Ok(())
        }
        Some("suite") => suite(&mut client, &sta, &args[1..]),
        Some("open") => {
            let ssid = args.get(1).ok_or("open needs an SSID")?;
            let bss = find(&mut client, ssid)?;
            let aid = join::join_open(&mut client, &sta, &bss)?;
            sys::write_str(&format!(
                "WIFIPROBE:CONNECTED ssid={ssid} security=open aid={aid}\n"
            ));
            stay(&mut client, &sta, &bss, aid, None)
        }
        Some("wpa2") => {
            let ssid = args.get(1).ok_or("wpa2 needs an SSID")?;
            let passphrase = args.get(2).ok_or("wpa2 needs a passphrase")?;
            let bss = find(&mut client, ssid)?;
            let (aid, sup) = join::join_wpa2(&mut client, &sta, &bss, passphrase)?;
            sys::write_str(&format!(
                "WIFIPROBE:CONNECTED ssid={ssid} security=wpa2 aid={aid}\n"
            ));
            stay(&mut client, &sta, &bss, aid, Some(sup))
        }
        Some(other) => Err(format!("unknown mode {other}")),
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let args: Vec<String> = sys::args().map(String::from).collect();
    // `args[0]` is the program's own name.
    let words = args.get(1..).unwrap_or(&[]).to_vec();
    match run(&words) {
        Ok(()) => sys::exit(0),
        Err(why) => {
            sys::write_str(&format!("WIFIPROBE:FAIL {why}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("WIFIPROBE:FAIL panic\n");
    sys::exit(1)
}
