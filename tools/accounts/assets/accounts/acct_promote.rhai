// Attack: Make the session user an administrator directly at accountsd (U1).
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// BLOCKED only with EPERM *and* accountsd's policy (libs/accountdb/src/policy.rs)
// text: a wrong old password (EACCES), an ACL refusal or any other EPERM is
// another reason, printed as such, which the judge refuses, so a regression
// cannot pass as BLOCKED. Anything unrecognised is an ERROR.
let policy = "only an administrator, through elevd, may create, delete or promote accounts";
let verdict = "SUCCEEDED:ok";
try {
    msg::connect("os.lazy.accounts.v1", "os.lazy.accountsd").invoke("SetAdmin", ["user", true]);
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") && text.contains(policy) {
        verdict = "BLOCKED:EPERM-policy";
    } else if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM-other";
    } else if text.contains("(EACCES") {
        verdict = "BLOCKED:EACCES";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:acct_promote:" + verdict);
