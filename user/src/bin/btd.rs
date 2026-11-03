//! `btd` (`/system/bin/btd`): the Bluetooth host service
//! (docs/bluetooth-plan.md). This is the B2 skeleton: it brings a controller
//! up through `usbd`'s transport and says what it found; the host stack
//! (`libs/bthci` and the rest) replaces the hand-encoded packets in phase B3.
//!
//! `init` starts it at boot as `_bt` (uid 916) holding only
//! `CAP_INPUT_SOURCE`: no device claim, no DMA, no read of the input bus. It
//! waits for `usbd` to serve `os.lazy.usb.bt` (an unplugged controller
//! withdraws the name, a plugged one registers it), attaches its rings, and
//! brings the controller up: `HCI_Reset`, `Read_Local_Version`,
//! `Read_BD_ADDR`, `Read_Local_Name`. Everything it receives comes from a radio a stranger can
//! reach, so every packet is a copy it parses with its lengths checked.
//!
//! Serial evidence: `BT:CRED`, `BT:WAIT` (no controller yet), `BT:LINK` (the
//! transport is attached), `BT:CTRL:PASS` (brought up: HCI and LMP versions,
//! manufacturer, the last two octets of the address, never the whole of
//! it, and the name) or `BT:CTRL:FAIL`, `BT:CTRL:GONE` (the controller was unplugged).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use btusb::{PACKET_COMMAND, PACKET_EVENT};
use framering::{FrameBuf, PushError};
use user::messenger::registry;
use user::messenger::usb_bt::{self as transport, wire, Attachment, Client};
use user::sys;
use user::trace::{self, Channel};

/// Ticks (100 Hz) between looks for a controller while none is bound.
const WAIT_TICKS: u64 = 100;
/// Ticks a command may take before the controller is called dead.
const COMMAND_TICKS: u64 = 300;
/// The longest park of the idle state; a doorbell wakes it earlier.
const IDLE_TICKS: u64 = 6000;

/// HCI opcodes (Core spec vol 4 part E, 7.3 and 7.4): the OGF in the top six
/// bits, the OCF below.
const OP_RESET: u16 = 0x0C03;
const OP_READ_LOCAL_VERSION: u16 = 0x1001;
const OP_READ_BD_ADDR: u16 = 0x1009;
const OP_READ_LOCAL_NAME: u16 = 0x0C14;

/// HCI event codes.
const EVT_COMMAND_COMPLETE: u8 = 0x0E;
const EVT_COMMAND_STATUS: u8 = 0x0F;

/// Why bring-up failed, for the `BT:CTRL:FAIL` line.
enum Failure {
    Timeout(u16),
    Status(u16, u8),
    Short(u16),
    Link(&'static str),
    Gone,
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Failure::Timeout(op) => write!(f, "opcode {op:#06x} timed out"),
            Failure::Status(op, status) => write!(f, "opcode {op:#06x} status {status:#04x}"),
            Failure::Short(op) => write!(f, "opcode {op:#06x} answered too short"),
            Failure::Link(what) => write!(f, "{what}"),
            Failure::Gone => f.write_str("the controller went away"),
        }
    }
}

/// The attached transport and the buffer events are copied into.
struct Link {
    client: Client,
    attachment: Attachment,
    frame: Box<FrameBuf>,
}

/// Whether `os.lazy.usb.bt` is registered by `usbd` (`_usb`, or root on a
/// kernel-spawned boot): the radio's packets are trusted to no other server.
fn served_by_usbd() -> bool {
    registry::list().is_ok_and(|entries| {
        entries.iter().any(|entry| {
            entry.name == transport::NAME
                && (entry.owner_uid == u64::from(usbpolicy::USB_UID) || entry.owner_uid == 0)
        })
    })
}

