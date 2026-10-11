//! DAC/ADC calibration, DACK (upstream `rtw8822c_rf_dac_cal` and its
//! helpers in `rtw8822c.c`: `rtw8822c_dac_cal_adc`, `_step1` to `_step4`,
//! `rtw8822c_dac_cal_rf_mode`, the IQ sampling and sorting).
//!
//! The chip measures the DC offset of each path's ADC and DAC through the
//! report register 0x2dbc (10-bit I and Q, two's complement around 0x400)
//! and writes compensation values. The first run's results are kept
//! ([`super::dack_restore`]) and restored on later bring-ups, which is much
//! faster; upstream does the same.

use crate::dev::{Dev, RF_PATH_A, RF_PATH_B};
use crate::hci::{Hci, RegExt};
use crate::phy::rf::{read_rf, write_rf, RFREG_MASK};
use crate::rtw8822c::dack_restore::{dac_cal_backup, dac_cal_restore};
use crate::rtw8822c::regs::bit;
use crate::rtw8822c::util::{check_hw_ready, Backup};

/// Upstream `DACK_SN_8822C`: samples per measurement.
const SN: usize = 100;

/// Upstream `rtw8822c_get_path_write_addr` / `_read_addr`.
pub(super) fn write_addr(path: usize) -> u32 {
    if path == RF_PATH_A {
        0x1800
    } else {
        0x4100
    }
}

pub(super) fn read_addr(path: usize) -> u32 {
    if path == RF_PATH_A {
        0x2800
    } else {
        0x4500
    }
}

/// Upstream `rtw8822c_rf_dac_cal`.
pub fn rf_dac_cal<H: Hci>(dev: &mut Dev<H>) {
    if dac_cal_restore(dev) {
        return;
    }
    let backup = Backup::save(
        &mut dev.hci,
        [
            0x180c, 0x1810, 0x410c, 0x4110, 0x1c3c, 0x1c24, 0x1d70, 0x9b4, 0x1a00, 0x1a14, 0x1d58, 0x1c38, 0x1e24,
            0x1e28, 0x1860, 0x4160,
        ],
    );
    // Upstream indexes its RF backup as `path * i + i` with i = 0, so both
    // paths share one slot: path B's RF 0x8f is what both get back.
    let mut rf_8f = 0;
    for path in [RF_PATH_A, RF_PATH_B] {
        rf_8f = read_rf(dev, path, 0x8f, RFREG_MASK);
    }

    bb_setting(&mut dev.hci);
    for path in [RF_PATH_A, RF_PATH_B] {
        let (adc_ic, adc_qc) = cal_adc(dev, path);
        for _ in 0..10 {
            step1(dev, path);
            let (ic, qc) = step2(dev, path);
            let (ic, qc) = step3(dev, path, adc_ic, adc_qc, ic, qc);
            if ic < 5 && qc < 5 {
                break;
            }
        }
        step4(&mut dev.hci, path);
    }

    let h = &mut dev.hci;
    h.write32(0x1b00, 0x0000_0008);
    h.write32_mask(0x4130, bit(30), 0x1);
    h.write8(0x1bcc, 0x0);
    h.write32(0x1b00, 0x0000_000a);
    h.write8(0x1bcc, 0x0);

    backup.restore(&mut dev.hci);
    for path in [RF_PATH_A, RF_PATH_B] {
        write_rf(dev, path, 0x8f, RFREG_MASK, rf_8f);
    }
    // Keep the results so the next bring-up restores instead of measuring.
    dac_cal_backup(dev);
}

/// Upstream `rtw8822c_dac_bb_setting`.
fn bb_setting<H: Hci>(h: &mut H) {
    h.write32_mask(0x1d58, 0xff8, 0x1ff);
    h.write32_mask(0x1a00, 0x3, 0x2);
    h.write32_mask(0x1a14, 0x300, 0x3);
    h.write32(0x1d70, 0x7e7e_7e7e);
    h.write32_mask(0x180c, 0x3, 0x0);
    h.write32_mask(0x410c, 0x3, 0x0);
    h.write32(0x1b00, 0x0000_0008);
    h.write8(0x1bcc, 0x3f);
    h.write32(0x1b00, 0x0000_000a);
    h.write8(0x1bcc, 0x3f);
    h.write32_mask(0x1e24, bit(31), 0x0);
    h.write32_mask(0x1e28, 0xf, 0x3);
}

