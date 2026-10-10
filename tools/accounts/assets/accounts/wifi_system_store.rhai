// Attack: plant a machine-wide Wi-Fi secret (the `system` scope, which every
// user and the greeter's machine joins with) without elevd's approval
// (docs/wifi-prerequisites-plan.md WP2). Only a recognised refusal counts as
// BLOCKED; a store that succeeded is removed again (it must not, but the
// harness should not leave it behind).
let verdict = "SUCCEEDED:stored";
try {
    sys::keyd::store_secret("system", "acct-probe", "acct-probe-pass");
    try {
        sys::keyd::delete_secret("system", "acct-probe");
    } catch (e) {
    }
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:wifi_system_store:" + verdict);
