//! What a heard BSS requires, and which known network to join.

use alloc::vec::Vec;

use ieee80211::{Akm, Bss, Cipher, Rsn, RsnCaps};

use crate::types::{Band, Profile, Security};

/// How the station would join a BSS: open, or WPA2-PSK with this AKM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    Open,
    Psk(Akm),
}

/// Classify a BSS. RSN is joinable only as `eapol` supports it: CCMP-128
/// pairwise and group, AKM 2 (preferred: every WPA2 AP has it) or 6, and no
/// management frame protection required. Everything else is
/// [`Security::Unsupported`] and never joined.
pub fn classify(bss: &Bss) -> (Security, Option<Join>) {
    match &bss.security {
        ieee80211::Security::Open => (Security::Open, Some(Join::Open)),
        ieee80211::Security::Rsn(rsn) => match psk_akm(rsn) {
            Some(akm) => (Security::Wpa2Psk, Some(Join::Psk(akm))),
            None => (Security::Unsupported, None),
        },
        _ => (Security::Unsupported, None),
    }
}

fn psk_akm(rsn: &Rsn) -> Option<Akm> {
    let ccmp = rsn.group == Cipher::Ccmp128 && rsn.pairwise.contains(&Cipher::Ccmp128);
    if !ccmp || rsn.caps.mfp_required() {
        return None;
    }
    [Akm::Psk, Akm::PskSha256]
        .into_iter()
        .find(|akm| rsn.akms.contains(akm))
}

/// The RSN element the association request carries for `akm`: exactly one
/// pairwise cipher and one AKM, as `eapol::Supplicant::new` requires and as
/// message 2 repeats.
pub fn assoc_rsn(akm: Akm) -> Rsn {
    Rsn {
        group: Cipher::Ccmp128,
        pairwise: alloc::vec![Cipher::Ccmp128],
        akms: alloc::vec![akm],
        caps: RsnCaps(0),
        pmkids: Vec::new(),
        group_mgmt: None,
    }
}

/// A BSS chosen for a profile.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub profile: Profile,
    pub bss: Bss,
    pub band: Band,
    pub join: Join,
}

/// The outcome of a selection.
#[derive(Debug)]
pub enum Pick {
    Found(alloc::boxed::Box<Candidate>),
    /// A BSS of a wanted SSID was heard, but none the station can join.
    Unsupported,
    /// Nothing of a wanted SSID was heard.
    NotFound,
}

/// The best BSS for any of `profiles` among `heard` (with each BSS's band):
/// highest profile priority first, then strongest signal. A BSS is fit for a
/// profile when its SSID is the profile's (a hidden beacon has no name and
/// never matches) and its security is the profile's.
pub fn pick<'a>(profiles: &[&Profile], heard: impl Iterator<Item = (&'a Bss, Band)>) -> Pick {
    let mut best: Option<Candidate> = None;
    let mut seen_ssid = false;
    for (bss, band) in heard {
        if bss.hidden {
            continue;
        }
        let (security, join) = classify(bss);
        for profile in profiles.iter().filter(|p| p.ssid == bss.ssid) {
            seen_ssid = true;
            let Some(join) = join.filter(|_| security == profile.security) else {
                continue;
            };
            let better = best.as_ref().is_none_or(|b| {
                (profile.priority, bss.rssi) > (b.profile.priority, b.bss.rssi)
            });
            if better {
                best = Some(Candidate {
                    profile: (*profile).clone(),
                    bss: bss.clone(),
                    band,
                    join,
                });
            }
        }
    }
    match best {
        Some(candidate) => Pick::Found(alloc::boxed::Box::new(candidate)),
        None if seen_ssid => Pick::Unsupported,
        None => Pick::NotFound,
    }
}
