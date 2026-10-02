//! Rhai values <-> Messenger TLV bodies, driven by the generated schema.
//!
//! The mapping (also documented for scripts in `docs/rhai/msg.md`):
//!
//! | IDL | Rhai |
//! |---|---|
//! | `Bool` | `bool` |
//! | `I32`/`U32`/`I64` | `int`, range-checked on the way out |
//! | `U64` | `int`; the 64-bit pattern is kept, so ids above `i64::MAX` read back negative and round-trip unchanged |
//! | `F64` | `float` (an `int` is accepted) |
//! | `String` | `string` |
//! | `Bytes` | `blob` (a `string` is accepted as its UTF-8 bytes) |
//! | `Array<T>` | `array` |
//! | `Option<T>` | `()` for none, else the value |
//! | struct | object map; missing fields take their zero value, unknown keys are an error |
//! | enum | the variant name as a string (an `int` index is accepted) |
//! | `Handle`/`Buffer` | read-only: decoded as `int` / a map, refused when sending |
//!
//! Decoding follows the compiled codecs: unknown field ids are skipped (a newer
//! service may add fields), malformed bodies are an error, never a panic.
//! Nesting is bounded by [`MAX_DEPTH`] in both directions.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Field as Tlv, Kind};
use rhai::{Array, Blob, Dynamic, Map, FLOAT, INT};

use super::schema::{Field, Interface, Ty};

/// Deepest composite nesting either direction accepts (`libmessenger`'s own).
pub const MAX_DEPTH: u8 = libmessenger::MAX_DEPTH;

type Result<T> = core::result::Result<T, String>;

fn wire(error: libmessenger::Error) -> String {
    error.message().to_string()
}

fn mismatch(what: &str, want: &str, value: &Dynamic) -> String {
    format!("{what}: expected {want}, got {}", value.type_name())
}

fn int_in(what: &str, value: &Dynamic, min: INT, max: INT) -> Result<INT> {
    let n = value
        .as_int()
        .map_err(|_| mismatch(what, "an integer", value))?;
    if n < min || n > max {
        return Err(format!("{what}: {n} is out of range ({min}..={max})"));
    }
    Ok(n)
}

/// Encode `values` as the fields `fields` (ids 1..), positionally.
pub fn encode_positional(
    iface: &Interface,
    fields: &[Field],
    values: &[Dynamic],
) -> Result<Vec<u8>> {
    if values.len() != fields.len() {
        return Err(format!(
            "expected {} argument(s), got {}",
            fields.len(),
            values.len()
        ));
    }
    let mut target = Encoder::new();
    for (index, (field, value)) in fields.iter().zip(values).enumerate() {
        encode_value(
            &mut target,
            index as u16 + 1,
            field.ty,
            value,
            iface,
            0,
            field.name,
        )?;
    }
    Ok(target.finish())
}

/// Encode a map of named values as `fields`; absent names take zero values.
pub fn encode_named(iface: &Interface, fields: &[Field], map: &Map, depth: u8) -> Result<Vec<u8>> {
    if let Some(key) = map
        .keys()
        .find(|k| !fields.iter().any(|f| f.name == k.as_str()))
    {
        let known: Vec<&str> = fields.iter().map(|f| f.name).collect();
        return Err(format!(
            "unknown field `{key}` (fields: {})",
            known.join(", ")
        ));
    }
    let mut target = Encoder::new();
    for (index, field) in fields.iter().enumerate() {
        let id = index as u16 + 1;
        match map.get(field.name) {
            Some(value) => {
                encode_value(&mut target, id, field.ty, value, iface, depth, field.name)?
            }
            None => encode_value(
                &mut target,
                id,
                field.ty,
                &zero(field.ty, iface),
                iface,
                depth,
                field.name,
            )?,
        }
    }
    Ok(target.finish())
}

