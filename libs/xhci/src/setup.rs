//! USB control requests (USB 2.0 9.3, 9.4, 11.24; HID 1.11 7.2): the
//! setup packets `usbd` sends on endpoint 0, independent of the controller.

/// A USB control request (USB 2.0 9.3), the 8 bytes of a setup packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupPacket {
    /// Whether the data stage (if any) is device-to-host.
    pub fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }

    /// The 8 bytes on the wire (USB 2.0 Table 9-2, little-endian fields).
    pub fn immediate_bytes(&self) -> [u8; 8] {
        let [value_lo, value_hi] = self.value.to_le_bytes();
        let [index_lo, index_hi] = self.index.to_le_bytes();
        let [length_lo, length_hi] = self.length.to_le_bytes();
        [
            self.request_type,
            self.request,
            value_lo,
            value_hi,
            index_lo,
            index_hi,
            length_lo,
            length_hi,
        ]
    }

    pub(crate) fn immediate(&self) -> u64 {
        u64::from_le_bytes(self.immediate_bytes())
    }
}

/// Standard, HID and hub class requests `usbd` issues. A new device class
/// adds its requests here (a builder per request, no raw packets elsewhere).
pub mod request {
    use super::SetupPacket;

    pub const GET_DESCRIPTOR: u8 = 6;
    pub const SET_CONFIGURATION: u8 = 9;
    pub const SET_INTERFACE: u8 = 11;
    pub const HID_SET_IDLE: u8 = 0x0A;
    pub const HID_SET_PROTOCOL: u8 = 0x0B;

