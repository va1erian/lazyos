// Attack: bind a file type to an app of the attacker's choosing at mimed
// (only the package manager may, audit H11): the registry is global, so a
// session that could would have the next user's `Open`, an administrator's
// included, launch its app. Only a recognised refusal counts as BLOCKED: any
// other failure is an ERROR, so a broken probe can never pass as proof of
// isolation.
let verdict = "SUCCEEDED:ok";
try {
    sys::mimed::register("text/x-accounts-probe", "evil.app", "open");
    verdict = "SUCCEEDED:registered";
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:mime_register:" + verdict);