/// The value a missing struct field or argument takes (the Rust `Default`).
fn zero(ty: Ty, iface: &Interface) -> Dynamic {
    match ty {
        Ty::Bool => Dynamic::FALSE,
        Ty::I32 | Ty::I64 | Ty::U32 | Ty::U64 | Ty::Handle => Dynamic::from_int(0),
        Ty::Enum(_) => Dynamic::from_int(0),
        Ty::F64 => Dynamic::from_float(0.0),
        Ty::String => Dynamic::from(String::new()),
        Ty::Bytes => Dynamic::from_blob(Blob::new()),
        Ty::Array(_) => Dynamic::from_array(Array::new()),
        Ty::Option(_) | Ty::Buffer => Dynamic::UNIT,
        Ty::Struct(name) => match iface.find_struct(name) {
            Some(_) => Dynamic::from_map(Map::new()),
            None => Dynamic::UNIT,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_value(
    target: &mut Encoder,
    id: u16,
    ty: Ty,
    value: &Dynamic,
    iface: &Interface,
    depth: u8,
    what: &str,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(format!("{what}: nested too deeply"));
    }
    let done = match ty {
        Ty::Bool => target.bool(
            id,
            value
                .as_bool()
                .map_err(|_| mismatch(what, "a bool", value))?,
        ),
        Ty::I32 => target.i32(
            id,
            int_in(what, value, i32::MIN.into(), i32::MAX.into())? as i32,
        ),
        Ty::U32 => target.u32(id, int_in(what, value, 0, u32::MAX.into())? as u32),
        Ty::I64 => target.i64(id, int_in(what, value, INT::MIN, INT::MAX)?),
        // Bit pattern kept on purpose (see the module table).
        Ty::U64 => target.u64(id, int_in(what, value, INT::MIN, INT::MAX)? as u64),
        Ty::F64 => target.f64(id, to_float(what, value)?),
        Ty::String => {
            let text = value
                .read_lock::<rhai::ImmutableString>()
                .ok_or_else(|| mismatch(what, "a string", value))?;
            target.string(id, &text)
        }
        Ty::Bytes => target.bytes(id, &to_bytes(what, value)?),
        Ty::Enum(name) => target.u32(id, enum_index(what, value, iface, name)?),
        Ty::Handle | Ty::Buffer => {
            return Err(format!(
                "{what}: scripts cannot send handles or shared buffers"
            ))
        }
        Ty::Array(inner) => {
            let items = value
                .read_lock::<Array>()
                .ok_or_else(|| mismatch(what, "an array", value))?;
            let mut nested = Encoder::new();
            for item in items.iter() {
                encode_value(&mut nested, 1, *inner, item, iface, depth + 1, what)?;
            }
            target.array(id, &nested)
        }
        Ty::Option(inner) => {
            if value.is_unit() {
                target.option(id, None)
            } else {
                let mut nested = Encoder::new();
                encode_value(&mut nested, 1, *inner, value, iface, depth + 1, what)?;
                target.option(id, Some(&nested))
            }
        }
        Ty::Struct(name) => {
            let def = iface
                .find_struct(name)
                .ok_or_else(|| format!("{what}: unknown struct {name}"))?;
            let map = value
                .read_lock::<Map>()
                .ok_or_else(|| mismatch(what, "an object map", value))?;
            let body = encode_named(iface, def.fields, &map, depth + 1)
                .map_err(|e| format!("{what} ({name}): {e}"))?;
            target.raw(Kind::Struct, id, &body)
        }
    };
    done.map_err(|e| format!("{what}: {}", wire(e)))
}

fn to_float(what: &str, value: &Dynamic) -> Result<f64> {
    if let Ok(f) = value.as_float() {
        return Ok(f);
    }
    value
        .as_int()
        .map(|n| n as f64)
        .map_err(|_| mismatch(what, "a number", value))
}

fn to_bytes(what: &str, value: &Dynamic) -> Result<Vec<u8>> {
    if let Some(blob) = value.read_lock::<Blob>() {
        return Ok(blob.clone());
    }
    value
        .read_lock::<rhai::ImmutableString>()
        .map(|s| s.as_bytes().to_vec())
        .ok_or_else(|| mismatch(what, "a blob or string", value))
}

fn enum_index(what: &str, value: &Dynamic, iface: &Interface, name: &str) -> Result<u32> {
    let variants = iface.find_enum(name).map_or(&[][..], |e| e.variants);
    if let Some(text) = value.read_lock::<rhai::ImmutableString>() {
        return variants
            .iter()
            .position(|v| *v == text.as_str())
            .map(|i| i as u32)
            .ok_or_else(|| {
                format!(
                    "{what}: `{}` is not a {name} ({})",
                    text.as_str(),
                    variants.join(", ")
                )
            });
    }
    Ok(int_in(what, value, 0, u32::MAX.into())? as u32)
}

/// Decode `body` into a map of `fields` (missing fields take zero values).
pub fn decode_named(iface: &Interface, fields: &[Field], body: &[u8], depth: u8) -> Result<Map> {
    if depth >= MAX_DEPTH {
        return Err("reply nested too deeply".into());
    }
    let mut out = Map::new();
    let mut decoder = Decoder::new(body);
    while let Some(tlv) = decoder.next().map_err(wire)? {
        // A structured error is the caller's business (`reply_error`).
        if tlv.kind == Kind::Error {
            continue;
        }
        let Some(field) = (tlv.id as usize).checked_sub(1).and_then(|i| fields.get(i)) else {
            continue; // a newer peer's field
        };
        let value = decode_value(&tlv, field.ty, iface, depth)
            .map_err(|e| format!("{}: {e}", field.name))?;
        out.insert(field.name.into(), value);
    }
    for field in fields {
        if !out.contains_key(field.name) {
            out.insert(field.name.into(), zero(field.ty, iface));
        }
    }
    Ok(out)
}

fn decode_value(tlv: &Tlv<'_>, ty: Ty, iface: &Interface, depth: u8) -> Result<Dynamic> {
    Ok(match ty {
        Ty::Bool => Dynamic::from_bool(tlv.as_bool().map_err(wire)?),
        Ty::I32 => Dynamic::from_int(tlv.as_i32().map_err(wire)?.into()),
        Ty::U32 => Dynamic::from_int(tlv.as_u32().map_err(wire)?.into()),
        Ty::I64 => Dynamic::from_int(tlv.as_i64().map_err(wire)?),
        Ty::U64 | Ty::Handle => Dynamic::from_int(tlv.as_u64().map_err(wire)? as INT),
        Ty::F64 => Dynamic::from_float(tlv.as_f64().map_err(wire)? as FLOAT),
        Ty::String => Dynamic::from(String::from(tlv.as_str().map_err(wire)?)),
        Ty::Bytes => Dynamic::from_blob(tlv.as_bytes().to_vec()),
        Ty::Buffer => {
            let b = tlv.as_buffer().map_err(wire)?;
            let mut map = Map::new();
            map.insert("handle".into(), Dynamic::from_int(b.handle as INT));
            map.insert("offset".into(), Dynamic::from_int(b.offset as INT));
            map.insert("len".into(), Dynamic::from_int(b.len as INT));
            map.insert("flags".into(), Dynamic::from_int(b.flags.into()));
            Dynamic::from_map(map)
        }
        Ty::Enum(name) => {
            let index = tlv.as_u32().map_err(wire)?;
            match iface
                .find_enum(name)
                .and_then(|e| e.variants.get(index as usize))
            {
                Some(variant) => Dynamic::from(String::from(*variant)),
                None => Dynamic::from_int(index.into()),
            }
        }
        Ty::Array(inner) => {
            let mut items = Array::new();
            let mut nested = tlv.nested(depth + 1).map_err(wire)?;
            while let Some(item) = nested.next().map_err(wire)? {
                items.push(decode_value(&item, *inner, iface, depth + 1)?);
            }
            Dynamic::from_array(items)
        }
        Ty::Option(inner) => {
            if tlv.payload.is_empty() {
                Dynamic::UNIT
            } else {
                let mut nested = tlv.nested(depth + 1).map_err(wire)?;
                let item = nested.next().map_err(wire)?.ok_or("empty option payload")?;
                decode_value(&item, *inner, iface, depth + 1)?
            }
        }
        Ty::Struct(name) => {
            let def = iface
                .find_struct(name)
                .ok_or_else(|| format!("unknown struct {name}"))?;
            Dynamic::from_map(decode_named(iface, def.fields, tlv.payload, depth + 1)?)
        }
    })
}

/// A service's structured error in a reply body: `(code, message)`.
///
/// Services reply with one `Error` field (at an id outside the declared reply
/// fields) instead of the reply; any top-level error field counts.
pub fn reply_error(body: &[u8]) -> Option<(u32, String)> {
    let mut decoder = Decoder::new(body);
    while let Ok(Some(tlv)) = decoder.next() {
        if tlv.kind == Kind::Error {
            let (code, message) = tlv.error_parts().ok()?;
            return Some((code, message.into()));
        }
    }
    None
}

/// Encode a topic payload of the declared type `payload` (a struct is the
/// body of its fields, an enum a single `U32` field 1, as the compiled topic
/// helpers write them).
pub fn encode_payload(iface: &Interface, payload: &str, value: &Dynamic) -> Result<Vec<u8>> {
    if let Some(def) = iface.find_struct(payload) {
        let map = value
            .read_lock::<Map>()
            .ok_or_else(|| mismatch(payload, "an object map", value))?;
        return encode_named(iface, def.fields, &map, 0);
    }
    let mut target = Encoder::new();
    let index = enum_index(payload, value, iface, payload)?;
    target.u32(1, index).map_err(wire)?;
    Ok(target.finish())
}

/// Decode a topic payload of the declared type `payload`.
pub fn decode_payload(iface: &Interface, payload: &str, body: &[u8]) -> Result<Dynamic> {
    if let Some(def) = iface.find_struct(payload) {
        return decode_named(iface, def.fields, body, 0).map(Dynamic::from_map);
    }
    let mut decoder = Decoder::new(body);
    while let Some(tlv) = decoder.next().map_err(wire)? {
        if tlv.id == 1 {
            return decode_value(&tlv, Ty::Enum(static_enum_name(iface, payload)), iface, 0);
        }
    }
    Err(format!("{payload}: empty payload"))
}

/// The `'static` spelling of an enum name declared by `iface`.
fn static_enum_name(iface: &Interface, name: &str) -> &'static str {
    iface.find_enum(name).map_or("", |e| e.name)
}
