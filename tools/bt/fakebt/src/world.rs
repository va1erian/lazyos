//! The controller `fakebt` serves: `libs/btsim`'s simulator with the harness's
//! HID peers, a script of what they do and when, and their bond state kept in
//! a file so a second QEMU run finds the same paired devices (a reboot test
//! is real only if the peers remember their keys).
//!
//! The wrapper implements `Controller` itself, so the USB side stays generic:
//! every call first runs the script steps that have come due, then lets the
//! simulator act.

use std::fs;
use std::path::PathBuf;

use bthci::BdAddr;
use btsim::{
    BrPeer, Controller, HciIn, LePeer, Part, PasskeyBehavior, PeerId, RadioFaults, ReconnectStyle,
    SimConfig, SimController,
};

use crate::script::{Action, Base, Step};

/// What the harness asked for.
pub struct Options {
    pub address: [u8; 6],
    pub keyboard: Option<[u8; 6]>,
    pub mouse: Option<[u8; 6]>,
    /// A peer without a stored bond is in pairing mode (the user holds the
    /// pairing button).
    pub pairable: bool,
    /// A peer without a stored bond holds a key the host never saw (a
    /// stranger that was paired to another machine): it pages the host
    /// when touched, and the host must not let it type.
    pub forged_bond: bool,
    pub mouse_waits_to_be_paged: bool,
    pub passkey: PasskeyBehavior,
    pub state: Option<PathBuf>,
    pub steps: Vec<Step>,
    pub le: LeOptions,
    /// A vendor radio that needs firmware (`--radio`), instead of a generic
    /// controller, and what goes wrong with it.
    pub radio: Option<(Part, RadioFaults)>,
}

/// The LE peers (docs/bluetooth-plan.md B4). An LE keyboard or mouse takes
/// the place of the BR/EDR one in the same script slot.
#[derive(Default)]
pub struct LeOptions {
    /// The LE keyboard's owner types the passkey this long after the host
    /// shows it (0: at once), so a screenshot can read the dialog.
    pub passkey_delay_ms: u64,
    pub keyboard: Option<[u8; 6]>,
    pub mouse: Option<[u8; 6]>,
    /// The keyboard says it has no input: Just Works, which the host must
    /// refuse for a keyboard.
    pub keyboard_just_works: bool,
    /// Bonded devices advertise with a fresh resolvable private address.
    pub rpa: bool,
    /// An extra keyboard that holds a key the host never issued and types.
    pub intruder: Option<[u8; 6]>,
    /// The peers send a notification before the link is encrypted.
    pub early_notify: bool,
}

type Bond = Option<([u8; 16], u8)>;

pub struct World {
    sim: SimController,
    keyboard: Option<PeerId>,
    mouse: Option<PeerId>,
    intruder: Option<PeerId>,
    steps: Vec<Step>,
    /// When page scan went on, each time (the host is armed).
    armed: Vec<u64>,
    scanning: bool,
    /// When each peer's HID channels became open, each time: keyboard, mouse.
    hid_opens: [Vec<u64>; 2],
    hid_open: [bool; 2],
    state: Option<PathBuf>,
    saved: Vec<(String, Bond)>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut key = [0u8; 16];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(key)
}

/// The stored bonds: lines of `ADDRESS KEY-HEX KEY-TYPE`.
fn read_state(path: &PathBuf) -> Vec<(String, Bond)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (addr, key, kind) = (parts.next()?, parts.next()?, parts.next()?);
            Some((addr.to_string(), Some((unhex(key)?, kind.parse().ok()?))))
        })
        .collect()
}

fn address_text(addr: BdAddr) -> String {
    addr.to_string()
}

