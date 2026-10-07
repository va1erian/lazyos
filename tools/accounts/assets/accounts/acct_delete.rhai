// Attack: Delete the admin account directly at accountsd (U1).
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// Only a recognised refusal counts as BLOCKED: any other failure is an
// ERROR, so a broken probe can never pass as proof of isolation.
let verdict = "SUCCEEDED:ok";
try {
    msg::connect("os.lazy.accounts.v1", "os.lazy.accountsd").invoke("Delete", ["admin", "keep"]);
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
print("ACCT:ATTACK:acct_delete:" + verdict);
