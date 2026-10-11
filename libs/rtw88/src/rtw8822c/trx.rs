//! TX/RX path selection (upstream `rtw8822c_config_trx_mode`,
//! `rtw8822c_config_rx_path`, `rtw8822c_config_tx_path` and their CCK/OFDM
//! halves, `rtw8822c_bb_reset`, `rtw8822c_set_antenna`).
//!
//! The 8822C default (`config_trx_mode(BB_PATH_AB, BB_PATH_AB, false)`,
//! from `phy_set_param`): receive on both paths, OFDM transmit on both
//! (1-stream frames from path A, `ANTMAP0 = 0x31`), CCK transmit on path A.

use crate::dev::Dev;
use crate::error::{Error, Result};
use crate::hci::{Hci, RegExt};
use crate::rtw8822c::channel::toggle_igi;
use crate::rtw8822c::regs::*;

/// Upstream `rtw8822c_bb_reset`: pulse the BB reset.
pub fn bb_reset<H: Hci>(h: &mut H) {
    h.set16(REG_SYS_FUNC_EN, BIT_FEN_BB_RSTB as u16);
    h.clr16(REG_SYS_FUNC_EN, BIT_FEN_BB_RSTB as u16);
    h.set16(REG_SYS_FUNC_EN, BIT_FEN_BB_RSTB as u16);
}

fn config_cck_rx_path<H: Hci>(h: &mut H, rx_path: u8) {
    if rx_path == BB_PATH_A || rx_path == BB_PATH_B {
        h.write32_mask(REG_CCANRX, 0x0006_0000, 0x0);
        h.write32_mask(REG_CCANRX, 0x0060_0000, 0x0);
    } else if rx_path == BB_PATH_AB {
        h.write32_mask(REG_CCANRX, 0x0060_0000, 0x1);
        h.write32_mask(REG_CCANRX, 0x0006_0000, 0x1);
    }
    match rx_path {
        BB_PATH_A => h.write32_mask(REG_RXCCKSEL, 0x0f00_0000, 0x0),
        BB_PATH_B => h.write32_mask(REG_RXCCKSEL, 0x0f00_0000, 0x5),
        BB_PATH_AB => h.write32_mask(REG_RXCCKSEL, 0x0f00_0000, 0x1),
        _ => {}
    }
}

fn config_ofdm_rx_path<H: Hci>(h: &mut H, rx_path: u8) {
    let both = if rx_path == BB_PATH_A || rx_path == BB_PATH_B {
        Some(0)
    } else if rx_path == BB_PATH_AB {
        Some(1)
    } else {
        None
    };
    if let Some(v) = both {
        h.write32_mask(REG_RXFNCTL, 0x300, v);
        h.write32_mask(REG_RXFNCTL, 0x60_0000, v);
        h.write32_mask(REG_AGCSWSH, bit(17), v);
        h.write32_mask(REG_ANTWTPD, bit(20), v);
        h.write32_mask(REG_MRCM, bit(24), v);
    }
    h.write32_mask(0x824, 0x0f00_0000, u32::from(rx_path));
    h.write32_mask(0x824, 0x000f_0000, u32::from(rx_path));
}

fn config_cck_tx_path<H: Hci>(h: &mut H, tx_path: u8, is_tx2_path: bool) {
    let v = match tx_path {
        BB_PATH_A => 0x8,
        BB_PATH_B => 0x4,
        _ if is_tx2_path => 0xc,
        _ => 0x8,
    };
    h.write32_mask(REG_RXCCKSEL, 0xf000_0000, v);
    bb_reset(h);
}

fn config_ofdm_tx_path<H: Hci>(h: &mut H, tx_path: u8, tx_path_sel_1ss: u8) {
    match tx_path {
        BB_PATH_A => {
            h.write32_mask(REG_ANTMAP0, 0xff, 0x11);
            h.write32_mask(REG_TXLGMAP, 0xff, 0x0);
        }
        BB_PATH_B => {
            h.write32_mask(REG_ANTMAP0, 0xff, 0x12);
            h.write32_mask(REG_TXLGMAP, 0xff, 0x0);
        }
        _ => match tx_path_sel_1ss {
            BB_PATH_AB => {
                h.write32_mask(REG_ANTMAP0, 0xff, 0x33);
                h.write32_mask(REG_TXLGMAP, 0xffff, 0x0404);
            }
            BB_PATH_B => {
                h.write32_mask(REG_ANTMAP0, 0xff, 0x32);
                h.write32_mask(REG_TXLGMAP, 0xffff, 0x0400);
            }
            BB_PATH_A => {
                h.write32_mask(REG_ANTMAP0, 0xff, 0x31);
                h.write32_mask(REG_TXLGMAP, 0xffff, 0x0400);
            }
            _ => {}
        },
    }
    bb_reset(h);
}

/// Upstream `rtw8822c_config_tx_path`.
pub fn config_tx_path<H: Hci>(
    h: &mut H,
    tx_path: u8,
    tx_path_sel_1ss: u8,
    tx_path_cck: u8,
    is_tx2_path: bool,
) {
    config_cck_tx_path(h, tx_path_cck, is_tx2_path);
    config_ofdm_tx_path(h, tx_path, tx_path_sel_1ss);
    bb_reset(h);
}

/// Upstream `rtw8822c_config_trx_mode`: power the paths in use (TX code
/// 0x33312, idle 0x11111), select RX and TX paths (1-stream and CCK from
/// path A), then toggle the IGI.
pub fn config_trx_mode<H: Hci>(dev: &mut Dev<H>, tx_path: u8, rx_path: u8, is_tx2_path: bool) {
    let h = &mut dev.hci;
    let code = |used: bool| if used { 0x33312 } else { 0x11111 };
    h.write32_mask(
        REG_ORITXCODE,
        MASK20BITS,
        code((tx_path | rx_path) & BB_PATH_A != 0),
    );
    h.write32_mask(
        REG_ORITXCODE2,
        MASK20BITS,
        code((tx_path | rx_path) & BB_PATH_B != 0),
    );
    config_cck_rx_path(h, rx_path);
    config_ofdm_rx_path(h, rx_path);
    config_tx_path(h, tx_path, BB_PATH_A, BB_PATH_A, is_tx2_path);
    toggle_igi(dev);
}

/// Upstream `rtw8822c_set_antenna`: TX on A, B or both; RX on A or both
/// (path B alone cannot receive).
pub fn set_antenna<H: Hci>(dev: &mut Dev<H>, antenna_tx: u8, antenna_rx: u8) -> Result<()> {
    if !matches!(antenna_tx, BB_PATH_A | BB_PATH_B | BB_PATH_AB) {
        return Err(Error::Invalid("tx path"));
    }
    if !matches!(antenna_rx, BB_PATH_A | BB_PATH_AB) {
        return Err(Error::Invalid("rx path"));
    }
    dev.hal.antenna_tx = u32::from(antenna_tx);
    dev.hal.antenna_rx = u32::from(antenna_rx);
    config_trx_mode(dev, antenna_tx, antenna_rx, false);
    Ok(())
}