impl World {
    pub fn new(options: Options) -> World {
        let mut sim = SimController::new(SimConfig {
            bd_addr: BdAddr::from_msb(options.address),
            passkey_delay_ms: options.le.passkey_delay_ms,
            ..SimConfig::default()
        });
        if let Some((part, faults)) = options.radio {
            sim.set_radio(part, faults);
        }
        let stored = options.state.as_ref().map(read_state).unwrap_or_default();
        let add = |sim: &mut SimController, mut peer: BrPeer, waits: bool| {
            let remembered = stored
                .iter()
                .find(|(addr, _)| *addr == address_text(peer.addr))
                .and_then(|(_, bond)| *bond);
            if waits {
                peer.reconnect = ReconnectStyle::HostMustPage;
            }
            peer.passkey = options.passkey;
            match remembered {
                Some(bond) => peer.link_key = Some(bond),
                None if options.pairable => peer.set_pairing_mode(true),
                None if options.forged_bond => peer.link_key = Some(([0x5A; 16], 5)),
                None => {}
            }
            sim.add_br_peer(peer)
        };
        let le = &options.le;
        let add_le = |sim: &mut SimController, mut peer: LePeer, forged: bool| {
            let remembered = stored
                .iter()
                .find(|(addr, _)| *addr == address_text(peer.addr))
                .and_then(|(_, bond)| *bond);
            peer.passkey = options.passkey;
            peer.use_rpa = le.rpa;
            peer.early_notify = le.early_notify;
            let id = sim.add_le_peer(peer);
            match remembered {
                Some(bond) => sim.set_bond(id, Some(bond)),
                None if forged => sim.set_bond(id, Some(([0x5A; 16], 1))),
                None if options.pairable => sim.enter_pairing_mode(id, 0),
                None => {}
            }
            id
        };
        let keyboard = match le.keyboard {
            Some(a) => {
                let mut peer = LePeer::keyboard(a);
                if le.keyboard_just_works {
                    peer.io = btsim::IoCapability::NoInputNoOutput;
                }
                Some(add_le(&mut sim, peer, false))
            }
            None => options
                .keyboard
                .map(|a| add(&mut sim, BrPeer::keyboard(a), false)),
        };
        let mouse = match le.mouse {
            Some(a) => Some(add_le(&mut sim, LePeer::mouse(a), false)),
            None => options
                .mouse
                .map(|a| add(&mut sim, BrPeer::mouse(a), options.mouse_waits_to_be_paged)),
        };
        let intruder = le
            .intruder
            .map(|a| add_le(&mut sim, LePeer::keyboard(a), true));
        let mut world = World {
            sim,
            keyboard,
            mouse,
            intruder,
            steps: options.steps,
            armed: Vec::new(),
            scanning: false,
            hid_opens: [Vec::new(), Vec::new()],
            hid_open: [false; 2],
            state: options.state,
            saved: Vec::new(),
        };
        world.saved = world.snapshot();
        world
    }

    fn snapshot(&self) -> Vec<(String, Bond)> {
        [self.keyboard, self.mouse]
            .into_iter()
            .flatten()
            .map(|id| (address_text(self.sim.peer_addr(id)), self.sim.bond_of(id)))
            .collect()
    }

    /// Write the peers' keys when they change (a pairing just finished).
    fn persist(&mut self) {
        let now = self.snapshot();
        if now == self.saved {
            return;
        }
        self.saved = now;
        let Some(path) = &self.state else { return };
        let text: String = self
            .saved
            .iter()
            .filter_map(|(addr, bond)| {
                bond.map(|(key, kind)| format!("{addr} {} {kind}\n", hex(&key)))
            })
            .collect();
        let tmp = path.with_extension("new");
        if fs::write(&tmp, text)
            .and_then(|()| fs::rename(&tmp, path))
            .is_err()
        {
            eprintln!("fakebt: cannot write {}", path.display());
        } else {
            println!(
                "fakebt: peer bonds saved ({} keys)",
                self.saved.iter().filter(|(_, b)| b.is_some()).count()
            );
        }
    }

