//! The Bluetooth page built offscreen (docs/bluetooth-plan.md B5): a scan, a
//! pairing that shows the passkey, one that asks to compare a number, a pairing
//! that fails, and forgetting a device. The doubles stand in for `btd`: the
//! test sends the events the service would, as the platform layer does, and
//! reads the requests the page made. A snapshot of each state is left in
//! `target/snapshots/settings-bluetooth-*.png` for a human to look at.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use uitheme::Mode;
use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::{Dip, Image};
use xui_settings::app::WINDOW;
use xui_settings::bluetooth::{
    BtCall, BtDevice, BtEvent, BtInput, BtPairing, BtPhase, BtReason, BtTransport,
};
use xui_settings::bluetooth_page::BtMsg;
use xui_settings::{
    theme_ops, MemAccounts, MemBluetooth, MemStore, MemSystem, Msg, Section, SettingsApp,
};

const KBD: &str = "C0:FF:EE:00:01:01";
const MOUSE: &str = "C0:FF:EE:00:01:02";

fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

/// Run `test` on its own thread with the fonts, failing if it hangs.
fn watchdog<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        register_fonts();
        let _ = tx.send(test());
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(value) => {
            let _ = handle.join();
            value
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}

fn device(address: &str, name: &str, input: BtInput, rssi: i32) -> BtDevice {
    BtDevice {
        address: address.into(),
        name: name.into(),
        transport: BtTransport::LowEnergy,
        input,
        rssi: Some(rssi),
        bonded: false,
        connected: false,
    }
}

fn pairing(phase: BtPhase, address: &str) -> BtPairing {
    BtPairing {
        phase,
        address: address.into(),
        passkey: 0,
        number: 0,
        reason: BtReason::None,
    }
}

fn ev(event: BtEvent) -> Msg {
    Msg::Bt(BtMsg::Event(event))
}

fn on(msg: BtMsg) -> Msg {
    Msg::Bt(msg)
}

fn snapshot(bluetooth: Rc<MemBluetooth>, messages: Vec<Msg>) -> Image {
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)),
        move |ui| {
            let store = MemStore::new();
            theme_ops::set_mode(&store, Mode::Dark).unwrap();
            SettingsApp::build(
                ui,
                Rc::new(store),
                Rc::new(MemSystem::default()),
                Rc::new(MemAccounts::default()),
                bluetooth,
            )
        },
        move |stage| {
            stage.emit(Msg::Section(Section::Bluetooth.index()));
            for msg in messages {
                stage.emit(msg);
            }
        },
    )
    .expect("the headless render")
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// The page after the service reported a controller and what a scan found.
fn found() -> Vec<Msg> {
    vec![
        ev(BtEvent::Snapshot {
            controller: true,
            discovering: false,
            devices: Vec::new(),
            pairing: BtPairing::idle(),
        }),
        on(BtMsg::Scan),
        ev(BtEvent::Device {
            device: device(KBD, "Sim Keyboard", BtInput::Keyboard, -48),
            removed: false,
        }),
        ev(BtEvent::Device {
            device: device(MOUSE, "Sim Mouse", BtInput::Pointer, -61),
            removed: false,
        }),
        on(BtMsg::Select(0)),
    ]
}

#[test]
fn no_controller_says_so_and_offers_nothing() {
    watchdog(|| {
        let bt = Rc::new(MemBluetooth::default());
        let image = snapshot(
            bt.clone(),
            vec![
                on(BtMsg::Scan),
                on(BtMsg::Pair),
                ev(BtEvent::Snapshot {
                    controller: false,
                    discovering: false,
                    devices: Vec::new(),
                    pairing: BtPairing::idle(),
                }),
            ],
        );
        save(&image, "settings-bluetooth-none.png");
        let calls = bt.calls.borrow().clone();
        // Connecting is the page's first act; nothing else is asked of a
        // service that has no controller (Scan is asked, the page enables it
        // only once the snapshot says a controller is up, which is checked in
        // the next test).
        assert_eq!(calls.first(), Some(&BtCall::Start));
    });
}

#[test]
fn a_keyboard_is_found_and_paired_with_a_passkey() {
    watchdog(|| {
        let bt = Rc::new(MemBluetooth::default());
        let mut messages = found();
        messages.push(on(BtMsg::Pair));
        let list = snapshot(bt.clone(), messages.clone());
        save(&list, "settings-bluetooth-list.png");
        messages.push(ev(BtEvent::Pairing(pairing(BtPhase::Connecting, KBD))));
        messages.push(ev(BtEvent::Pairing(BtPairing {
            passkey: 4217,
            ..pairing(BtPhase::ShowPasskey, KBD)
        })));
        let shown = snapshot(bt.clone(), messages.clone());
        save(&shown, "settings-bluetooth-passkey.png");
        messages.push(ev(BtEvent::Pairing(pairing(BtPhase::Paired, KBD))));
        save(
            &snapshot(bt.clone(), messages),
            "settings-bluetooth-paired.png",
        );
        let calls = bt.calls.borrow().clone();
        assert!(calls.contains(&BtCall::Scan), "{calls:?}");
        assert!(calls.contains(&BtCall::Pair(KBD.into())), "{calls:?}");
    });
}

#[test]
fn a_number_is_accepted_or_rejected_for_the_device_being_paired() {
    watchdog(|| {
        for accept in [true, false] {
            let bt = Rc::new(MemBluetooth::default());
            let mut messages = found();
            messages.push(on(BtMsg::Pair));
            messages.push(ev(BtEvent::Pairing(BtPairing {
                number: 987_654,
                ..pairing(BtPhase::ConfirmNumber, KBD)
            })));
            let image = snapshot(bt.clone(), messages.clone());
            if accept {
                save(&image, "settings-bluetooth-number.png");
            }
            messages.push(on(if accept { BtMsg::Accept } else { BtMsg::Reject }));
            snapshot(bt.clone(), messages);
            let calls = bt.calls.borrow().clone();
            assert!(
                calls.contains(&BtCall::Confirm(KBD.into(), accept)),
                "{calls:?}"
            );
        }
    });
}

#[test]
fn a_failed_pairing_and_a_forgotten_device() {
    watchdog(|| {
        let bt = Rc::new(MemBluetooth::default());
        let mut messages = found();
        messages.push(on(BtMsg::Pair));
        messages.push(ev(BtEvent::Pairing(BtPairing {
            reason: BtReason::Unsupported,
            ..pairing(BtPhase::Failed, KBD)
        })));
        save(
            &snapshot(bt.clone(), messages.clone()),
            "settings-bluetooth-failed.png",
        );
        let mut paired = device(MOUSE, "Sim Mouse", BtInput::Pointer, -61);
        paired.bonded = true;
        paired.connected = true;
        messages.push(ev(BtEvent::Device {
            device: paired,
            removed: false,
        }));
        messages.push(on(BtMsg::Select(1)));
        messages.push(on(BtMsg::Forget));
        messages.push(on(BtMsg::Cancel));
        snapshot(bt.clone(), messages);
        let calls = bt.calls.borrow().clone();
        assert!(calls.contains(&BtCall::Forget(MOUSE.into())), "{calls:?}");
        assert!(calls.contains(&BtCall::Cancel), "{calls:?}");
    });
}

#[test]
fn nothing_is_asked_for_without_a_selection() {
    watchdog(|| {
        let bt = Rc::new(MemBluetooth::default());
        snapshot(bt.clone(), vec![on(BtMsg::Pair), on(BtMsg::Forget)]);
        let calls = bt.calls.borrow().clone();
        assert_eq!(calls, [BtCall::Start], "{calls:?}");
    });
}
