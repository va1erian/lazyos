//! Host-to-chip commands (upstream `fw.c`: `rtw_fw_send_h2c_command`,
//! `rtw_fw_send_h2c_command_register`, `rtw_fw_send_h2c_packet`, and the
//! `fw.h` field setters they are filled with).
//!
//! Two transports: an 8-byte command goes through one of four mailboxes
//! (`HMEBOX0..3`, used in turn); a 32-byte packet goes down the H2C TX
//! queue with a sequence number.

use crate::dev::Dev;
use crate::error::{Error, Result};
use crate::hci::{Hci, RegExt};
use crate::mac::regs::{HMEBOX, HMEBOX_EX, HMETFR};
use crate::tx;

/// Bytes of an H2C packet (`H2C_PKT_SIZE`).
pub const H2C_PKT_SIZE: usize = 32;
/// Bytes of the H2C packet header (`H2C_PKT_HDR_SIZE`).
pub const H2C_PKT_HDR_SIZE: u16 = 8;
/// `H2C_PKT_CMD_ID` and `H2C_PKT_CATEGORY`.
const H2C_PKT_CMD_ID: u32 = 0xFF;
const H2C_PKT_CATEGORY: u32 = 0x01;

/// Upstream `le32p_replace_bits` on word `word` of an H2C buffer: replace
/// the field under `mask` with `value`.
pub fn set_field(buf: &mut [u8], word: usize, mask: u32, value: u32) {
    let at = word * 4;
    let Some(bytes) = buf.get_mut(at..at + 4) else {
        return;
    };
    let old = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let new = (old & !mask) | ((value << mask.trailing_zeros()) & mask);
    bytes.copy_from_slice(&new.to_le_bytes());
}

/// Upstream `SET_H2C_CMD_ID_CLASS`: the command id of an 8-byte command.
pub fn set_cmd_id(h2c: &mut [u8; 8], id: u8) {
    set_field(h2c, 0, 0xFF, u32::from(id));
}

/// Upstream `rtw_h2c_pkt_set_header` + `SET_PKT_H2C_TOTAL_LEN`.
pub fn set_pkt_header(pkt: &mut [u8; H2C_PKT_SIZE], sub_id: u8, payload_len: u16) {
    set_field(pkt, 0, 0x7F, H2C_PKT_CATEGORY);
    set_field(pkt, 0, 0xFF00, H2C_PKT_CMD_ID);
    set_field(pkt, 0, 0xFFFF_0000, u32::from(sub_id));
    set_field(pkt, 1, 0xFFFF, u32::from(H2C_PKT_HDR_SIZE + payload_len));
}

/// Wait for mailbox `box_num` to be free, then write `w0`/`w1` into it and
/// move to the next box. The `_EX` half goes first: writing the main half
/// is what raises the interrupt to the firmware.
fn send_box<H: Hci>(dev: &mut Dev<H>, w0: u32, w1: u32) -> Result<()> {
    let box_num = dev.fw.last_box_num;
    let (Some(&box_reg), Some(&box_ex_reg)) = (
        HMEBOX.get(box_num as usize),
        HMEBOX_EX.get(box_num as usize),
    ) else {
        return Err(Error::Invalid("invalid h2c mail box number"));
    };
    dev.hci
        .poll8("h2c mailbox", HMETFR, 1 << box_num, 0, 100, 3000)?;
    dev.hci.write32(box_ex_reg, w1);
    dev.hci.write32(box_reg, w0);
    dev.fw.last_box_num = (box_num + 1) % 4;
    Ok(())
}

/// Upstream `rtw_fw_send_h2c_command`: send an 8-byte command through the
/// next mailbox. This is the contract the chip package calls.
pub fn send_h2c_cmd<H: Hci>(dev: &mut Dev<H>, h2c: &[u8; 8]) -> Result<()> {
    let msg = u32::from_le_bytes([h2c[0], h2c[1], h2c[2], h2c[3]]);
    let msg_ext = u32::from_le_bytes([h2c[4], h2c[5], h2c[6], h2c[7]]);
    send_box(dev, msg, msg_ext)
}

/// Upstream `rtw_fw_send_h2c_command_register`: the same mailbox, words
/// given as host values.
pub fn send_h2c_register<H: Hci>(dev: &mut Dev<H>, w0: u32, w1: u32) -> Result<()> {
    send_box(dev, w0, w1)
}

/// Upstream `rtw_fw_send_h2c_packet`: stamp the sequence number and send the
/// packet down the H2C queue. Upstream's `rtw_tx_write_data_h2c_get` fills
/// only the size, and the PCIe bus the queue select, so the descriptor here
/// has exactly those two fields.
pub fn send_h2c_packet<H: Hci>(dev: &mut Dev<H>, pkt: &mut [u8; H2C_PKT_SIZE]) -> Result<()> {
    set_field(pkt, 1, 0xFFFF_0000, u32::from(dev.fw.seq));
    let ret = dev.hci.write_data_h2c(&tx::h2c_pkt(pkt));
    // Upstream advances the sequence even when the bus refused the packet.
    dev.fw.seq = dev.fw.seq.wrapping_add(1);
    ret
}
