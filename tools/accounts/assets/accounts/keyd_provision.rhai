// Attack: plant a password verifier for an account with keyd Provision.
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// Only a recognised refusal counts as BLOCKED: any other failure is an ERROR,
// so a broken probe can never pass as proof of isolation.
let verdict = "SUCCEEDED:ok";
try {
    sys::keyd::provision("acct-probe", "acct-probe-secret-1");
} catch (e) {
    let text = e.to_string();
    if text.contains("(EACCES") {
        verdict = "BLOCKED:EACCES";
    } else if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:keyd_provision:" + verdict);
