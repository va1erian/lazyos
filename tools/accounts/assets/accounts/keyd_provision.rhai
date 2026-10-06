// Attack: plant a password verifier for an account with keyd Provision.
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
let verdict = "SUCCEEDED:ok";
try {
    sys::keyd::provision("acct-probe", "acct-probe-secret-1");
} catch (e) {
    let text = e.to_string();
    if text.len() > 40 { text = text.sub_string(0, 40); }
    verdict = "BLOCKED:" + text.replace(" ", "_");
}
print("ACCT:ATTACK:keyd_provision:" + verdict);
