//! `wifisim` (`/system/bin/wifisim`): the simulated Wi-Fi radio for CI
//! (docs/wifi-prerequisites-plan.md section 3.6, `LAZYOS_WIFI_SIM=1` images
//! only). It serves `os.lazy.net.wifi.hw.v1` as
//! `os.lazy.net.wifi.hw/wlan0` and `os.lazy.net.nic.v1` as
//! `os.lazy.net.nic/wlan0` (kind wireless) exactly as a chip driver would,
//! through the soft-MAC every radio program shares (`user::wifi`,
//! `libs/wifimac`), over a radio whose air is a second virtio-net card
//! (`air.rs`, wire format in `libs/simair`). The host side of that air is
//! `tools/wifi/fake_ap.py`.
//!
//! **The air card.** `netdrv` serves the card the harness gives the MAC
//! [`netpolicy::SIM_AIR_MAC`] as `os.lazy.net.nic/air0` instead of an `eth`
//! name (only in images built with the simulator), and `netd` never attaches
//! an `air*` card (`netpolicy::stack_attaches`): its frames are the
//! simulator's, not the network's. This program is that card's one client.
//!
//! Runs as `_wifisim` (uid 913) with **no capability**: it holds no device,
//! only a ring with `netdrv` and its two services. `probe=<mode>` (the
//! harness's `LAZYOS_WIFI_SIM_ARGS`) also starts `wifiprobe <mode>`, a test
//! client that drives `hw.v1` directly when no station manager is in the
//! image.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use user::sys;

#[path = "wifisim/air.rs"]
mod air;

/// The radio's interface name.
const IFNAME: &str = "wlan0";
/// How long to wait between looks for the air card.
const RETRY_MS: u64 = 500;

/// `probe=<mode>`: the test client's arguments, `:` separated.
fn probe_args() -> Option<Vec<String>> {
    let mut buffer = [0u8; 256];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let mode = text
        .split_whitespace()
        .find_map(|word| word.strip_prefix("probe="))?;
    Some(mode.split(':').map(String::from).collect())
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("wifisim: simulated Wi-Fi radio\n");
    let uid = match sys::cred_get(None) {
        Ok(cred) => {
            sys::write_str(&format!("WIFISIM:CRED uid={} caps={:#x}\n", cred.uid, cred.caps));
            cred.uid
        }
        Err(_) => netpolicy::WIFISIM_UID,
    };
    let air_name = netpolicy::SIM_AIR_IFNAME;
    let mut said = false;
    let radio = loop {
        match air::AirRadio::connect(air_name) {
            Ok(radio) => break radio,
            Err(why) => {
                if !said {
                    sys::write_str(&format!("WIFISIM:WAIT {why}\n"));
                    said = true;
                }
                let _ = sys::sleep_ns(RETRY_MS * 1_000_000);
            }
        }
    };
    sys::write_str(&format!("WIFISIM:AIR if={air_name} attached\n"));
    if let Some(args) = probe_args() {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        match sys::spawn_native(fhs::bin::WIFIPROBE, &words) {
            Some(pid) => sys::write_str(&format!("WIFISIM:PROBE:SPAWN pid={pid}\n")),
            None => sys::write_str("WIFISIM:PROBE:SPAWN failed\n"),
        }
    }
    match user::wifi::run(radio, IFNAME, uid) {
        Ok(()) => sys::exit(0),
        Err(why) => {
            sys::write_str(&format!("WIFISIM:FAIL {why}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
