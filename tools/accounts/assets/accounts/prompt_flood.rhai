// Attack (U2, review of #659 H4): raise the trusted prompt again and again,
// so the person at the screen never gets the desktop back (and never reaches
// Log out). The first request shows the prompt and the harness cancels it;
// then five more come back to back. BLOCKED when every one of them is
// refused at once, without a prompt (EAGAIN, elevd's hold after a cancelled
// prompt); SUCCEEDED when any of them showed a prompt again (it was
// cancelled, timed out or approved). The prompt is not given a person's
// answer the second time, so a prompt would end only by timing out.
msg::set_timeout(0);
let first = "";
for attempt in 0..240 {
    try {
        sys::elevd::request("time.set", ["1767225600"]);
        first = "approved";
    } catch (e) {
        let text = e.to_string();
        // An earlier scenario's hold: wait it out before the first prompt.
        if text.contains("(EAGAIN") && (text.contains("may ask again") || text.contains("try again in")) {
            os::sleep(1000);
            continue;
        }
        first = if text.contains("(ECANCELED") { "cancelled" } else { "other" };
    }
    break;
}
if first != "cancelled" {
    print("ACCT:ATTACK:prompt_flood:ERROR:first_" + first);
    exit(0);
}
let held = 0;
let prompted = 0;
let other = 0;
let started = os::clock();
for i in 0..5 {
    try {
        sys::elevd::request("time.set", ["1767225600"]);
        prompted += 1;
    } catch (e) {
        let text = e.to_string();
        if text.contains("(EAGAIN") && text.contains("may ask again") {
            held += 1;
        } else if text.contains("(ECANCELED") || text.contains("(ETIMEDOUT") {
            prompted += 1;
        } else {
            other += 1;
        }
    }
}
let took = os::clock() - started;
let verdict = "SUCCEEDED:prompted=" + prompted;
if prompted == 0 && other > 0 {
    verdict = "ERROR:other=" + other;
} else if prompted == 0 && held == 5 && took < 5.0 {
    verdict = "BLOCKED:held=" + held;
} else if prompted == 0 {
    verdict = "ERROR:held=" + held + "_slow";
}
print("ACCT:ATTACK:prompt_flood:" + verdict);