/// Wait for `usbd` to bind a controller, then attach the rings.
fn open() -> Link {
    let mut logged = false;
    loop {
        if served_by_usbd() {
            if let Ok(client) = Client::connect() {
                match client.attach(transport::SLOTS) {
                    Ok(attachment) => {
                        return Link {
                            client,
                            attachment,
                            frame: Box::new([0; framering::MAX_FRAME]),
                        };
                    }
                    Err(error) => {
                        trace::line!(Channel::Bt, "LINK:FAIL attach {}", error.message());
                        client.release();
                    }
                }
            }
        } else if !logged {
            trace::line!(Channel::Bt, "WAIT no Bluetooth controller yet");
            logged = true;
        }
        sys::wait(sys::clock() + WAIT_TICKS);
    }
}

impl Link {
    /// Queue one command and wake `usbd` if it sleeps on the ring.
    fn send(&mut self, opcode: u16, params: &[u8]) -> Result<(), Failure> {
        let mut packet = Vec::with_capacity(4 + params.len());
        packet.push(PACKET_COMMAND);
        packet.extend_from_slice(&opcode.to_le_bytes());
        packet.push(params.len() as u8);
        packet.extend_from_slice(params);
        match self.attachment.commands.push(&packet) {
            Ok(()) => {}
            Err(PushError::Full) => return Err(Failure::Link("the command ring is full")),
            Err(_) => return Err(Failure::Link("the command ring is broken")),
        }
        if self.attachment.commands.take_notify() {
            let _ = self.client.kick(self.attachment.ring);
        }
        Ok(())
    }

    /// The next event frame, copied into `self.frame`: `Some(len)`, or `None`
    /// when the ring is empty. An impossible slot is skipped.
    fn next_frame(&mut self) -> Result<Option<usize>, Failure> {
        loop {
            match self.attachment.events.pop(&mut self.frame) {
                Ok(frame) => return Ok(frame),
                Err(framering::PopError::BadLength(_)) => {}
                Err(framering::PopError::Corrupt) => {
                    return Err(Failure::Link("the event ring is broken"))
                }
            }
        }
    }

    /// Sleep until `usbd` has events (or the deadline): arm the ring, look
    /// once more, and only then wait for the doorbell. `Err(Gone)` when the
    /// controller was unplugged.
    fn sleep(&mut self, deadline: u64) -> Result<(), Failure> {
        self.attachment.events.arm();
        if matches!(self.attachment.events.pending(), Ok(n) if n > 0) {
            return Ok(());
        }
        match self.attachment.wait(deadline) {
            Ok(Some(bits)) if bits & (1 << wire::NOTIFY_BIT_GONE) != 0 => Err(Failure::Gone),
            Ok(_) => Ok(()),
            Err(_) => Err(Failure::Gone),
        }
    }

    /// Run one command and return the parameters after the status byte of its
    /// Command Complete event.
    fn command(&mut self, opcode: u16, params: &[u8]) -> Result<Vec<u8>, Failure> {
        self.send(opcode, params)?;
        let deadline = sys::clock() + COMMAND_TICKS;
        loop {
            while let Some(len) = self.next_frame()? {
                let frame = &self.frame[..len];
                match parse_answer(frame, opcode) {
                    Answer::Complete(status, rest) => {
                        return if status == 0 {
                            Ok(rest)
                        } else {
                            Err(Failure::Status(opcode, status))
                        };
                    }
                    Answer::Refused(status) => return Err(Failure::Status(opcode, status)),
                    Answer::Short => return Err(Failure::Short(opcode)),
                    Answer::Other => {}
                }
            }
            if sys::clock() >= deadline {
                return Err(Failure::Timeout(opcode));
            }
            self.sleep(deadline)?;
        }
    }

