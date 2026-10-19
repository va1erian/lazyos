// The labelled probe of the keyd app-secrets check (tools/keyd/run.py), run by
// the test package `org.keyd.<id>` (tools/keyd/probe_packages.py), so every
// call below is made under the label `app:org.keyd.<id>`:
//   rhai appprobe.rhai <id>
// Writes exactly one line to $HOME/.apps/org.keyd.<id>/out.txt, which
// probe.sh reads back:
//   KEYD:APP:<id>:<phase>:PASS|FAIL:<detail>
//
// first run (the app holds nothing yet)
//   - `token` is not found even if another app kept one of that name,
//   - Put and Get round-trip, bad names and an empty secret are EINVAL,
//   - the 17th secret is ENOSPC (the cap is 16 per app and user).
// later runs (the app holds `token`): it is still there, and unchanged.
msg::set_timeout(0);
let id = os::args()[0];
let value = "s3cret-" + id;

// The errno name `action` was refused with, or "ok".
fn refusal(action) {
    let out = "ok";
    try {
        action.call();
    } catch (e) {
        let text = e.to_string();
        out = "OTHER:" + text;
        for code in ["EPERM", "EINVAL", "ENOENT", "ENOSPC", "EIO", "EACCES"] {
            if text.contains("(" + code) {
                out = code;
            }
        }
    }
    out
}

fn text_of(blob) {
    if type_of(blob) == "blob" { blob.as_string() } else { blob.to_string() }
}

let phase = "first";
let detail = "";
let ok = true;
let names = sys::keyd_appsecret::list();
if names.len() == 0 {
    let seen = refusal(|| sys::keyd_appsecret::get("token"));
    let put = refusal(|| sys::keyd_appsecret::put("token", value));
    let back = text_of(sys::keyd_appsecret::get("token"));
    let bad_name = refusal(|| sys::keyd_appsecret::put("bad name", "x"));
    let empty = refusal(|| sys::keyd_appsecret::put("empty", ""));
    // `token` plus fifteen more is the cap; the next one is refused.
    let filled = true;
    for i in 0..15 {
        if refusal(|| sys::keyd_appsecret::put("k" + i, "x")) != "ok" {
            filled = false;
        }
    }
    let full = refusal(|| sys::keyd_appsecret::put("k15", "x"));
    for i in 0..15 {
        refusal(|| sys::keyd_appsecret::delete("k" + i));
    }
    let left = sys::keyd_appsecret::list().len();
    detail = `${seen},${put},${if back == value { "roundtrip" } else { "MISMATCH" }},${bad_name},${empty},${if filled { "filled" } else { "NOTFILLED" }},${full},left=${left}`;
    ok = seen == "ENOENT" && put == "ok" && back == value && bad_name == "EINVAL"
        && empty == "EINVAL" && filled && full == "ENOSPC" && left == 1;
} else {
    phase = "later";
    let back = text_of(sys::keyd_appsecret::get("token"));
    detail = `${if back == value { "survived" } else { "MISMATCH" }},names=${names.len()}`;
    ok = back == value && names.len() == 1;
}
let line = "KEYD:APP:" + id + ":" + phase + ":" + (if ok { "PASS" } else { "FAIL" }) + ":" + detail + "\n";
os::write(os::env("HOME") + "/.apps/org.keyd." + id + "/out.txt", line);
