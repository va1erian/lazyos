// Attack: set a keyboard layout the session user does not own. The first
// argument picks the target: `machine` writes the machine default
// (sys/input/layout, which only elevd may write), `other` writes admin's own
// layout (user/1001/input/layout); the scenario is named layout_<target>.
// The session's own user/1000/input/layout is not an attack: it is how
// Settings sets the user's layout with no prompt.
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
let args = os::args();
let target = if args.len() > 0 { args[0] } else { "machine" };
let path = if target == "other" { "user/1001/input/layout" } else { "sys/input/layout" };
let verdict = "SUCCEEDED:ok";
try {
    let v = sys::confd::new_value();
    v.kind = 3;
    v.str_value = "fr";
    sys::confd::set(path, v);
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
print("ACCT:ATTACK:layout_" + target + ":" + verdict);
