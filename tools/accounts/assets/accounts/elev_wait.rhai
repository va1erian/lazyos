// The prompt scenarios (U2): ask elevd to set the clock, which shows the
// trusted prompt; the harness types into it and cancels it. Prints
// ACCT:PROMPT:<tag>:CANCELLED when elevd reports the cancel (the tag, the
// first argument, names the scenario: over, keys, flood), or
// ACCT:PROMPT:<tag>:NOKEYS when the compositor refused to show a prompt it
// could not take the keyboard for (review of #659, H3).
//
// A cancelled prompt holds the asker back for a while (elevd, H4): an
// earlier scenario's hold is waited out first, never mistaken for an answer.
// A call waits 5 s for its reply by default; the prompt waits for a person.
msg::set_timeout(0);
let args = os::args();
let tag = if args.len() > 0 { args[0] } else { "over" };
let outcome = "";
for attempt in 0..240 {
    try {
        sys::elevd::request("time.set", ["1767225600"]);
        outcome = "APPROVED";
    } catch (e) {
        let text = e.to_string();
        if text.contains("(EAGAIN") && (text.contains("may ask again") || text.contains("try again in")) {
            os::sleep(1000);
            continue;
        }
        if text.contains("(ECANCELED") {
            outcome = "CANCELLED";
        } else if text.contains("(EAGAIN") && text.contains("keyboard") {
            outcome = "NOKEYS";
        } else {
            outcome = `OTHER:${text}`;
        }
    }
    break;
}
print(`ACCT:PROMPT:${tag}:${outcome}`);