    /// `HCI_Reset`, then the version, the address and the name (the name's
    /// event is 254 bytes, many packets of the events endpoint: it proves
    /// `usbd` reassembles them).
    fn bring_up(&mut self) -> Result<(), Failure> {
        self.command(OP_RESET, &[])?;
        let version = self.command(OP_READ_LOCAL_VERSION, &[])?;
        let address = self.command(OP_READ_BD_ADDR, &[])?;
        let name = self.command(OP_READ_LOCAL_NAME, &[])?;
        // Read_Local_Version: HCI version, HCI revision (2), LMP version,
        // manufacturer (2), LMP subversion (2); Read_BD_ADDR: six octets,
        // least significant first.
        let [hci, _, _, lmp, m0, m1, ..] = version.as_slice() else {
            return Err(Failure::Short(OP_READ_LOCAL_VERSION));
        };
        let [a0, a1, _, _, _, _] = address.as_slice() else {
            return Err(Failure::Short(OP_READ_BD_ADDR));
        };
        // The name is a stranger's text: printable ASCII up to the first NUL,
        // cut to 32 characters, and last on the line.
        let name: alloc::string::String = name
            .iter()
            .take_while(|&&b| b != 0)
            .filter(|b| b.is_ascii_graphic() || **b == b' ')
            .take(32)
            .map(|&b| char::from(b))
            .collect();
        trace::pass!(
            Channel::Bt,
            "CTRL",
            "hci={hci} lmp={lmp} vendor={:#06x} addr=**:**:**:**:{a1:02X}:{a0:02X} name={name}",
            u16::from_le_bytes([*m0, *m1]),
        );
        Ok(())
    }

    /// After bring-up: swallow events until the controller goes away.
    fn idle(&mut self) {
        loop {
            match self.next_frame() {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(_) => return,
            }
            if self.sleep(sys::clock() + IDLE_TICKS).is_err() {
                return;
            }
        }
    }

    fn close(self) {
        let _ = self.client.detach(self.attachment.ring);
        self.attachment.close();
        self.client.release();
    }
}

/// What an event frame means for the command in flight.
enum Answer {
    /// Command Complete: the status and the return parameters after it.
    Complete(u8, Vec<u8>),
    /// Command Status with a non-zero status: the controller refused it.
    Refused(u8),
    /// An event for this command too short to read.
    Short,
    /// Any other event (or another command's).
    Other,
}

/// Read a frame (`PACKET_EVENT`, code, length, parameters) as an answer to
/// `opcode`. Every index is checked: the bytes are a stranger's.
fn parse_answer(frame: &[u8], opcode: u16) -> Answer {
    let [kind, code, plen, params @ ..] = frame else {
        return Answer::Other;
    };
    if *kind != PACKET_EVENT || params.len() != usize::from(*plen) {
        return Answer::Other;
    }
    match *code {
        EVT_COMMAND_COMPLETE => {
            // Num_HCI_Command_Packets, the opcode, then the return parameters
            // (the status first).
            let [_, lo, hi, status, rest @ ..] = params else {
                return if params.get(1..3) == Some(&opcode.to_le_bytes()) {
                    Answer::Short
                } else {
                    Answer::Other
                };
            };
            if u16::from_le_bytes([*lo, *hi]) != opcode {
                return Answer::Other;
            }
            Answer::Complete(*status, rest.to_vec())
        }
        EVT_COMMAND_STATUS => {
            // Status, Num_HCI_Command_Packets, the opcode.
            let [status, _, lo, hi] = params else {
                return Answer::Other;
            };
            if u16::from_le_bytes([*lo, *hi]) == opcode && *status != 0 {
                Answer::Refused(*status)
            } else {
                Answer::Other
            }
        }
        _ => Answer::Other,
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match sys::cred_get(None) {
        Ok(cred) => trace::line!(Channel::Bt, "CRED uid={} caps={:#x}", cred.uid, cred.caps),
        Err(errno) => trace::line!(Channel::Bt, "CRED unavailable (errno {errno})"),
    }
    loop {
        let mut link = open();
        trace::line!(Channel::Bt, "LINK ring={}", link.attachment.ring);
        match link.bring_up() {
            Ok(()) => link.idle(),
            Err(Failure::Gone) => {}
            Err(why) => {
                trace::fail!(Channel::Bt, "CTRL", "{why}");
                link.idle();
            }
        }
        trace::line!(Channel::Bt, "CTRL:GONE the controller was unplugged");
        link.close();
        // A controller that fails at once every time must not spin: wait for
        // `usbd` to withdraw it before looking again.
        while served_by_usbd() {
            sys::wait(sys::clock() + WAIT_TICKS);
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    trace::line!(Channel::Bt, "PANIC {info}");
    sys::exit(2)
}
