//! Boot-log lines as data.
//!
//! The grammar and the parser live in `tracefmt` (docs/tracing-plan.md): the
//! kernel, the services and the apps build lines with it, and this module
//! parses with the same code, so an emitter and a reader cannot drift. What
//! stays here is the JSON form `dbgd` answers with.

use alloc::string::String;

pub use tracefmt::line::{is_marker, parse, split_complete, Line, MAX_FIELDS};

use crate::json::Object;

/// `{"pos":..?,"t_ms":..?,"tag":..?,"fields":{..},"text":".."}`; `pos` is
/// the byte offset of the line in the boot log when the caller knows it.
pub fn to_json(line: &Line, pos: Option<u64>) -> String {
    let mut object = Object::new();
    if let Some(pos) = pos {
        object = object.uint("pos", pos);
    }
    if let Some(ms) = line.stamp_ms {
        object = object.uint("t_ms", ms);
    }
    if let Some(tag) = &line.tag {
        object = object.str("tag", tag);
    }
    if !line.fields.is_empty() {
        let mut fields = Object::new();
        for (index, (key, value)) in line.fields.iter().enumerate() {
            // A repeated key keeps its first value; JSON objects take one.
            if !line.fields[..index].iter().any(|(k, _)| k == key) {
                fields = fields.str(key, value);
            }
        }
        object = object.raw("fields", &fields.finish());
    }
    object.str("text", &line.text).finish()
}