/// Upstream `rtw8822c_dac_iq_check`: a sample within +-100 of zero.
fn iq_check(v: u32) -> bool {
    !((v >= 0x200 && 0x400u32.wrapping_sub(v) > 0x64) || (v < 0x200 && v > 0x64))
}

/// Upstream `rtw8822c_dac_cal_iq_sample`: up to 10000 reads for 100 good
/// samples.
fn iq_sample<H: Hci>(h: &mut H, iv: &mut [u32; SN], qv: &mut [u32; SN]) {
    let (mut i, mut cnt) = (0, 0);
    while i < SN && cnt < 10000 {
        cnt += 1;
        let t = h.read32_mask(0x2dbc, 0x3f_ffff);
        iv[i] = (t & 0x3f_f000) >> 12;
        qv[i] = t & 0x3ff;
        if iq_check(iv[i]) && iq_check(qv[i]) {
            i += 1;
        }
    }
}

/// Upstream `rtw8822c_rf_minmax_cmp` (values >= 0x200 are negative).
fn minmax_cmp(v: u32, min: &mut u32, max: &mut u32) {
    if v >= 0x200 {
        if *min >= 0x200 {
            if *min > v {
                *min = v;
            }
        } else {
            *min = v;
        }
        if *max >= 0x200 && *max < v {
            *max = v;
        }
    } else {
        if *min < 0x200 && *min > v {
            *min = v;
        }
        if *max >= 0x200 || *max < v {
            *max = v;
        }
    }
}

/// Upstream `__rtw8822c_dac_iq_sort`: order two samples, negatives first.
fn sort2(v1: &mut u32, v2: &mut u32) {
    let swap = match (*v1 >= 0x200, *v2 >= 0x200) {
        (true, true) | (false, false) => *v1 > *v2,
        (false, true) => true,
        (true, false) => false,
    };
    if swap {
        core::mem::swap(v1, v2);
    }
}

/// Upstream `rtw8822c_dac_iq_sort` (a bubble sort of both vectors).
fn iq_sort(iv: &mut [u32; SN], qv: &mut [u32; SN]) {
    for i in 0..SN - 1 {
        for j in 0..SN - 1 - i {
            let (a, b) = iv.split_at_mut(j + 1);
            sort2(&mut a[j], &mut b[0]);
            let (a, b) = qv.split_at_mut(j + 1);
            sort2(&mut a[j], &mut b[0]);
        }
    }
}

/// Upstream `rtw8822c_dac_iq_offset`: the mean of the middle 80 samples.
fn iq_offset(vec: &[u32; SN]) -> u32 {
    let (mut m, mut p) = (0u32, 0u32);
    for &v in &vec[10..SN - 10] {
        if v > 0x200 {
            m = m.wrapping_add(0x400u32.wrapping_sub(v));
        } else {
            p = p.wrapping_add(v);
        }
    }
    let n = (SN - 20) as u32;
    if p > m {
        (p - m) / n
    } else {
        let t = (m - p) / n;
        if t != 0 {
            0x400 - t
        } else {
            0
        }
    }
}

fn delta(min: u32, max: u32) -> u32 {
    if (max < 0x200 && min < 0x200) || (max >= 0x200 && min >= 0x200) {
        max.wrapping_sub(min)
    } else {
        max.wrapping_add(0x400u32.wrapping_sub(min))
    }
}

