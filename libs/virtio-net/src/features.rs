//! Feature bits (virtio 1.2, 5.1.3) and the sets this driver asks for.

pub const CSUM: u64 = 1 << 0;
pub const GUEST_CSUM: u64 = 1 << 1;
pub const CTRL_GUEST_OFFLOADS: u64 = 1 << 2;
pub const MTU: u64 = 1 << 3;
pub const MAC: u64 = 1 << 5;
pub const GUEST_TSO4: u64 = 1 << 7;
pub const GUEST_TSO6: u64 = 1 << 8;
pub const GUEST_ECN: u64 = 1 << 9;
pub const GUEST_UFO: u64 = 1 << 10;
pub const HOST_TSO4: u64 = 1 << 11;
pub const HOST_TSO6: u64 = 1 << 12;
pub const HOST_ECN: u64 = 1 << 13;
pub const HOST_UFO: u64 = 1 << 14;
pub const MRG_RXBUF: u64 = 1 << 15;
pub const STATUS: u64 = 1 << 16;
pub const CTRL_VQ: u64 = 1 << 17;
pub const CTRL_RX: u64 = 1 << 18;
pub const CTRL_VLAN: u64 = 1 << 19;
pub const GUEST_ANNOUNCE: u64 = 1 << 21;
pub const MQ: u64 = 1 << 22;
pub const CTRL_MAC_ADDR: u64 = 1 << 23;
pub const SPEED_DUPLEX: u64 = 1 << 63;

/// Bits the driver requires beyond `VERSION_1` (which the transport always
/// requires): none. A device without a MAC is refused later, by
/// [`crate::config::NetConfig::usable_mac`], so the reason is reported.
pub const REQUIRED: u64 = 0;

/// Bits the driver takes when offered. No checksum or segmentation offload, no
/// merged receive buffers and no control queue: every frame is one complete
/// buffer and the device does no work the stack has not seen
/// (`docs/networking-plan.md` section 5, step 2).
pub const WANTED: u64 = MAC | STATUS;

/// Bits that would change the packet header or buffer layout, or make the
/// device produce frames the driver does not expect. None must ever be
/// accepted; a test pins this.
pub const FORBIDDEN: u64 = CSUM
    | GUEST_CSUM
    | CTRL_GUEST_OFFLOADS
    | GUEST_TSO4
    | GUEST_TSO6
    | GUEST_ECN
    | GUEST_UFO
    | HOST_TSO4
    | HOST_TSO6
    | HOST_ECN
    | HOST_UFO
    | MRG_RXBUF
    | CTRL_VQ
    | CTRL_RX
    | CTRL_VLAN
    | MQ
    | CTRL_MAC_ADDR;

/// The `wanted` bits the driver takes out of what `offered` lists.
pub const fn accept(offered: u64) -> u64 {
    offered & WANTED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_positions_match_the_spec() {
        assert_eq!(MAC, 0x20);
        assert_eq!(STATUS, 0x1_0000);
        assert_eq!(MRG_RXBUF, 0x8000);
        assert_eq!(MQ, 0x40_0000);
        assert_eq!(SPEED_DUPLEX, 1 << 63);
    }

    #[test]
    fn only_mac_and_status_are_taken() {
        // A device offering everything still gets exactly what was asked for.
        assert_eq!(accept(u64::MAX), MAC | STATUS);
        assert_eq!(accept(0), 0);
        assert_eq!(accept(MAC | MRG_RXBUF | CSUM), MAC);
    }

    #[test]
    fn nothing_that_changes_the_layout_is_wanted() {
        assert_eq!(WANTED & FORBIDDEN, 0);
        assert_eq!(accept(u64::MAX) & FORBIDDEN, 0);
        // Bits 0..=31 the driver does not know are never taken either.
        assert_eq!(accept(0xFFFF_FFFF) & !(MAC | STATUS), 0);
    }
}
