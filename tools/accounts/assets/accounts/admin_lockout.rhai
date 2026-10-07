// The admin_lockout scenario (review of #659, H5): while auth_hammer.rhai
// floods Authenticate("admin", ...) from the session, ask elevd for the
// Config editor's view (conf.elevate, which changes nothing); the harness
// types admin's right password into the trusted prompt. BLOCKED (the
// attack failed) when elevd grants it: the session's flood did not lock
// admin out. SUCCEEDED when the right password was refused as locked.
// A call waits 5 s for its reply by default; the prompt waits for a person.
msg::set_timeout(0);
let verdict = "BLOCKED:granted";
try {
    sys::elevd::request("conf.elevate", []);
} catch (e) {
    let text = e.to_string();
    if text.contains("(EAGAIN") || text.contains("(EBUSY") {
        verdict = "SUCCEEDED:locked";
    } else if text.contains("(EACCES") || text.contains("(EPERM") {
        verdict = "SUCCEEDED:refused";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:admin_lockout:" + verdict);
