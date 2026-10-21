//! Parsing what a person types into a form field. Every function is total:
//! any text gives a value or a message, never a panic.

use rhai_lazy::msg::schema::Ty;

/// The inclusive range of integers a form field of `ty` accepts. A `U64`
/// accepts the negative values Rhai reads ids above `i64::MAX` back as (the
/// codec keeps the 64-bit pattern).
pub fn int_range(ty: Ty) -> Option<(i128, i128)> {
    Some(match ty {
        Ty::I32 => (i32::MIN.into(), i32::MAX.into()),
        Ty::U32 => (0, u32::MAX.into()),
        Ty::I64 => (i64::MIN.into(), i64::MAX.into()),
        Ty::U64 => (i64::MIN.into(), u64::MAX.into()),
        _ => return None,
    })
}

/// An integer as typed: optional sign, decimal digits with `_`, or a `0x`,
/// `0o` or `0b` prefix. Checked against `min..=max`.
pub fn parse_int(text: &str, min: i128, max: i128) -> Result<i128, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("enter a number".to_string());
    }
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let (radix, body) = match digits.get(..2) {
        Some("0x") | Some("0X") => (16, &digits[2..]),
        Some("0o") | Some("0O") => (8, &digits[2..]),
        Some("0b") | Some("0B") => (2, &digits[2..]),
        _ => (10, digits),
    };
    let clean: String = body.chars().filter(|c| *c != '_').collect();
    if clean.is_empty() || !clean.chars().all(|c| c.is_digit(radix)) {
        return Err(format!("`{trimmed}` is not a number"));
    }
    let magnitude =
        i128::from_str_radix(&clean, radix).map_err(|_| format!("`{trimmed}` is too large"))?;
    let value = if negative { -magnitude } else { magnitude };
    if value < min || value > max {
        return Err(format!("{value} is out of range ({min}..={max})"));
    }
    Ok(value)
}

/// The Rhai integer literal for a form integer: values of a `U64` above
/// `i64::MAX` are written as the negative number with the same 64 bits.
pub fn int_literal(value: i128) -> String {
    if value > i128::from(i64::MAX) {
        let wrapped = value - (1_i128 << 64);
        return int_text(wrapped);
    }
    int_text(value)
}

fn int_text(value: i128) -> String {
    if value == i128::from(i64::MIN) {
        "(-9223372036854775807 - 1)".to_string()
    } else {
        value.to_string()
    }
}

/// A decimal number as typed (`1.5`, `-2`, `1e3`, `inf`, `nan`).
pub fn parse_float(text: &str) -> Result<f64, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("enter a number".to_string());
    }
    let cleaned: String = trimmed.chars().filter(|c| *c != '_').collect();
    cleaned
        .parse::<f64>()
        .map_err(|_| format!("`{trimmed}` is not a number"))
}

/// Hexadecimal bytes as typed: pairs of digits, optionally separated by
/// spaces, commas or colons, with an optional `0x` prefix.
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    let mut digits = Vec::with_capacity(body.len());
    for ch in body.chars() {
        match ch {
            ' ' | ',' | ':' | '\t' | '\n' | '\r' | '_' => {}
            c if c.is_ascii_hexdigit() => digits.push(c.to_digit(16).unwrap_or(0) as u8),
            other => return Err(format!("`{other}` is not a hex digit")),
        }
    }
    if digits.len() % 2 != 0 {
        return Err("hex bytes come in pairs of digits".to_string());
    }
    Ok(digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect())
}

/// Bytes as space-separated hex pairs, the form [`parse_hex`] reads back.
pub fn format_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
