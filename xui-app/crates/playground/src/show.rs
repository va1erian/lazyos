//! A Rhai value as text: an indented tree for the Output and the event list,
//! and "copy as Rhai" source that evaluates back to the same value.
//!
//! The tree is the format `pg_show` (the Rhai prelude, [`crate::prelude`])
//! prints; a test runs both over the same values.

use rhai::{Array, Blob, Dynamic, ImmutableString, Map};

use crate::util::rhai_str;

/// Containers nest at most this deep in the tree before `...`.
pub const MAX_TREE_DEPTH: usize = 20;
/// Bytes of a blob the tree shows.
pub const BLOB_SHOWN: usize = 32;

/// The blob line: `blob(3) 01 ff 10`.
pub fn blob_line(bytes: &[u8]) -> String {
    let mut out = format!("blob({})", bytes.len());
    for byte in bytes.iter().take(BLOB_SHOWN) {
        out.push_str(&format!(" {byte:02x}"));
    }
    if bytes.len() > BLOB_SHOWN {
        out.push_str(" ...");
    }
    out
}

fn scalar(value: &Dynamic) -> String {
    if value.is_unit() {
        "()".to_string()
    } else if let Some(text) = value.read_lock::<ImmutableString>() {
        format!("{:?}", text.as_str())
    } else if let Some(ch) = value.read_lock::<char>() {
        format!("{:?}", *ch)
    } else if let Some(blob) = value.read_lock::<Blob>() {
        blob_line(&blob)
    } else {
        value.to_string()
    }
}

fn tree_at(out: &mut Vec<String>, label: &str, value: &Dynamic, pad: &str, depth: usize) {
    let child_pad = format!("{pad}  ");
    if let Some(map) = value.read_lock::<Map>() {
        out.push(format!("{pad}{label}map({})", map.len()));
        if depth >= MAX_TREE_DEPTH {
            out.push(format!("{pad}  ..."));
            return;
        }
        for (key, item) in map.iter() {
            tree_at(out, &format!("{key}: "), item, &child_pad, depth + 1);
        }
    } else if let Some(items) = value.read_lock::<Array>() {
        out.push(format!("{pad}{label}array({})", items.len()));
        if depth >= MAX_TREE_DEPTH {
            out.push(format!("{pad}  ..."));
            return;
        }
        for (index, item) in items.iter().enumerate() {
            tree_at(out, &format!("[{index}]: "), item, &child_pad, depth + 1);
        }
    } else {
        out.push(format!("{pad}{label}{}", scalar(value)));
    }
}

/// The value as an indented tree, one line per leaf, no trailing newline.
pub fn tree(value: &Dynamic) -> String {
    tree_lines(value).join("\n")
}

/// The same, as lines.
pub fn tree_lines(value: &Dynamic) -> Vec<String> {
    let mut out = Vec::new();
    tree_at(&mut out, "", value, "", 0);
    out
}

/// Lines wider than this break a container over several lines in
/// [`to_rhai`].
const WIDE: usize = 72;

/// Whether `key` can be written bare as a map key: Rhai's rule is that the
/// first character that is not `_` is a letter, and the rest are letters,
/// digits or `_` (`_0` and `__` are refused).
fn is_identifier(key: &str) -> bool {
    let rest = key.trim_start_matches('_');
    matches!(rest.chars().next(), Some(c) if c.is_ascii_alphabetic())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !crate::util::is_rhai_reserved(key)
}

fn blob_source(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "blob()".to_string();
    }
    if bytes.iter().all(|b| (0x20..0x7f).contains(b)) {
        return format!("{}.to_blob()", rhai_str(&String::from_utf8_lossy(bytes)));
    }
    let items = bytes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}].reduce(|b, x| {{ b.push(x); b }}, blob())")
}

fn float_source(f: f64) -> String {
    if f.is_nan() {
        "(0.0 / 0.0)".to_string()
    } else if f.is_infinite() {
        if f > 0.0 {
            "(1.0 / 0.0)"
        } else {
            "(-1.0 / 0.0)"
        }
        .to_string()
    } else if f == 0.0 && f.is_sign_negative() {
        "(-0.0)".to_string()
    } else {
        Dynamic::from_float(f).to_string()
    }
}

