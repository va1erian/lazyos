// Attack: check a password at keyd directly, around accountsd's brake
// (only accountsd may, review of #659 H2). The right password, so a
// keyd that answered would say SUCCEEDED, never a misleading BLOCKED.
// Only a recognised refusal counts as BLOCKED: any other failure is an
// ERROR, so a broken probe can never pass as proof of isolation.
let verdict = "SUCCEEDED:ok";
try {
    sys::keyd::verify("admin", "nimda");
    verdict = "SUCCEEDED:answered";
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:keyd_verify:" + verdict);