/// Upstream `rtw8822c_dac_cal_iq_search`: re-sample the extremes until
/// the spread is within 5, then average.
fn iq_search<H: Hci>(h: &mut H, iv: &mut [u32; SN], qv: &mut [u32; SN]) -> (u32, u32) {
    let mut cnt = 0;
    loop {
        let (mut i_min, mut i_max, mut q_min, mut q_max) = (iv[0], iv[0], qv[0], qv[0]);
        for i in 0..SN {
            minmax_cmp(iv[i], &mut i_min, &mut i_max);
            minmax_cmp(qv[i], &mut q_min, &mut q_max);
        }
        let (i_delta, q_delta) = (delta(i_min, i_max), delta(q_min, q_max));
        iq_sort(iv, qv);
        if i_delta > 5 || q_delta > 5 {
            let t = h.read32_mask(0x2dbc, 0x3f_ffff);
            iv[0] = (t & 0x3f_f000) >> 12;
            qv[0] = t & 0x3ff;
            let t = h.read32_mask(0x2dbc, 0x3f_ffff);
            iv[SN - 1] = (t & 0x3f_f000) >> 12;
            qv[SN - 1] = t & 0x3ff;
        } else {
            break;
        }
        // Upstream's `do { } while (cnt++ < 100)`.
        let again = cnt < 100;
        cnt += 1;
        if !again {
            break;
        }
    }
    (iq_offset(iv), iq_offset(qv))
}

/// Upstream `rtw8822c_dac_cal_rf_mode`: one I/Q offset measurement.
pub(super) fn rf_mode<H: Hci>(dev: &mut Dev<H>) -> (u32, u32) {
    // Upstream reads both paths' RF mode for its debug log.
    let _ = read_rf(dev, RF_PATH_A, 0x0, RFREG_MASK);
    let _ = read_rf(dev, RF_PATH_B, 0x0, RFREG_MASK);
    let mut iv = [0u32; SN];
    let mut qv = [0u32; SN];
    iq_sample(&mut dev.hci, &mut iv, &mut qv);
    iq_search(&mut dev.hci, &mut iv, &mut qv)
}

/// Upstream `rtw8822c_dac_cal_adc`: ADC DC offset of `path`, up to 10
/// rounds; returns the last non-zero compensation (I, Q).
fn cal_adc<H: Hci>(dev: &mut Dev<H>, path: usize) -> (u32, u32) {
    let base = write_addr(path);
    let path_sel = if path == RF_PATH_A { 0xa0000 } else { 0x80000 };
    let (mut adc_ic, mut adc_qc) = (0, 0);
    let h = &mut dev.hci;
    h.write32_mask(base + 0x30, bit(30), 0x0);
    if path == RF_PATH_B {
        h.write32(base + 0x30, 0x30db_8041);
    }
    h.write32(base + 0x60, 0xf004_0ff0);
    h.write32(base + 0x0c, 0xdff0_0220);
    h.write32(base + 0x10, 0x02dd_08c4);
    h.write32(base + 0x0c, 0x1000_0260);
    write_rf(dev, RF_PATH_A, 0x0, RFREG_MASK, 0x10000);
    write_rf(dev, RF_PATH_B, 0x0, RFREG_MASK, 0x10000);
    for _ in 0..10 {
        dev.hci.write32(0x1c3c, path_sel + 0x8003);
        dev.hci.write32(0x1c24, 0x0001_0002);
        let (mut ic, mut qc) = rf_mode(dev);
        if ic != 0 {
            ic = 0x400u32.wrapping_sub(ic);
            adc_ic = ic;
        }
        if qc != 0 {
            qc = 0x400u32.wrapping_sub(qc);
            adc_qc = qc;
        }
        let temp = (ic & 0x3ff) | ((qc & 0x3ff) << 10);
        dev.hci.write32(base + 0x68, temp);
        dev.dm.dack.adck[path] = temp;
        // Check the ADC DC offset with the compensation applied.
        dev.hci.write32(0x1c3c, path_sel + 0x8103);
        let (mut ic, mut qc) = rf_mode(dev);
        if ic >= 0x200 {
            ic = 0x400 - ic;
        }
        if qc >= 0x200 {
            qc = 0x400 - qc;
        }
        if ic < 5 && qc < 5 {
            break;
        }
    }
    let h = &mut dev.hci;
    h.write32(0x1c3c, 0x0000_0003);
    h.write32(base + 0x0c, 0x1000_0260);
    h.write32(base + 0x10, 0x02d5_08c4);
    // Release the pull-low switch on the IQ path.
    write_rf(dev, path, 0x8f, bit(13), 0x1);
    (adc_ic, adc_qc)
}

