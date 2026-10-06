// Attack: write confd's system settings (sys/**) as the session user.
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// The verdict is the `set` alone: a failed cleanup never turns a write that
// went through into BLOCKED, and only a recognised refusal counts as BLOCKED.
let verdict = "SUCCEEDED:ok";
let written = false;
try {
    let v = sys::confd::new_value();
    v.kind = 3;
    v.str_value = "attack";
    sys::confd::set("sys/accounts/probe", v);
    written = true;
} catch (e) {
    let text = e.to_string();
    if text.contains("(EACCES") {
        verdict = "BLOCKED:EACCES";
    } else if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else if text.contains("code 1004)") {
        // confd's own refusal, CONFD_DENIED (idl/confd.midl).
        verdict = "BLOCKED:CONFD_DENIED";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
if written {
    try {
        sys::confd::delete("sys/accounts/probe");
    } catch (e) {
        verdict = "SUCCEEDED:cleanup_failed";
    }
}
print("ACCT:ATTACK:confd_sys:" + verdict);
