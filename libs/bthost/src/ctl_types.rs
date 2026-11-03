//! The types the control interface (`os.lazy.bt.v1`, docs/bluetooth-plan.md
//! B5) is made of, as the host sees them. The shell maps them to the wire.
//! None of them can hold a key.

use alloc::string::String;

use bthci::BdAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    BrEdr,
    Le,
}

/// What a device types with, as far as it is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Unknown,
    Keyboard,
    Pointer,
}

/// One device in the list: a bond, a scan result, or both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub addr: BdAddr,
    pub name: String,
    pub transport: Transport,
    pub input: InputKind,
    pub rssi: Option<i8>,
    pub bonded: bool,
    /// Registered as an input source.
    pub connected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceChange {
    Found,
    Changed,
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Connecting,
    ShowPasskey,
    ConfirmNumber,
    Paired,
    Failed,
}

impl Phase {
    /// A pairing is under way (not idle, not over).
    pub fn active(self) -> bool {
        matches!(
            self,
            Phase::Connecting | Phase::ShowPasskey | Phase::ConfirmNumber
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailReason {
    None,
    Timeout,
    Cancelled,
    Rejected,
    AuthFailed,
    Unsupported,
    LinkLost,
}

/// The pairing in progress, or the last one's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairingState {
    pub phase: Phase,
    pub addr: Option<BdAddr>,
    /// Phase `ShowPasskey`.
    pub passkey: u32,
    /// Phase `ConfirmNumber`.
    pub number: u32,
    pub reason: FailReason,
}

impl PairingState {
    pub const IDLE: PairingState = PairingState {
        phase: Phase::Idle,
        addr: None,
        passkey: 0,
        number: 0,
        reason: FailReason::None,
    };
}

/// Why a control call was refused (the shell turns these into errnos).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlError {
    /// No controller is up.
    NoController,
    /// A pairing is running.
    Busy,
    /// No such device.
    NotFound,
    /// Not the device being paired, or nothing to answer.
    Invalid,
}