/// Upstream `rtw8822c_dac_cal_step1`: run the DAC's own calibration.
fn step1<H: Hci>(dev: &mut Dev<H>, path: usize) {
    let (base, read) = (write_addr(path), read_addr(path));
    let adck = dev.dm.dack.adck[path];
    let h = &mut dev.hci;
    h.write32(base + 0x68, adck);
    h.write32(base + 0x0c, 0xdff0_0220);
    if path == RF_PATH_A {
        h.write32(base + 0x60, 0xf004_0ff0);
        h.write32(0x1c38, 0xffff_ffff);
    }
    h.write32(base + 0x10, 0x02d5_08c5);
    h.write32(0x9b4, 0xdb66_db00);
    h.write32(base + 0xb0, 0x0a11_fb88);
    h.write32(base + 0xbc, 0x0008_ff81);
    h.write32(base + 0xc0, 0x0003_d208);
    h.write32(base + 0xcc, 0x0a11_fb88);
    h.write32(base + 0xd8, 0x0008_ff81);
    h.write32(base + 0xdc, 0x0003_d208);
    h.write32(base + 0xb8, 0x6000_0000);
    h.delay_us(2000);
    h.write32(base + 0xbc, 0x000a_ff8d);
    h.delay_us(2000);
    h.write32(base + 0xb0, 0x0a11_fb89);
    h.write32(base + 0xcc, 0x0a11_fb89);
    h.delay_us(1000);
    h.write32(base + 0xb8, 0x6200_0000);
    h.write32(base + 0xd4, 0x6200_0000);
    h.delay_us(20_000);
    // Upstream logs "failed to wait for dack ready" and carries on.
    let _ = check_hw_ready(h, read + 0x08, 0x7f_ff80, 0xffff) && check_hw_ready(h, read + 0x34, 0x7f_ff80, 0xffff);
    h.write32(base + 0xb8, 0x0200_0000);
    h.delay_us(1000);
    h.write32(base + 0xbc, 0x0008_ff87);
    h.write32(0x9b4, 0xdb6d_b600);
    h.write32(base + 0x10, 0x02d5_08c5);
    h.write32(base + 0xbc, 0x0008_ff87);
    h.write32(base + 0x60, 0xf000_0000);
}

/// Upstream `rtw8822c_dac_cal_step2`: measure the DAC offset and scale it
/// into the compensation code (x2.4, centred at 0x80).
fn step2<H: Hci>(dev: &mut Dev<H>, path: usize) -> (u32, u32) {
    let base = write_addr(path);
    let h = &mut dev.hci;
    h.write32_mask(base + 0xbc, 0xf000_0000, 0x0);
    h.write32_mask(base + 0xc0, 0xf, 0x8);
    h.write32_mask(base + 0xd8, 0xf000_0000, 0x0);
    h.write32_mask(base + 0xdc, 0xf, 0x8);
    h.write32(0x1b00, 0x0000_0008);
    h.write8(0x1bcc, 0x03f);
    h.write32(base + 0x0c, 0xdff0_0220);
    h.write32(base + 0x10, 0x02d5_08c5);
    h.write32(0x1c3c, 0x0008_8103);
    let (ic, qc) = rf_mode(dev);
    (scale(ic), scale(qc))
}

fn scale(v: u32) -> u32 {
    let v = if v != 0 { 0x400u32.wrapping_sub(v) } else { v };
    if v < 0x300 {
        v * 2 * 6 / 5 + 0x80
    } else {
        0x7fu32.wrapping_sub(0x400u32.wrapping_sub(v) * 2 * 6 / 5)
    }
}