    /// Note the moments the peers' HID channels became open.
    fn note_hid(&mut self, now: u64) {
        for (slot, peer) in [self.keyboard, self.mouse].into_iter().enumerate() {
            let open = peer.is_some_and(|id| self.sim.input_ready(id));
            if open && !self.hid_open[slot] {
                self.hid_opens[slot].push(now);
                println!(
                    "fakebt: peer {slot} HID channels open #{} at {now} ms",
                    self.hid_opens[slot].len()
                );
            }
            self.hid_open[slot] = open;
        }
    }

    fn note_scan(&mut self, now: u64) {
        let on = self.sim.scan_enable() & 2 != 0;
        if on && !self.scanning {
            self.armed.push(now);
            println!(
                "fakebt: host armed (page scan on) #{} at {now} ms",
                self.armed.len()
            );
        }
        self.scanning = on;
    }

    fn base_ms(&self, base: Base) -> Option<u64> {
        match base {
            Base::Connect => Some(0),
            Base::Armed(n) => self.armed.get(n.checked_sub(1)?).copied(),
            Base::Keyboard(n) => self.hid_opens[0].get(n.checked_sub(1)?).copied(),
            Base::Mouse(n) => self.hid_opens[1].get(n.checked_sub(1)?).copied(),
        }
    }

    /// Run the steps that have come due.
    fn run_script(&mut self, now: u64) {
        for i in 0..self.steps.len() {
            let step = &self.steps[i];
            let due = !step.fired
                && self
                    .base_ms(step.base)
                    .is_some_and(|base| now >= base.saturating_add(step.at_ms));
            if due {
                self.steps[i].fired = true;
                let action = self.steps[i].action.clone();
                self.perform(now, &action);
            }
        }
    }

    fn perform(&mut self, now: u64, action: &Action) {
        match (action, self.keyboard, self.mouse) {
            (Action::Key(usage), Some(kbd), _) => self.sim.press_key(kbd, now, *usage),
            (Action::Hold(usage), Some(kbd), _) => self.sim.key_report(kbd, now, *usage),
            (Action::Release, Some(kbd), _) => self.sim.key_report(kbd, now, 0),
            (Action::IntruderKey(usage), _, _) if self.intruder.is_some() => {
                if let Some(id) = self.intruder {
                    self.sim.press_key(id, now, *usage);
                }
            }
            (Action::Wake, kbd, mouse) => {
                for peer in [kbd, mouse].into_iter().flatten() {
                    self.sim.wake(peer, now);
                }
            }
            (Action::Move { dx, dy, buttons }, _, Some(mouse)) => {
                self.sim.move_mouse(mouse, now, *buttons, *dx, *dy)
            }
            (Action::DropKeyboard(reason), Some(kbd), _) => self.sim.drop_link(kbd, now, *reason),
            (Action::DropMouse(reason), _, Some(mouse)) => self.sim.drop_link(mouse, now, *reason),
            _ => eprintln!("fakebt: no peer for {action:?}"),
        }
        println!("fakebt: {action:?} at {now} ms");
    }
}

impl Controller for World {
    fn command(&mut self, now_ms: u64, packet: &[u8]) {
        self.run_script(now_ms);
        self.sim.command(now_ms, packet);
        self.note_scan(now_ms);
        self.persist();
    }

    fn acl_out(&mut self, now_ms: u64, packet: &[u8]) {
        self.run_script(now_ms);
        self.sim.acl_out(now_ms, packet);
        self.note_hid(now_ms);
        self.persist();
    }

    fn poll_in(&mut self, now_ms: u64) -> Option<HciIn> {
        self.run_script(now_ms);
        let packet = self.sim.poll_in(now_ms);
        self.note_scan(now_ms);
        self.note_hid(now_ms);
        self.persist();
        packet
    }