fn int_source(n: i64) -> String {
    if n == i64::MIN {
        "(-9223372036854775807 - 1)".to_string()
    } else {
        n.to_string()
    }
}

fn source_at(value: &Dynamic, pad: &str) -> String {
    if value.is_unit() {
        return "()".to_string();
    }
    if let Ok(b) = value.as_bool() {
        return b.to_string();
    }
    if let Ok(n) = value.as_int() {
        return int_source(n);
    }
    if let Ok(f) = value.as_float() {
        return float_source(f);
    }
    if let Some(text) = value.read_lock::<ImmutableString>() {
        return rhai_str(&text);
    }
    if let Some(ch) = value.read_lock::<char>() {
        return format!("{:?}", *ch);
    }
    if let Some(blob) = value.read_lock::<Blob>() {
        return blob_source(&blob);
    }
    let inner = format!("{pad}    ");
    if let Some(items) = value.read_lock::<Array>() {
        let parts: Vec<String> = items.iter().map(|i| source_at(i, &inner)).collect();
        return join_container("[", "]", &parts, pad);
    }
    if let Some(map) = value.read_lock::<Map>() {
        let parts: Vec<String> = map
            .iter()
            .map(|(key, item)| {
                let key = if is_identifier(key) {
                    key.to_string()
                } else {
                    rhai_str(key)
                };
                format!("{key}: {}", source_at(item, &inner))
            })
            .collect();
        return join_container("#{", "}", &parts, pad);
    }
    format!("() /* {} */", value.type_name())
}

pub(crate) fn join_container(open: &str, close: &str, parts: &[String], pad: &str) -> String {
    if parts.is_empty() {
        return format!("{open}{close}");
    }
    let flat = if open == "[" {
        format!("[{}]", parts.join(", "))
    } else {
        format!("{open} {} {close}", parts.join(", "))
    };
    if flat.len() <= WIDE && !flat.contains('\n') {
        return flat;
    }
    let inner = format!("{pad}    ");
    let body = parts
        .iter()
        .map(|p| format!("{inner}{p},"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{open}\n{body}\n{pad}{close}")
}

/// The value as Rhai source that evaluates to an equal value ("copy as
/// Rhai"). A value of a type the codec never produces becomes `()` with a
/// comment naming the type.
pub fn to_rhai(value: &Dynamic) -> String {
    source_at(value, "")
}

/// Deep structural equality for the values the codec produces (unit, bool,
/// int, float, string, char, blob, array, map). Floats compare by bits, and
/// any NaN equals any other, so a round trip of NaN is equal to itself.
pub fn same(a: &Dynamic, b: &Dynamic) -> bool {
    if a.is_unit() || b.is_unit() {
        return a.is_unit() && b.is_unit();
    }
    if let (Some(x), Some(y)) = (a.read_lock::<Map>(), b.read_lock::<Map>()) {
        return x.len() == y.len()
            && x.iter()
                .all(|(k, v)| y.get(k.as_str()).is_some_and(|w| same(v, w)));
    }
    if let (Some(x), Some(y)) = (a.read_lock::<Array>(), b.read_lock::<Array>()) {
        return x.len() == y.len() && x.iter().zip(y.iter()).all(|(v, w)| same(v, w));
    }
    if let (Some(x), Some(y)) = (a.read_lock::<Blob>(), b.read_lock::<Blob>()) {
        return *x == *y;
    }
    if let (Some(x), Some(y)) = (
        a.read_lock::<ImmutableString>(),
        b.read_lock::<ImmutableString>(),
    ) {
        return x.as_str() == y.as_str();
    }
    if let (Ok(x), Ok(y)) = (a.as_float(), b.as_float()) {
        return x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan());
    }
    if let (Ok(x), Ok(y)) = (a.as_int(), b.as_int()) {
        return x == y;
    }
    if let (Ok(x), Ok(y)) = (a.as_bool(), b.as_bool()) {
        return x == y;
    }
    if let (Ok(x), Ok(y)) = (a.as_char(), b.as_char()) {
        return x == y;
    }
    false
}
