//! The Rhai text the runner puts in front of a script that asks for it.
//!
//! The child `rhai` has no module the app can add to, so the helpers are
//! plain functions with a `pg_` prefix (`pg_show(value)`): a script-defined
//! function cannot have a `::` in its name. Error positions the child reports
//! count the prelude's lines; [`crate::runner::compose`] and
//! [`crate::runner::map_error`] subtract them again.

/// The helpers. `pg_show` prints a value as the same indented tree
/// [`crate::show::tree`] returns (a test compares the two).
pub const SOURCE: &str = r#"// --- Messenger Playground prelude: pg_show(value) prints a value as a tree ---
fn pg_blob(b) {
    let n = b.len();
    let s = `blob(${n})`;
    let shown = if n > 32 { 32 } else { n };
    for i in 0..shown {
        let x = b[i];
        s += if x < 16 { " 0" } else { " " };
        s += x.to_hex();
    }
    if n > shown { s += " ..."; }
    s
}
fn pg_scalar(v) {
    let t = type_of(v);
    if t == "()" { "()" }
    else if t == "string" || t == "char" { v.to_debug() }
    else if t == "blob" { pg_blob(v) }
    else { v.to_string() }
}
fn pg_show_at(label, v, pad, depth) {
    let t = type_of(v);
    if t == "map" {
        print(`${pad}${label}map(${v.len()})`);
        if depth >= 20 { print(`${pad}  ...`); return; }
        for k in v.keys() { pg_show_at(`${k}: `, v[k], pad + "  ", depth + 1); }
    } else if t == "array" {
        print(`${pad}${label}array(${v.len()})`);
        if depth >= 20 { print(`${pad}  ...`); return; }
        for i in 0..v.len() { pg_show_at(`[${i}]: `, v[i], pad + "  ", depth + 1); }
    } else {
        print(`${pad}${label}${pg_scalar(v)}`);
    }
}
fn pg_show(v) { pg_show_at("", v, "", 0); }
// --- end of prelude ---
"#;

/// How many lines the prelude adds in front of a script.
pub fn lines() -> usize {
    SOURCE.lines().count()
}

/// Whether `script` mentions a helper, so it needs the prelude.
pub fn wanted(script: &str) -> bool {
    script.contains("pg_show") || script.contains("pg_blob") || script.contains("pg_scalar")
}
