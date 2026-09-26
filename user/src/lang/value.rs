//! Runtime values for the interpreter.
//!
//! Dyon-inspired: numbers are `f64`, plus booleans, strings and arrays.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub enum Value {
    Num(f64),
    Bool(bool),
    Str(String),
    Array(Vec<Value>),
}

impl Value {
    /// Truthiness, as used by `if` and the logical operators.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Num(n) => *n != 0.0,
            Value::Bool(b) => *b,
            Value::Str(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Num(_) => "number",
            Value::Bool(_) => "bool",
            Value::Str(_) => "string",
            Value::Array(_) => "array",
        }
    }

    /// Human-readable rendering used by `print`.
    pub fn display(&self) -> String {
        match self {
            Value::Num(n) => format_num(*n),
            Value::Bool(b) => String::from(if *b { "true" } else { "false" }),
            Value::Str(s) => s.clone(),
            Value::Array(items) => {
                let mut out = String::from("[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&item.display());
                }
                out.push(']');
                out
            }
        }
    }
}

/// Format an `f64` without `std`. Whole numbers print without a decimal point.
///
/// Only uses arithmetic (no float methods) so it works in `no_std`.
fn format_num(n: f64) -> String {
    if n.is_nan() {
        return String::from("nan");
    }
    if n.is_infinite() {
        return String::from(if n < 0.0 { "-inf" } else { "inf" });
    }
    let positive = if n < 0.0 { -n } else { n };
    if n == (n as i64) as f64 && positive < 1e15 {
        return format!("{}", n as i64);
    }

    let whole = positive as u64;
    let mut fraction = positive - whole as f64;
    let mut digits = String::new();
    for _ in 0..6 {
        fraction *= 10.0;
        let digit = fraction as u8;
        digits.push((b'0' + digit) as char);
        fraction -= digit as f64;
    }
    while digits.ends_with('0') {
        digits.pop();
    }

    let sign = if n < 0.0 { "-" } else { "" };
    format!("{sign}{whole}.{digits}")
}
