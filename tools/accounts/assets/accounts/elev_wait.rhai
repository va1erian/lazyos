// The prompt scenario (U2): ask elevd to set the clock, which shows the
// trusted prompt; the harness opens a window over it, types into it and
// cancels it. Prints ACCT:PROMPT:CANCELLED when elevd reports the cancel.
// A call waits 5 s for its reply by default; the prompt waits for a person.
msg::set_timeout(0);
try {
    sys::elevd::request("time.set", ["1767225600"]);
    print("ACCT:PROMPT:APPROVED");
} catch (e) {
    if e.to_string().contains("(ECANCELED") {
        print("ACCT:PROMPT:CANCELLED");
    } else {
        print(`ACCT:PROMPT:OTHER:${e}`);
    }
}
