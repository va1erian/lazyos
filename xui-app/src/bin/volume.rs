//! `volume` (`os.lazy.volume`): the sound volume in the taskbar tray
//! (docs/tray-plan.md T3). A resident applet with no window: a Lucide
//! `volume-2` icon whose tooltip shows the master volume, the wheel over it
//! turns the volume up and down in 5 % steps, a click or the menu's Mute row
//! mutes, all through the system mixer's control interface
//! (`os.lazy.audio.mixer.v1`, `audiod`). It opens with every session.
//!
//! Serial evidence: `VOLUME:UP:PASS level=<percent> mute=<bool>` once it
//! read the mixer (`VOLUME:NOCARD` without a mixer or a card),
//! `VOLUME:LEVEL:<percent>` and `VOLUME:MUTE:<bool>` on every change,
//! `VOLUME:QUIT:PASS` when `init` asks it to quit.

use audioclient::MixerControl;
use trayclient::{item, lucide, menu_row, wire, Event};
use xui_app::platform::audio::{Audio, UNITY_GAIN};
use xui_app::resident::{Resident, Wake};

/// One wheel notch: 5 % of unity gain.
const STEP: u32 = UNITY_GAIN / 20;
/// The menu's Mute row.
const ROW_MUTE: u32 = 1;

/// The master volume as the applet last set or read it.
struct Volume {
    gain: u32,
    mute: bool,
    card: bool,
}

impl Volume {
    fn percent(&self) -> u32 {
        self.gain.saturating_mul(100) / UNITY_GAIN
    }

    fn tooltip(&self) -> String {
        if !self.card {
            String::from("No sound card")
        } else if self.mute {
            format!("Muted ({} %)", self.percent())
        } else {
            format!("Volume {} %", self.percent())
        }
    }

    fn menu(&self) -> Vec<wire::MenuItem> {
        let mut mute = menu_row(ROW_MUTE, "Mute", wire::MENU_KIND_CHECK);
        mute.checked = self.mute;
        mute.enabled = self.card;
        vec![mute]
    }

    fn status(&self) -> u32 {
        if self.card && !self.mute {
            wire::STATUS_ACTIVE
        } else {
            wire::STATUS_PASSIVE
        }
    }
}

/// Read the mixer's master volume; `None` without a mixer.
fn read() -> Option<Volume> {
    let mixer = MixerControl::new(Audio::try_connect()?);
    let master = mixer.master().ok()?;
    Some(Volume {
        gain: master.gain_q16.min(UNITY_GAIN),
        mute: master.mute,
        card: master.card,
    })
}

/// Apply `volume` to the mixer.
fn write(volume: &Volume) -> bool {
    let Some(audio) = Audio::try_connect() else {
        return false;
    };
    MixerControl::new(audio)
        .set_master(volume.gain, volume.mute)
        .is_ok()
}

/// Show `volume` on the item.
fn show(res: &Resident, volume: &Volume) {
    let patch = wire::UpdateArgs {
        tooltip: Some(volume.tooltip()),
        status: Some(volume.status()),
        menu: Some(wire::Menu {
            rows: volume.menu(),
        }),
        ..wire::UpdateArgs::default()
    };
    let _ = res.tray.borrow_mut().update(patch);
}

/// A tray event; `true` when the volume changed.
fn event(volume: &mut Volume, event: Event) -> bool {
    if !volume.card {
        return false;
    }
    match event {
        Event::Scroll { delta } => {
            let step = STEP.saturating_mul(delta.unsigned_abs());
            volume.gain = if delta > 0 {
                volume.gain.saturating_add(step).min(UNITY_GAIN)
            } else {
                volume.gain.saturating_sub(step)
            };
            if write(volume) {
                println!("VOLUME:LEVEL:{}", volume.percent());
            }
            true
        }
        Event::Activate { .. } | Event::MenuItem { id: ROW_MUTE, .. } => {
            volume.mute = !volume.mute;
            if write(volume) {
                println!("VOLUME:MUTE:{}", volume.mute);
            }
            true
        }
        _ => false,
    }
}

fn main() {
    let res = Resident::windowless("VOLUME");
    let mut volume = read().unwrap_or(Volume {
        gain: UNITY_GAIN,
        mute: false,
        card: false,
    });
    if !volume.card {
        println!("VOLUME:NOCARD");
    }
    let mut first = item(lucide("volume-2"), &volume.tooltip());
    first.status = volume.status();
    first.menu = volume.menu();
    // Before the shell serves the tray the item is set when it announces
    // itself (`xui_app::tray`), so a refusal here is not fatal.
    if let Err(code) = res.tray.borrow_mut().set(first) {
        println!("VOLUME:TRAY:LATER err={}", -code);
    }
    println!(
        "VOLUME:UP:PASS level={} mute={}",
        volume.percent(),
        volume.mute
    );
    loop {
        for wake in res.idle(500) {
            match wake {
                Wake::Quit(_) => {
                    let _ = res.tray.borrow_mut().clear();
                    println!("VOLUME:QUIT:PASS");
                    std::process::exit(0);
                }
                // There is no window to show: the icon is the whole app.
                Wake::Reopen(_) => {}
                Wake::Tray(tray) => {
                    if event(&mut volume, tray) {
                        show(&res, &volume);
                    }
                }
            }
        }
    }
}
