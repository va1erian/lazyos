//! What the audio class needs from a device beyond `device.rs`: control
//! transfers with an OUT data stage (class SET_CUR requests), the product
//! string, and Configure Endpoint for endpoints that come and go with an
//! alternate setting (xHCI 4.6.6.1: drop the old alternate's endpoints and
//! add the new one's, then send SET_INTERFACE).

use alloc::string::String;
use user::trace;

use xhci::context::{EndpointContext, InputContext};
use xhci::trb::{self, request, SetupPacket};

use super::super::hc::Hc;
use super::super::Error;
use super::{Device, DATA, DATA_BYTES, INPUT};

/// Most characters of a product string kept (it names an output in menus).
const NAME_CHARS: usize = 48;
/// The language id product strings are read in (US English, what every
/// device that has strings supports).
const LANG_EN_US: u16 = 0x0409;

/// A configuration chain read short of its `total` bytes: cut it after the
/// last whole record and say so in its `wTotalLength`, so the parsers see a
/// consistent (if partial) chain. Returns the length to keep.
pub(super) fn truncate_chain(chain: &mut [u8], total: usize) -> usize {
    if chain.len() >= total || chain.len() < 4 {
        return chain.len().min(total);
    }
    let mut end = 0;
    while end < chain.len() {
        let len = usize::from(chain[end]);
        if len < 2 || end + len > chain.len() {
            break;
        }
        end += len;
    }
    let [lo, hi] = (end as u16).to_le_bytes();
    chain[2] = lo;
    chain[3] = hi;
    trace::line!(
        trace::Channel::Usbd,
        "DESC:TRUNCATED total={total} kept={end}"
    );
    end
}

impl Device {
    /// A control transfer with a host-to-device data stage carrying `data`.
    pub(in super::super) fn control_out_data(
        &mut self,
        hc: &mut Hc,
        setup: SetupPacket,
        data: &[u8],
    ) -> Result<(), Error> {
        if data.len() != usize::from(setup.length) || data.len() > DATA_BYTES || setup.is_in() {
            return Err(Error::Descriptor("request data length"));
        }
        self.mem.write(DATA, data);
        self.control(hc, &setup)
    }

    /// The device's product string, printable ASCII only, empty when it has
    /// none or will not say.
    pub(in super::super) fn product_name(&mut self, hc: &mut Hc) -> String {
        if self.product_string == 0 {
            return String::new();
        }
        let mut bytes = [0u8; 255];
        let setup = request::get_string(self.product_string, LANG_EN_US, bytes.len() as u16);
        if self.control_in(hc, setup, &mut bytes).is_err() {
            return String::new();
        }
        // bLength, bDescriptorType 3, then UTF-16LE code units.
        let len = usize::from(bytes[0]).min(bytes.len());
        if len < 2 || bytes[1] != 3 {
            return String::new();
        }
        bytes[2..len]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .filter_map(|unit| char::from_u32(u32::from(unit)))
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(NAME_CHARS)
            .collect::<String>()
            .trim()
            .into()
    }

    /// Configure Endpoint that drops `drop` and adds `add` (an endpoint in
    /// both is replaced). The slot context's entries cover every endpoint
    /// the device has open after it.
    pub(in super::super) fn reconfigure(
        &mut self,
        hc: &mut Hc,
        add: &[(u8, EndpointContext)],
        drop: &[u8],
    ) -> Result<(), Error> {
        let highest = self
            .contexts
            .iter()
            .map(|&(dci, _)| dci)
            .chain(add.iter().map(|&(dci, _)| dci))
            .max()
            .unwrap_or(1);
        let slot = self.at.slot_context(highest, None);
        let context_64 = hc.info.context_64;
        let stride = if context_64 { 16 } else { 8 };
        let words = self
            .mem
            .dwords(INPUT, xhci::context::INPUT_CONTEXTS * stride);
        let mut input = InputContext::new(words, context_64).map_err(Error::Xhci)?;
        input.slot(&slot).map_err(Error::Xhci)?;
        for &dci in drop {
            input.drop_endpoint(dci).map_err(Error::Xhci)?;
        }
        for (dci, context) in add {
            input.endpoint(*dci, context).map_err(Error::Xhci)?;
        }
        hc.command(trb::configure_endpoint(self.mem.bus(INPUT), self.slot))
            .map(|_| ())
    }
}
