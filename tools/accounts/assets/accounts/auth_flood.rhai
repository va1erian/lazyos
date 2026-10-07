// Attack (U1): flood accountsd's Authenticate with wrong passwords for
// admin. Without a brake every attempt is checked; with it, after three
// failures the attempts are refused at once (EAGAIN) for a growing delay.
// BLOCKED when at most 8 of the 40 attempts were checked and some were slowed.
let accounts = msg::connect("os.lazy.accounts.v1", "os.lazy.accountsd");
let checked = 0;
let slowed = 0;
let other = 0;
for i in 0..40 {
    try {
        accounts.invoke("Authenticate", ["admin", "wrong-guess-" + i]);
        checked += 1;
    } catch (e) {
        if e.to_string().contains("(EAGAIN") {
            slowed += 1;
        } else {
            other += 1;
        }
    }
}
let verdict = "SUCCEEDED:checked=" + checked;
if other > 0 {
    verdict = "ERROR:other=" + other;
} else if checked <= 8 && slowed > 0 {
    verdict = "BLOCKED:checked=" + checked + "_slowed=" + slowed;
}
print("ACCT:ATTACK:auth_flood:" + verdict);