    /// GET_DESCRIPTOR(`kind`, `index`) for `length` bytes.
    pub fn get_descriptor(kind: u8, index: u8, length: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x80,
            request: GET_DESCRIPTOR,
            value: u16::from(kind) << 8 | u16::from(index),
            index: 0,
            length,
        }
    }

    /// GET_DESCRIPTOR(STRING `index`) in language `lang`, `length` bytes.
    pub fn get_string(index: u8, lang: u16, length: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x80,
            request: GET_DESCRIPTOR,
            value: 3 << 8 | u16::from(index),
            index: lang,
            length,
        }
    }

    /// GET_DESCRIPTOR(REPORT) of HID `interface`, for `length` bytes: the
    /// recipient is the interface, not the device (HID 1.11 7.1.1).
    pub fn get_report_descriptor(interface: u8, length: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x81,
            request: GET_DESCRIPTOR,
            value: 0x22 << 8,
            index: u16::from(interface),
            length,
        }
    }

    pub fn set_configuration(value: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0x00,
            request: SET_CONFIGURATION,
            value: u16::from(value),
            index: 0,
            length: 0,
        }
    }

    /// HID SET_PROTOCOL: 0 boot, 1 report.
    pub fn set_protocol(interface: u8, boot: bool) -> SetupPacket {
        SetupPacket {
            request_type: 0x21,
            request: HID_SET_PROTOCOL,
            value: u16::from(!boot),
            index: u16::from(interface),
            length: 0,
        }
    }

    /// HID SET_IDLE(0): report only on change.
    pub fn set_idle(interface: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0x21,
            request: HID_SET_IDLE,
            value: 0,
            index: u16::from(interface),
            length: 0,
        }
    }

    /// CLEAR_FEATURE(ENDPOINT_HALT) on endpoint `address` (USB 2.0 9.4.1):
    /// after a STALL, the device side of the halt.
    pub fn clear_endpoint_halt(address: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0x02,
            request: CLEAR_FEATURE,
            value: 0,
            index: u16::from(address),
            length: 0,
        }
    }

    /// SET_INTERFACE(`interface`, `alternate`): e.g. a hub's multi-TT setting.
    pub fn set_interface(interface: u8, alternate: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0x01,
            request: SET_INTERFACE,
            value: u16::from(alternate),
            index: u16::from(interface),
            length: 0,
        }
    }

    /// Hub class requests (USB 2.0 11.24.2, USB 3.2 10.16.2).
    pub const GET_STATUS: u8 = 0;
    pub const CLEAR_FEATURE: u8 = 1;
    pub const SET_FEATURE: u8 = 3;
    pub const SET_HUB_DEPTH: u8 = 12;
    /// Hub descriptor types: USB 2 and SuperSpeed.
    pub const HUB_DESCRIPTOR: u8 = 0x29;
    pub const SS_HUB_DESCRIPTOR: u8 = 0x2A;

    /// GET_DESCRIPTOR(HUB) of a hub, USB 2 (`0x29`) or SuperSpeed (`0x2A`).
    pub fn get_hub_descriptor(kind: u8, length: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0xA0,
            request: GET_DESCRIPTOR,
            value: u16::from(kind) << 8,
            index: 0,
            length,
        }
    }

    /// GET_STATUS of hub `port`: `wPortStatus` then `wPortChange`.
    pub fn get_port_status(port: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0xA3,
            request: GET_STATUS,
            value: 0,
            index: u16::from(port),
            length: 4,
        }
    }

    /// SET_FEATURE(`feature`) on hub `port` (power, reset, warm reset).
    pub fn set_port_feature(port: u8, feature: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x23,
            request: SET_FEATURE,
            value: feature,
            index: u16::from(port),
            length: 0,
        }
    }

    /// CLEAR_FEATURE(`feature`) on hub `port` (acknowledges a change bit).
    pub fn clear_port_feature(port: u8, feature: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x23,
            request: CLEAR_FEATURE,
            value: feature,
            index: u16::from(port),
            length: 0,
        }
    }

    /// SET_HUB_DEPTH (SuperSpeed hubs, before anything else): how many hubs
    /// sit between it and the root port (0 for a hub on a root port).
    pub fn set_hub_depth(depth: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0x20,
            request: SET_HUB_DEPTH,
            value: u16::from(depth),
            index: 0,
            length: 0,
        }
    }

    /// USB Audio Class requests (UAC 1.0 5.2, UAC 2.0 5.2): to an entity of
    /// the AudioControl interface, or (1.0 Sampling Frequency) to an
    /// endpoint. The payloads come from `usbaudio::request`.
    pub mod audio {
        use super::super::SetupPacket;

        /// UAC 1.0 SET_CUR and GET_CUR; UAC 2.0 CUR (both directions) has
        /// SET_CUR's number, and RANGE is 2.
        pub const SET_CUR: u8 = 0x01;
        pub const GET_CUR: u8 = 0x81;
        pub const CUR: u8 = 0x01;
        pub const RANGE: u8 = 0x02;
        /// Control selectors: 1.0 endpoint SAMPLING_FREQ_CONTROL, 2.0 clock
        /// CS_SAM_FREQ_CONTROL, feature unit FU_MUTE_CONTROL and 2.0
        /// CX_CLOCK_SELECTOR_CONTROL are all 1.
        pub const SAMPLING_FREQ: u8 = 0x01;
        pub const MUTE: u8 = 0x01;
        pub const SELECTOR: u8 = 0x01;

        fn entity(interface: u8, entity: u8) -> u16 {
            u16::from(entity) << 8 | u16::from(interface)
        }

        /// UAC 1.0: set endpoint `address`'s sampling frequency (3 bytes).
        pub fn uac1_set_rate(address: u8) -> SetupPacket {
            SetupPacket {
                request_type: 0x22,
                request: SET_CUR,
                value: u16::from(SAMPLING_FREQ) << 8,
                index: u16::from(address),
                length: 3,
            }
        }

        /// UAC 1.0: read endpoint `address`'s sampling frequency back.
        pub fn uac1_get_rate(address: u8) -> SetupPacket {
            SetupPacket {
                request_type: 0xA2,
                request: GET_CUR,
                value: u16::from(SAMPLING_FREQ) << 8,
                index: u16::from(address),
                length: 3,
            }
        }

        /// UAC 2.0: set clock source `clock`'s frequency (4 bytes).
        pub fn uac2_set_rate(interface: u8, clock: u8) -> SetupPacket {
            SetupPacket {
                request_type: 0x21,
                request: CUR,
                value: u16::from(SAMPLING_FREQ) << 8,
                index: entity(interface, clock),
                length: 4,
            }
        }

        /// UAC 2.0: read clock source `clock`'s frequency.
        pub fn uac2_get_rate(interface: u8, clock: u8) -> SetupPacket {
            SetupPacket {
                request_type: 0xA1,
                request: CUR,
                value: u16::from(SAMPLING_FREQ) << 8,
                index: entity(interface, clock),
                length: 4,
            }
        }

        /// UAC 2.0: `length` bytes of clock `clock`'s frequency ranges (2
        /// first, for the count; then the whole reply).
        pub fn uac2_rate_ranges(interface: u8, clock: u8, length: u16) -> SetupPacket {
            SetupPacket {
                request_type: 0xA1,
                request: RANGE,
                value: u16::from(SAMPLING_FREQ) << 8,
                index: entity(interface, clock),
                length,
            }
        }

        /// UAC 2.0: select an input (1 byte, 1-based) of clock selector `id`.
        pub fn uac2_select_clock(interface: u8, id: u8) -> SetupPacket {
            SetupPacket {
                request_type: 0x21,
                request: CUR,
                value: u16::from(SELECTOR) << 8,
                index: entity(interface, id),
                length: 1,
            }
        }

        /// Feature unit `unit`'s master mute (1 byte, both revisions: 0
        /// unmutes, 1 mutes).
        pub fn set_mute(interface: u8, unit: u8) -> SetupPacket {
            SetupPacket {
                request_type: 0x21,
                request: SET_CUR,
                value: u16::from(MUTE) << 8,
                index: entity(interface, unit),
                length: 1,
            }
        }
    }
}
