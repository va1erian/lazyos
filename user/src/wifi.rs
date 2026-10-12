//! The serving half of a Wi-Fi radio program (docs/wifi-rtw88-plan.md
//! section 3): a [`wifimac::SoftMac`] over the program's radio, served as
//! two Messenger interfaces from one loop ([`server::run`]):
//!
//! * `os.lazy.net.wifi.hw.v1` under `os.lazy.net.wifi.hw/<ifname>`
//!   ([`hw::service_name`]) for the station manager `wlanmd`, which finds
//!   radios by listing that prefix ([`hw`]);
//! * `os.lazy.net.nic.v1` under `os.lazy.net.nic/<ifname>` with
//!   `NicInfo.kind = Wireless` for `netd`, through the same driver engine
//!   `netdrv` uses (`libs/nicdrv`) over the soft-MAC's data path ([`nic`]).
//!
//! `wifisim` (the simulated radio) is the first program built on it; `wifid`
//! (the RTL8822CE over `libs/rtw88`) is meant to be the second, implementing
//! [`server::Hardware`] and nothing else.

pub mod hw;
pub mod nic;
pub mod server;

pub use server::{run, Hardware};
