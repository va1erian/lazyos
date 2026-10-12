//! What a station manager does, the minimum of it: pick a BSS from a scan,
//! authenticate (open system), associate, run the 4-way handshake for
//! WPA2-PSK through `libs/eapol`, install the keys and open the port; then
//! stay, answering group rekeys and reporting beacon loss and
//! deauthentication.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user::trace;

use eapol::{Action, Akm, Cipher, Config, Key, Standard, Supplicant};
use ieee80211::build;
use ieee80211::frame::{Body, Mgmt, AUTH_OPEN, STATUS_SUCCESS};
use ieee80211::ie::IeBuilder;
use ieee80211::{cap, Bss, Rsn, Security};
use user::sys;
use user::wifi::hw::wire;

use super::client::{Event, HwClient};

/// How long one step of the exchange may take.
const STEP_MS: u64 = 3000;
/// The 4-way handshake's deadline after association.
const HANDSHAKE_MS: u64 = 5000;

fn hex(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Wait for a management frame of `subtype` from the BSS.
fn wait_mgmt(client: &mut HwClient, subtype: u8) -> Result<Vec<u8>, String> {
    let deadline = sys::monotonic_ms() + STEP_MS;
    while let Some(event) = client.next_event(deadline)? {
        if let Event::RxMgmt { kind, frame, .. } = event {
            if kind == wire::FRAME_KIND_MGMT && frame.first().map(|b| b >> 4) == Some(subtype) {
                return Ok(frame);
            }
        }
    }
    Err(format!("no management frame of subtype {subtype}"))
}

/// Authenticate and associate with `bss`; returns the AID. `rsn_ie` is the
/// RSN element to send (a WPA2 join), if any.
pub fn associate(
    client: &mut HwClient,
    sta: &[u8; 6],
    bss: &Bss,
    rsn_ie: Option<&[u8]>,
) -> Result<u16, String> {
    client.join(&bss.bssid, u32::from(bss.channel), wire::BAND_GHZ2)?;
    let auth = build::auth(&bss.bssid, sta, &bss.bssid, 0, (AUTH_OPEN, 1, 0), &[]);
    client.tx(wire::FRAME_KIND_MGMT, auth)?;
    let reply = wait_mgmt(client, ieee80211::frame::SUB_AUTH)?;
    match Mgmt::parse(&reply).map(|m| m.body) {
        Ok(Body::Auth(a)) if a.status == STATUS_SUCCESS && a.transaction == 2 => {}
        _ => return Err(String::from("authentication refused")),
    }
    client.set_state(wire::STA_STATE_AUTHENTICATED, 0)?;
    trace::print!(
        trace::Channel::Wifiprobe,
        "AUTH:PASS bssid={}",
        hex(&bss.bssid)
    );
    let mut ies = IeBuilder::new()
        .ssid(&bss.ssid)
        .and_then(|b| b.rates(&[0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24]))
        .map_err(|e| format!("{e:?}"))?;
    if let Some(rsn) = rsn_ie {
        ies = ies.raw(48, &rsn[2..]).map_err(|e| format!("{e:?}"))?;
    }
    let capability = cap::ESS | if rsn_ie.is_some() { cap::PRIVACY } else { 0 };
    let request = build::assoc_request(sta, &bss.bssid, 0, capability, 10, &ies.finish());
    client.tx(wire::FRAME_KIND_MGMT, request)?;
    let reply = wait_mgmt(client, ieee80211::frame::SUB_ASSOC_RESP)?;
    let aid = match Mgmt::parse(&reply).map(|m| m.body) {
        Ok(Body::AssocResponse(a)) if a.status == STATUS_SUCCESS => a.aid(),
        _ => return Err(String::from("association refused")),
    };
    client.set_state(wire::STA_STATE_ASSOCIATED, u32::from(aid))?;
    trace::print!(trace::Channel::Wifiprobe, "ASSOC:PASS aid={aid}");
    Ok(aid)
}

/// An open network: associate and open the port.
pub fn join_open(client: &mut HwClient, sta: &[u8; 6], bss: &Bss) -> Result<u16, String> {
    if bss.security != Security::Open {
        return Err(String::from("not an open network"));
    }
    let aid = associate(client, sta, bss, None)?;
    client.set_state(wire::STA_STATE_AUTHORIZED, u32::from(aid))?;
    Ok(aid)
}

fn nonce() -> [u8; 32] {
    let mut out = [0u8; 32];
    let _ = sys::random(&mut out);
    out
}

/// An EAPOL PDU as the 802.3 frame `TxMgmt` takes.
fn eapol_frame(bssid: &[u8; 6], sta: &[u8; 6], pdu: &[u8]) -> Vec<u8> {
    let mut eth = Vec::with_capacity(14 + pdu.len());
    eth.extend_from_slice(bssid);
    eth.extend_from_slice(sta);
    eth.extend_from_slice(&ieee80211::data::ETHERTYPE_EAPOL.to_be_bytes());
    eth.extend_from_slice(pdu);
    eth
}

pub type Sup = Supplicant<Standard<fn() -> [u8; 32]>>;

/// Run one supplicant step's actions. Returns whether the port opened.
pub fn perform(
    client: &HwClient,
    sta: &[u8; 6],
    bss: &Bss,
    aid: u16,
    actions: Vec<Action>,
) -> Result<bool, String> {
    let mut authorized = false;
    for action in actions {
        match action {
            Action::Send(pdu) => {
                client.tx(wire::FRAME_KIND_EAPOL, eapol_frame(&bss.bssid, sta, &pdu))?
            }
            Action::InstallPtk { tk, .. } => {
                client.set_key(wire::KEY_KIND_PAIRWISE, 0, tk.expose(), &[], &bss.bssid)?;
                trace::print!(trace::Channel::Wifiprobe, "KEYS:PTK");
            }
            Action::InstallGtk {
                index, key, rsc, ..
            } => {
                client.set_key(
                    wire::KEY_KIND_GROUP,
                    u32::from(index),
                    key.expose(),
                    &rsc,
                    &bss.bssid,
                )?;
                trace::print!(trace::Channel::Wifiprobe, "KEYS:GTK index={index}");
            }
            Action::Authorized => {
                client.set_state(wire::STA_STATE_AUTHORIZED, u32::from(aid))?;
                authorized = true;
            }
        }
    }
    Ok(authorized)
}

/// A WPA2-PSK network: associate with our RSN element, then the 4-way
/// handshake with the PMK from `passphrase`.
pub fn join_wpa2(
    client: &mut HwClient,
    sta: &[u8; 6],
    bss: &Bss,
    passphrase: &str,
) -> Result<(u16, Sup), String> {
    let ap_rsn_ie = bss.rsn_ie.clone().ok_or("no RSN element")?;
    let ours = Rsn::wpa2_psk().to_ie().map_err(|e| format!("{e:?}"))?;
    let pmk = lazyos_crypto::wifi::pbkdf2_sha1(passphrase.as_bytes(), &bss.ssid)
        .map_err(|e| String::from(e.message()))?;
    let config = Config {
        akm: Akm::Psk,
        pairwise: Cipher::Ccmp128,
        group: Cipher::Ccmp128,
        pmk: Key::new(pmk),
        aa: bss.bssid,
        spa: *sta,
        ap_rsn_ie,
        assoc_rsn_ie: ours.clone(),
    };
    let mut sup: Sup = Supplicant::new(config, Standard::new(nonce as fn() -> [u8; 32]))
        .map_err(|e| format!("supplicant: {e:?}"))?;
    let aid = associate(client, sta, bss, Some(&ours))?;
    let deadline = sys::monotonic_ms() + HANDSHAKE_MS;
    while let Some(event) = client.next_event(deadline)? {
        let Event::RxMgmt { kind, frame, .. } = event else {
            continue;
        };
        if kind != wire::FRAME_KIND_EAPOL || frame.len() < 14 {
            continue;
        }
        match sup.input(&frame[14..]) {
            Ok(actions) => {
                if perform(client, sta, bss, aid, actions)? {
                    return Ok((aid, sup));
                }
            }
            Err(error) => trace::print!(trace::Channel::Wifiprobe, "EAPOL:REFUSED {error:?}"),
        }
    }
    Err(String::from("the 4-way handshake did not finish"))
}

/// The strongest BSS of `ssid` in a scan.
pub fn pick(entries: &[wire::ScanEntry], ssid: &str, now: u64) -> Option<Bss> {
    entries
        .iter()
        .filter_map(|e| {
            let frame = Mgmt::parse(&e.frame).ok()?;
            Bss::from_frame(&frame, e.channel as u8, e.rssi_dbm as i8, now)
        })
        .filter(|bss| bss.ssid == ssid.as_bytes())
        .max_by_key(|bss| bss.rssi)
}