/// Upstream `rtw8822c_dac_cal_step3`: apply the codes, re-run, and return
/// the residual offsets (magnitudes) the caller's loop checks.
fn step3<H: Hci>(dev: &mut Dev<H>, path: usize, adc_ic: u32, adc_qc: u32, ic: u32, qc: u32) -> (u32, u32) {
    let (base, read) = (write_addr(path), read_addr(path));
    let h = &mut dev.hci;
    h.write32(base + 0x0c, 0xdff0_0220);
    h.write32(base + 0x10, 0x02d5_08c5);
    h.write32(0x9b4, 0xdb66_db00);
    h.write32(base + 0xb0, 0x0a11_fb88);
    h.write32(base + 0xbc, 0xc008_ff81);
    h.write32(base + 0xc0, 0x0003_d208);
    h.write32_mask(base + 0xbc, 0xf000_0000, ic & 0xf);
    h.write32_mask(base + 0xc0, 0xf, (ic & 0xf0) >> 4);
    h.write32(base + 0xcc, 0x0a11_fb88);
    h.write32(base + 0xd8, 0xe008_ff81);
    h.write32(base + 0xdc, 0x0003_d208);
    h.write32_mask(base + 0xd8, 0xf000_0000, qc & 0xf);
    h.write32_mask(base + 0xdc, 0xf, (qc & 0xf0) >> 4);
    h.write32(base + 0xb8, 0x6000_0000);
    h.delay_us(2000);
    h.write32_mask(base + 0xbc, 0xe, 0x6);
    h.delay_us(2000);
    h.write32(base + 0xb0, 0x0a11_fb89);
    h.write32(base + 0xcc, 0x0a11_fb89);
    h.delay_us(1000);
    h.write32(base + 0xb8, 0x6200_0000);
    h.write32(base + 0xd4, 0x6200_0000);
    h.delay_us(20_000);
    // Upstream logs "failed to write IQ vector to hardware" and carries on.
    let _ = check_hw_ready(h, read + 0x24, 0x07f8_0000, ic) && check_hw_ready(h, read + 0x50, 0x07f8_0000, qc);
    h.write32(base + 0xb8, 0x0200_0000);
    h.delay_us(1000);
    h.write32_mask(base + 0xbc, 0xe, 0x3);
    h.write32(0x9b4, 0xdb6d_b600);
    // Check the DAC DC offset with a known ADC offset added.
    let temp = ((adc_ic + 0x10) & 0x3ff) | (((adc_qc + 0x10) & 0x3ff) << 10);
    h.write32(base + 0x68, temp);
    h.write32(base + 0x10, 0x02d5_08c5);
    h.write32(base + 0x60, 0xf000_0000);
    let (i, q) = rf_mode(dev);
    let unbias = |v: u32| if v >= 0x10 { v - 0x10 } else { 0x400 - (0x10 - v) };
    let mag = |v: u32| if v >= 0x200 { 0x400u32.wrapping_sub(v) } else { v };
    (mag(unbias(i)), mag(unbias(q)))
}

/// Upstream `rtw8822c_dac_cal_step4`: leave calibration mode.
fn step4<H: Hci>(h: &mut H, path: usize) {
    let base = write_addr(path);
    h.write32(base + 0x68, 0x0);
    h.write32(base + 0x10, 0x02d5_08c4);
    h.write32_mask(base + 0xbc, 0x1, 0x0);
    h.write32_mask(base + 0x30, bit(30), 0x1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_of_symmetric_samples() {
        let mut v = [0u32; SN];
        for (i, x) in v.iter_mut().enumerate() {
            *x = if i % 2 == 0 { 3 } else { 0x3fd };
        }
        assert_eq!(iq_offset(&v), 0);
        let v = [4u32; SN];
        assert_eq!(iq_offset(&v), 4);
        let v = [0x3fcu32; SN];
        assert_eq!(iq_offset(&v), 0x3fc);
    }

    #[test]
    fn sort_puts_negatives_first() {
        let mut iv = [0u32; SN];
        let mut qv = [0u32; SN];
        iv[0] = 5;
        iv[1] = 0x3ff;
        iv[2] = 0x3fe;
        iq_sort(&mut iv, &mut qv);
        assert_eq!(&iv[..3], &[0x3fe, 0x3ff, 0]);
        assert_eq!(iv[SN - 1], 5);
    }

    #[test]
    fn scale_matches_upstream_formula() {
        assert_eq!(scale(0), 0x80);
        // 0x3f0 -> 0x10 -> 0x10 * 12 / 5 + 0x80
        assert_eq!(scale(0x3f0), 0x10 * 12 / 5 + 0x80);
        // 0x10 -> 0x3f0 (>= 0x300) -> 0x7f - 0x10 * 12 / 5
        assert_eq!(scale(0x10), 0x7f - 0x10 * 12 / 5);
    }
}
