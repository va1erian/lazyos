//! What `hardware.rs` reads from a received frame besides handing it up:
//! the channel it arrived on (the chip's PHY status), and the BSS's legacy
//! rates (its beacons and association responses), which mac80211 would
//! have handed upstream as `bss_conf.basic_rates` and `sta->supp_rates`.
//!
//! Only fixed offsets after length checks and `libs/ieee80211`'s bounded
//! element walker; nothing here allocates or fails.

use ieee80211::ie::{ElementIter, ID_EXT_RATES, ID_RATES};
use rtw88::dev::Band as RtwBand;
use rtw88::rx::RxPktStat;
use wifimac::{Band, Channel};

/// The channel the PHY status says `stat`'s frame arrived on, when it says
/// one (upstream `rtw_set_rx_freq_band`'s band and centre frequency).
pub(super) fn rx_channel(stat: &RxPktStat) -> Option<Channel> {
    if !stat.phy_status || stat.channel_invalid {
        return None;
    }
    match (stat.band, stat.freq) {
        (0, 2484) => Some(Channel::new(Band::Ghz2, 14)),
        (0, f @ 2412..=2472) => Some(Channel::new(Band::Ghz2, ((f - 2407) / 5) as u8)),
        (1, f @ 5180..=5885) => Some(Channel::new(Band::Ghz5, ((f - 5000) / 5) as u8)),
        _ => None,
    }
}

/// The chip library's name for `band` (`None`: a band it does not drive).
pub(super) fn rtw_band(band: Band) -> Option<RtwBand> {
    match band {
        Band::Ghz2 => Some(RtwBand::Ghz2),
        Band::Ghz5 => Some(RtwBand::Ghz5),
        Band::Ghz6 => None,
    }
}

/// A BSS's legacy rates in `band`, as `rtw88::station::legacy_rates`
/// bitmaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BssRates {
    pub(super) band: RtwBand,
    pub(super) supported: u32,
    pub(super) basic: u32,
}

/// Octets of the fixed fields before the elements: timestamp, interval and
/// capability (beacon, probe response); capability, status and AID
/// ((re)association response).
const BEACON_FIXED: usize = 12;
const ASSOC_RESP_FIXED: usize = 6;
const MGMT_HEADER: usize = 24;

/// The rates `bssid` announces in `frame`, received in `band`: from its
/// (re)association response, and from a beacon or probe response when
/// `beacons` (the caller has none yet).
pub(super) fn bss_rates(
    frame: &[u8],
    bssid: &[u8; 6],
    band: RtwBand,
    beacons: bool,
) -> Option<BssRates> {
    let (&fc0, &fc1) = (frame.first()?, frame.get(1)?);
    // Version 0, management, not protected.
    if fc0 & 0x0f != 0 || fc1 & 0x40 != 0 {
        return None;
    }
    let fixed = match fc0 >> 4 {
        1 | 3 => ASSOC_RESP_FIXED,
        5 | 8 if beacons => BEACON_FIXED,
        _ => return None,
    };
    if frame.get(16..22)? != bssid {
        return None;
    }
    let elements = frame.get(MGMT_HEADER + fixed..)?;
    let octets = ElementIter::new(elements)
        .filter(|e| e.id == ID_RATES || e.id == ID_EXT_RATES)
        .flat_map(|e| e.body.iter().copied());
    let (supported, basic) = rtw88::station::legacy_rates(band, octets);
    (supported != 0).then_some(BssRates {
        band,
        supported,
        basic,
    })
}
