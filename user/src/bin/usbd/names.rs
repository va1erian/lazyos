//! Names for the steps of enumeration, so a timeout on a real controller says
//! which step stalled instead of a bare "event" (docs/compat/kabylake).

use xhci::trb::kind;

/// What a command TRB type is called in a timeout message.
pub(super) fn command_name(kind: u8) -> &'static str {
    match kind {
        kind::ENABLE_SLOT => "Enable Slot completion",
        kind::DISABLE_SLOT => "Disable Slot completion",
        kind::ADDRESS_DEVICE => "Address Device completion",
        kind::CONFIGURE_ENDPOINT => "Configure Endpoint completion",
        kind::EVALUATE_CONTEXT => "Evaluate Context completion",
        kind::RESET_ENDPOINT => "Reset Endpoint completion",
        kind::STOP_ENDPOINT => "Stop Endpoint completion",
        kind::SET_TR_DEQUEUE => "Set TR Dequeue completion",
        _ => "command completion",
    }
}

/// What a standard control request is called in a timeout message.
pub(super) fn request_name(request: u8) -> &'static str {
    match request {
        5 => "SET_ADDRESS transfer",
        6 => "GET_DESCRIPTOR transfer",
        9 => "SET_CONFIGURATION transfer",
        10 => "SET_IDLE transfer",
        11 => "SET_PROTOCOL transfer",
        _ => "control transfer",
    }
}
