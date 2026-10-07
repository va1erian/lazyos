// Attack: Drop admin's password verifier at keyd (only accountsd may, U1).
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// Only a recognised refusal counts as BLOCKED: any other failure is an
// ERROR, so a broken probe can never pass as proof of isolation.
let verdict = "SUCCEEDED:ok";
try {
    sys::keyd::forget("admin");
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else if text.contains("(EACCES") {
        verdict = "BLOCKED:EACCES";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:keyd_forget:" + verdict);
