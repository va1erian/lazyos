//! The one ARP exchange the driver's self-test performs: "who has `target`?"
//! out, the answer back. It proves transmit and receive through the real
//! device and shows up in the packet capture. It is test traffic; the driver
//! never parses a received frame for any other purpose.

use virtio_net::ETH_HEADER;

/// Bytes of an Ethernet + ARP (IPv4) frame.
pub const ARP_FRAME: usize = ETH_HEADER + 28;

const ETHERTYPE_ARP: [u8; 2] = [0x08, 0x06];

/// A broadcast ARP request: `sender_mac`/`sender_ip` asking who has `target_ip`.
pub fn request(sender_mac: [u8; 6], sender_ip: [u8; 4], target_ip: [u8; 4]) -> [u8; ARP_FRAME] {
    let mut f = [0u8; ARP_FRAME];
    f[0..6].copy_from_slice(&[0xFF; 6]);
    f[6..12].copy_from_slice(&sender_mac);
    f[12..14].copy_from_slice(&ETHERTYPE_ARP);
    f[14..16].copy_from_slice(&[0, 1]); // hardware type: Ethernet
    f[16..18].copy_from_slice(&[0x08, 0x00]); // protocol type: IPv4
    f[18] = 6;
    f[19] = 4;
    f[20..22].copy_from_slice(&[0, 1]); // operation: request
    f[22..28].copy_from_slice(&sender_mac);
    f[28..32].copy_from_slice(&sender_ip);
    // target hardware address is zero in a request
    f[38..42].copy_from_slice(&target_ip);
    f
}

/// Whether `frame` is an ARP reply from `answering_ip` to `our_mac`, and if so
/// the MAC that answered.
pub fn reply_from(frame: &[u8], our_mac: [u8; 6], answering_ip: [u8; 4]) -> Option<[u8; 6]> {
    if frame.len() < ARP_FRAME || frame[0..6] != our_mac || frame[12..14] != ETHERTYPE_ARP {
        return None;
    }
    let is_ipv4_ethernet = frame[14..16] == [0, 1]
        && frame[16..18] == [0x08, 0x00]
        && frame[18] == 6
        && frame[19] == 4;
    if !is_ipv4_ethernet || frame[20..22] != [0, 2] || frame[28..32] != answering_ip {
        return None;
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&frame[22..28]);
    Some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];

    #[test]
    fn the_request_is_a_well_formed_42_byte_broadcast() {
        let f = request(MAC, [10, 0, 2, 15], [10, 0, 2, 2]);
        assert_eq!(f.len(), 42);
        assert_eq!(&f[..6], &[0xFF; 6]);
        assert_eq!(&f[6..12], &MAC);
        assert_eq!(&f[12..14], &[0x08, 0x06]);
        assert_eq!(&f[20..22], &[0, 1]);
        assert_eq!(&f[28..32], &[10, 0, 2, 15]);
        assert_eq!(&f[38..42], &[10, 0, 2, 2]);
    }

    fn reply(to: [u8; 6], from_ip: [u8; 4]) -> [u8; 42] {
        let mut f = request([0x52, 0x55, 10, 0, 2, 2], from_ip, [10, 0, 2, 15]);
        f[0..6].copy_from_slice(&to);
        f[21] = 2;
        f
    }

    #[test]
    fn a_reply_to_us_from_the_right_address_is_recognised() {
        let f = reply(MAC, [10, 0, 2, 2]);
        assert_eq!(
            reply_from(&f, MAC, [10, 0, 2, 2]),
            Some([0x52, 0x55, 10, 0, 2, 2])
        );
    }

    #[test]
    fn everything_else_is_not() {
        let good = reply(MAC, [10, 0, 2, 2]);
        assert_eq!(
            reply_from(&good, MAC, [10, 0, 2, 3]),
            None,
            "wrong answerer"
        );
        assert_eq!(reply_from(&good, [2; 6], [10, 0, 2, 2]), None, "not for us");
        assert_eq!(
            reply_from(&good[..41], MAC, [10, 0, 2, 2]),
            None,
            "truncated"
        );
        assert_eq!(reply_from(&[], MAC, [10, 0, 2, 2]), None);
        let mut request = good;
        request[21] = 1;
        assert_eq!(
            reply_from(&request, MAC, [10, 0, 2, 2]),
            None,
            "a request is not a reply"
        );
        let mut ip = good;
        ip[12] = 0x08;
        ip[13] = 0x00;
        assert_eq!(reply_from(&ip, MAC, [10, 0, 2, 2]), None, "not ARP");
    }
}
