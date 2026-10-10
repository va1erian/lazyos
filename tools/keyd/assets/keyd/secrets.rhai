// One step of the keyd secrets check (tools/keyd/run.py), as the session user:
//   rhai secrets.rhai <step>
// Prints exactly one line, KEYD:T:<step>:PASS|FAIL:<detail>, which the
// Terminal reports on serial as TERM:OUT:... and run.py judges.
//
//   user_store      keep the user secret `home`
//   user_list       the user's names, comma separated (PASS carries them)
//   user_delete     forget `home`
//   pmk_denied      WifiPmk for the user's own secret: wlanmd's alone (EPERM)
//   pmk_other       WifiPmk naming admin's uid: EPERM as well
//   system_denied   StoreSecret in the system scope without elevd (EPERM)
//   system_store    net.wifi.system store through elevd (the prompt: admin)
//   system_delete   net.wifi.system delete through elevd (the prompt: admin)
//   system_list     the system names, comma separated
//   bad_input       a bad name and an empty secret are refused (EINVAL)
msg::set_timeout(0);
let step = os::args()[0];

fn names(scope) {
    let text = "";
    for name in sys::keyd::list_secrets(scope) {
        text += if text == "" { name } else { "," + name };
    }
    text
}

// The errno name `action` was refused with, or "ok".
fn refusal(action) {
    let out = "ok";
    try {
        action.call();
    } catch (e) {
        let text = e.to_string();
        out = "OTHER:" + text;
        for code in ["EPERM", "EINVAL", "ENOENT", "ENOSPC", "EIO", "EACCES", "ECANCELED"] {
            if text.contains("(" + code) {
                out = code;
            }
        }
    }
    out
}

let result = "FAIL:unknown step";
if step == "user_store" {
    let r = refusal(|| sys::keyd::store_secret("user", "home", "correct horse"));
    result = if r == "ok" { "PASS:stored" } else { "FAIL:" + r };
} else if step == "user_list" {
    result = "PASS:" + names("user");
} else if step == "user_delete" {
    let r = refusal(|| sys::keyd::delete_secret("user", "home"));
    result = if r == "ok" { "PASS:deleted" } else { "FAIL:" + r };
} else if step == "pmk_denied" {
    let r = refusal(|| sys::keyd::wifi_pmk("user", "home", "IEEE", 1000));
    result = if r == "EPERM" { "PASS:EPERM" } else { "FAIL:" + r };
} else if step == "pmk_other" {
    let r = refusal(|| sys::keyd::wifi_pmk("user", "home", "IEEE", 1001));
    result = if r == "EPERM" { "PASS:EPERM" } else { "FAIL:" + r };
} else if step == "system_denied" {
    let r = refusal(|| sys::keyd::store_secret("system", "intruder", "correct horse"));
    result = if r == "EPERM" { "PASS:EPERM" } else { "FAIL:" + r };
} else if step == "system_store" {
    let r = refusal(|| sys::elevd::request("net.wifi.system", ["store", "office", "correct horse"]));
    result = if r == "ok" { "PASS:stored" } else { "FAIL:" + r };
} else if step == "system_delete" {
    let r = refusal(|| sys::elevd::request("net.wifi.system", ["delete", "office"]));
    result = if r == "ok" { "PASS:deleted" } else { "FAIL:" + r };
} else if step == "system_list" {
    result = "PASS:" + names("system");
} else if step == "bad_input" {
    let a = refusal(|| sys::keyd::store_secret("user", "bad name", "correct horse"));
    let b = refusal(|| sys::keyd::store_secret("user", "empty", ""));
    let c = refusal(|| sys::keyd::store_secret("everyone", "x", "correct horse"));
    result = if a == "EINVAL" && b == "EINVAL" && c == "EINVAL" { "PASS:EINVAL" } else { `FAIL:${a},${b},${c}` };
}
print("KEYD:T:" + step + ":" + result);
