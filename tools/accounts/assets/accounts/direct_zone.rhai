// Attack: Switch the time zone at timed directly instead of through elevd (U2).
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// BLOCKED only with EPERM *and* timed's refusal (user/src/bin/timed/handler.rs)
// text: a wrong old password (EACCES), an ACL refusal or any other EPERM is
// another reason, printed as such, which the judge refuses, so a regression
// cannot pass as BLOCKED. Anything unrecognised is an ERROR.
let policy = "only an administrator, through elevd, may change the clock or the time zone";
let verdict = "SUCCEEDED:ok";
try {
    sys::timed::set_zone("Asia/Tokyo");
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
print("ACCT:ATTACK:direct_zone:" + verdict);
