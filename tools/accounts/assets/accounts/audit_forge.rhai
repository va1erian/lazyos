// Attack (review of #659): forge a line of elevd's audit trail. A conf.set
// text value carrying a carriage return, a line break and a whole
// ELEVD:REQUEST line would, if written raw, add a fake "granted" record to
// the serial log and the journal. BLOCKED when elevd refuses the value
// before any prompt (EINVAL, "control or formatting characters": a value
// may hold rows, `\n` and `\t`, shown escaped, but never a `\r`);
// attack_judge.py also fails the run if a line with admin=forged ever
// appears. Never left waiting on a prompt: the call gives up after msg's
// default 5 s.
let forged = "x\r\nELEVD:REQUEST op=account.admin uid=0 label=0 session=0 admin=forged outcome=granted summary=FORGED";
let verdict = "SUCCEEDED:ok";
try {
    sys::elevd::request("conf.set", ["sys/accounts/forge", "str", forged]);
} catch (e) {
    let text = e.to_string();
    if text.contains("(EINVAL") && text.contains("control or formatting") {
        verdict = "BLOCKED:EINVAL";
    } else if text.contains("(EAGAIN") || text.contains("(ECANCELED") || text.contains("(ETIMEDOUT") || text.contains("timed out") {
        // It reached the prompt (or its hold): the value was accepted.
        verdict = "SUCCEEDED:accepted";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:audit_forge:" + verdict);
