//! usbredir wire constants (usbredirproto.h): packet types, capability
//! bits, header sizes and transfer statuses. Split out of `redir.rs`
//! (file-length limit).

pub(super) mod kind {
    pub const HELLO: u32 = 0;
    pub const DEVICE_CONNECT: u32 = 1;
    pub const DEVICE_DISCONNECT: u32 = 2;
    pub const RESET: u32 = 3;
    pub const INTERFACE_INFO: u32 = 4;
    pub const EP_INFO: u32 = 5;
    pub const SET_CONFIGURATION: u32 = 6;
    pub const GET_CONFIGURATION: u32 = 7;
    pub const CONFIGURATION_STATUS: u32 = 8;
    pub const SET_ALT_SETTING: u32 = 9;
    pub const GET_ALT_SETTING: u32 = 10;
    pub const ALT_SETTING_STATUS: u32 = 11;
    pub const START_INTERRUPT: u32 = 15;
    pub const STOP_INTERRUPT: u32 = 16;
    pub const INTERRUPT_STATUS: u32 = 17;
    pub const CANCEL_DATA_PACKET: u32 = 21;
    pub const CONTROL_PACKET: u32 = 100;
    pub const BULK_PACKET: u32 = 101;
    pub const INTERRUPT_PACKET: u32 = 103;
}

/// Capability bits (usbredirproto.h). QEMU's xHCI refuses a host that lacks
/// the wide forms ("usb-redir-host lacks capabilities needed for use with
/// XHCI"), so this side announces them and speaks them once both sides do:
/// 64-bit packet ids (a 16-byte header), 32-bit bulk lengths (the bulk header
/// grows by two bytes), the device version in `device_connect`, and the max
/// packet sizes in `ep_info`. Bulk streams are not announced: they would add
/// a stream count per endpoint to `ep_info`, and nothing here uses them.
pub(super) const CAP_CONNECT_DEVICE_VERSION: u32 = 1;
pub(super) const CAP_EP_INFO_MAX_PACKET_SIZE: u32 = 4;
pub(super) const CAP_64BITS_IDS: u32 = 5;
pub(super) const CAP_32BITS_BULK_LENGTH: u32 = 6;
pub(super) const OUR_CAPS: u32 = 1 << CAP_CONNECT_DEVICE_VERSION
    | 1 << CAP_EP_INFO_MAX_PACKET_SIZE
    | 1 << CAP_64BITS_IDS
    | 1 << CAP_32BITS_BULK_LENGTH;
/// The packet header before and after the 64-bit ids are negotiated.
pub(super) const HEADER_NARROW: usize = 12;
pub(super) const HEADER_WIDE: usize = 16;
/// The most a body may be: a request longer than that is a broken peer.
pub(super) const MAX_BODY: usize = 1 << 20;

pub(super) const STATUS_SUCCESS: u8 = 0;
pub(super) const STATUS_CANCELLED: u8 = 1;
pub(super) const STATUS_INVAL: u8 = 2;
pub(super) const STATUS_STALL: u8 = 4;
pub(super) const SPEED_HIGH: u8 = 2;
pub(super) const TYPE_INVALID: u8 = 255;
