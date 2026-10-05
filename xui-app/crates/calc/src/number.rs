//! Numbers as a pocket calculator shows them.
//!
//! The engine computes in `f64` and rounds every result to [`PRECISION`]
//! digits, a little under what `f64` holds, so the binary noise of a sum
//! like `0.1 + 0.2` never reaches the next operation. [`format`] shows
//! [`SIGNIFICANT`] digits of it, as plain decimals with no trailing zeros,
//! switching to scientific notation only when the plain form would not fit:
//! `1 ÷ 3 × 3` shows `1`.

/// The significant digits the display shows (and a typed number may have).
pub const SIGNIFICANT: usize = 12;

/// The significant digits a result keeps between operations.
pub const PRECISION: usize = 15;

/// The longest text [`format`] returns, sign included.
pub const MAX_LEN: usize = 16;

/// The largest magnitude the plain form shows (`999 999 999 999`).
const PLAIN_MAX_EXP: i32 = SIGNIFICANT as i32 - 1;

/// Smaller magnitudes than `1e-6` read better in scientific notation.
const PLAIN_MIN_EXP: i32 = -6;

/// Mantissa digits after the point in scientific notation, so the longest
/// form (`-1.2345679e-100`) stays within [`MAX_LEN`].
const SCI_DECIMALS: usize = 7;

/// `value` rounded to [`PRECISION`] significant digits.
pub fn round(value: f64) -> f64 {
    if value == 0.0 || !value.is_finite() {
        return value;
    }
    format!("{:.*e}", PRECISION - 1, value)
        .parse()
        .unwrap_or(value)
}

/// The decimal exponent of `value` once rounded to [`SIGNIFICANT`] digits,
/// read from the rounded text: `999.9999999999` rounds to `1000`, so `3`.
fn exponent(value: f64) -> i32 {
    let text = format!("{:.*e}", SIGNIFICANT - 1, value);
    text.split_once('e')
        .and_then(|(_, exp)| exp.parse().ok())
        .unwrap_or(0)
}

/// `value` as the display shows it: `"0.3"`, `"-12"`, `"1.5e-9"`.
pub fn format(value: f64) -> String {
    if value == 0.0 {
        // Also -0.0: a calculator never shows a negative zero result.
        return "0".to_owned();
    }
    let exp = exponent(value);
    if (PLAIN_MIN_EXP..=PLAIN_MAX_EXP).contains(&exp) {
        let decimals = (PLAIN_MAX_EXP - exp).max(0) as usize;
        let plain = trim_fraction(format!("{value:.decimals$}"));
        if plain.len() <= MAX_LEN {
            return plain;
        }
    }
    scientific(value)
}

/// `value` in scientific notation with a trimmed mantissa: `1.5e-9`.
fn scientific(value: f64) -> String {
    let text = format!("{value:.SCI_DECIMALS$e}");
    match text.split_once('e') {
        Some((mantissa, exp)) => format!("{}e{exp}", trim_fraction(mantissa.to_owned())),
        None => text,
    }
}

/// Drops trailing zeros after a decimal point, and the point itself when
/// nothing follows it: `"2.500"` -> `"2.5"`, `"3.000"` -> `"3"`.
fn trim_fraction(mut text: String) -> String {
    if text.contains('.') {
        let kept = text.trim_end_matches('0').trim_end_matches('.').len();
        text.truncate(kept);
    }
    if text == "-0" {
        text = "0".to_owned();
    }
    text
}

/// How many digits a typed entry holds (sign and point excluded).
pub fn digit_count(entry: &str) -> usize {
    entry.chars().filter(char::is_ascii_digit).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_hides_binary_noise() {
        assert_eq!(round(0.1 + 0.2), 0.3);
        assert_eq!(format(round(0.1 + 0.2)), "0.3");
        assert_eq!(format(round(1.1 * 1.1)), "1.21");
        assert_eq!(format(round(0.7 + 0.1)), "0.8");
        assert_eq!(format(round(3.0 * 1.1)), "3.3");
    }

    #[test]
    fn integers_have_no_fraction() {
        assert_eq!(format(0.0), "0");
        assert_eq!(format(-0.0), "0");
        assert_eq!(format(57.0), "57");
        assert_eq!(format(-12.0), "-12");
        assert_eq!(format(999_999_999_999.0), "999999999999");
    }

    #[test]
    fn fractions_are_trimmed() {
        assert_eq!(format(2.5), "2.5");
        assert_eq!(format(-0.125), "-0.125");
        assert_eq!(format(round(2.0 / 3.0)), "0.666666666667");
        assert_eq!(format(round(round(1.0 / 3.0) * 3.0)), "1");
        assert_eq!(format(round(1.0 / 3.0)), "0.333333333333");
        assert_eq!(format(0.000_001_5), "0.0000015");
    }

    #[test]
    fn large_and_tiny_values_go_scientific() {
        assert_eq!(format(1e12), "1e12");
        assert_eq!(format(-1.5e15), "-1.5e15");
        assert_eq!(format(123_456_789_012_345.0), "1.2345679e14");
        assert_eq!(format(1.5e-9), "1.5e-9");
        assert_eq!(format(-1.234_567_89e-100), "-1.2345679e-100");
    }

    #[test]
    fn rounding_up_to_a_new_power_of_ten_stays_plain() {
        // 999.9999999999 rounds to 1000, which must print as an integer,
        // not with the eight decimals its unrounded exponent asks for.
        assert_eq!(format(999.999_999_999_9), "1000");
    }

    #[test]
    fn every_format_fits_the_display() {
        let values = [
            1.0 / 7.0,
            -1.0 / 7.0,
            123_456.789_012_345,
            -999_999_999_999.0,
            1e-6 / 7.0,
            -1e-7 / 3.0,
            f64::MAX,
            -f64::MAX,
            f64::MIN_POSITIVE,
        ];
        for value in values {
            let text = format(round(value));
            assert!(text.len() <= MAX_LEN, "{value}: {text}");
            assert!(text.parse::<f64>().is_ok(), "{value}: {text}");
        }
    }

    #[test]
    fn digits_are_counted_without_sign_or_point() {
        assert_eq!(digit_count("-12.50"), 4);
        assert_eq!(digit_count("0."), 1);
    }
}