    fn take_detach(&mut self, now_ms: u64) -> Option<u64> {
        self.sim.take_detach(now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KBD: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x01];

    fn options(state: PathBuf, pairable: bool) -> Options {
        Options {
            address: [0x00, 0x1A, 0x7D, 0xDA, 0x71, 0x13],
            keyboard: Some(KBD),
            mouse: None,
            pairable,
            forged_bond: false,
            mouse_waits_to_be_paged: false,
            passkey: PasskeyBehavior::Correct,
            state: Some(state),
            steps: Vec::new(),
            le: LeOptions::default(),
            radio: None,
        }
    }

    #[test]
    fn a_radio_world_answers_its_own_version_and_detaches_on_recovery() {
        use bthci::Command;
        let mut opts = options(temp("radio"), false);
        opts.radio = Some((
            Part::IntelAx201,
            RadioFaults {
                reject_downloads: 1,
                ..RadioFaults::default()
            },
        ));
        let mut world = World::new(opts);
        // Read_Version in the bootloader: the legacy record, fw_variant 6.
        world.command(
            0,
            &Command::Unknown {
                opcode: 0xfc05,
                params: Vec::new(),
            }
            .encode(),
        );
        let Some(HciIn::Event(event)) = world.poll_in(5) else {
            panic!("no answer");
        };
        assert_eq!(&event[5..], &[0, 0x37, 0x13, 0, 0x06, 4, 0, 0, 0, 0]);
        // The recovery reset leaves the bus, and only then.
        assert_eq!(world.take_detach(6), None);
        world.command(
            10,
            &Command::Unknown {
                opcode: 0xfc01,
                params: vec![1, 1, 1, 0, 0, 0, 0, 0],
            }
            .encode(),
        );
        assert!(world.take_detach(11).is_some());
    }

    fn temp(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("fakebt-{}-{name}", std::process::id()));
        let _ = fs::remove_file(&path);
        path
    }

    #[test]
    fn keys_survive_a_second_run() {
        let state = temp("state");
        // First run: pairable, no stored key.
        let mut world = World::new(options(state.clone(), true));
        let kbd = world.keyboard.unwrap();
        assert!(world.sim.br(kbd).pairing_mode);
        assert!(world.sim.br(kbd).link_key.is_none());
        // The pairing finishes: the key is written when it changes.
        world.sim.br_mut(kbd).link_key = Some(([7; 16], 5));
        world.persist();
        let text = fs::read_to_string(&state).unwrap();
        let addr = address_text(world.sim.br(kbd).addr);
        assert_eq!(text, format!("{addr} {} 5\n", hex(&[7; 16])));
        // Second run: the peer remembers, and is not in pairing mode (which
        // would drop the bond, as it does on a real device).
        let world = World::new(options(state.clone(), true));
        let kbd = world.keyboard.unwrap();
        assert_eq!(world.sim.br(kbd).link_key, Some(([7; 16], 5)));
        assert!(!world.sim.br(kbd).pairing_mode);
        let _ = fs::remove_file(&state);
    }

    #[test]
    fn a_damaged_state_file_is_ignored_line_by_line() {
        let state = temp("damaged");
        fs::write(
            &state,
            "garbage\nC0:FF:EE:00:00:01 zz 5\nC0:FF:EE:00:00:01 00 5\n",
        )
        .unwrap();
        assert!(read_state(&state).is_empty());
        let _ = fs::remove_file(&state);
        assert!(read_state(&state).is_empty(), "a missing file is no bonds");
    }

    #[test]
    fn a_forged_bond_is_a_key_the_host_never_issued() {
        let mut o = options(temp("forged"), false);
        o.forged_bond = true;
        let world = World::new(o);
        let kbd = world.keyboard.unwrap();
        assert!(world.sim.br(kbd).link_key.is_some());
        assert!(!world.sim.br(kbd).pairing_mode);
    }

    #[test]
    fn the_script_runs_once_after_its_trigger() {
        let mut o = options(temp("script"), false);
        o.forged_bond = true;
        o.steps = crate::script::parse("armed1+100:key=0x04").unwrap();
        let mut world = World::new(o);
        world.run_script(1_000);
        assert!(!world.steps[0].fired, "page scan never went on");
        world.armed.push(1_000);
        world.run_script(1_050);
        assert!(!world.steps[0].fired, "not due yet");
        world.run_script(1_100);
        assert!(world.steps[0].fired);
    }
}
