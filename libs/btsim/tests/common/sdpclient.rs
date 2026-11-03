//! The host's SDP client: ask for the HID record with a service search
//! attribute request, follow continuations, and pull the report descriptor
//! out of the HIDDescriptorList.

use super::{Host, PSM_CONTROL};

impl Host {
    pub(super) fn sdp_request(&mut self, handle: u16, continuation: Option<u16>) {
        let Some(link) = self.links.iter_mut().find(|l| l.handle == handle) else {
            return;
        };
        let Some(cid) = link.sdp else { return };
        link.tid = link.tid.wrapping_add(1);
        // Pattern: HID service class. Attributes: every one (range 0x0000-0xFFFF).
        let mut params = vec![0x35, 0x03, 0x19, 0x11, 0x24, 0x00, 0x80];
        params.extend_from_slice(&[0x35, 0x05, 0x0A, 0x00, 0x00, 0xFF, 0xFF]);
        match continuation {
            None => params.push(0),
            Some(off) => params.extend_from_slice(&[2, (off >> 8) as u8, off as u8]),
        }
        let mut pdu = vec![0x06];
        pdu.extend_from_slice(&link.tid.to_be_bytes());
        pdu.extend_from_slice(&(params.len() as u16).to_be_bytes());
        pdu.extend_from_slice(&params);
        self.sdp_requests += 1;
        self.send_on(handle, cid, &pdu);
    }

    pub(super) fn on_sdp_response(&mut self, handle: u16, cid: u16, pdu: &[u8]) {
        assert_eq!(
            pdu.first(),
            Some(&0x07),
            "a ServiceSearchAttributeResponse: {pdu:02x?}"
        );
        let count = usize::from(u16::from_be_bytes([pdu[5], pdu[6]]));
        let body = &pdu[7..7 + count];
        let cont = &pdu[7 + count..];
        let link = self
            .links
            .iter_mut()
            .find(|l| l.handle == handle)
            .expect("link");
        link.sdp_buf.extend_from_slice(body);
        if cont.len() == 3 && cont[0] == 2 {
            let off = u16::from_be_bytes([cont[1], cont[2]]);
            assert_eq!(
                usize::from(off),
                link.sdp_buf.len(),
                "continuation offset follows the data"
            );
            return self.sdp_request(handle, Some(off));
        }
        assert_eq!(cont, [0], "no continuation left");
        let buf = std::mem::take(&mut link.sdp_buf);
        self.descriptor = descriptor_in(&buf);
        let actions = self
            .l2
            .disconnect(self.now, handle, cid)
            .expect("sdp channel");
        self.apply(actions);
        if let Ok((_, actions)) = self.l2.connect(self.now, handle, PSM_CONTROL) {
            self.apply(actions);
        }
    }
}

/// Find `[0x08, 0x22, text(2-byte length), descriptor]` in the record.
pub fn descriptor_in(record: &[u8]) -> Option<Vec<u8>> {
    let at = record.windows(3).position(|w| w == [0x08, 0x22, 0x26])?;
    let len = usize::from(u16::from_be_bytes([
        *record.get(at + 3)?,
        *record.get(at + 4)?,
    ]));
    record.get(at + 5..at + 5 + len).map(<[u8]>::to_vec)
}
