// The admin_lockout scenario (review of #659, H5): while auth_hammer.rhai
// floods Authenticate("admin", ...) from the session, ask elevd for the
// Config editor's view (conf.elevate, which changes nothing); the harness
// types admin's right password into the trusted prompt. BLOCKED (the
// attack failed) when elevd grants it: the session's flood did not lock
// admin out. SUCCEEDED when the right password was refused as locked.
//
// An earlier prompt scenario's cancel holds this session back for a while
// (elevd, H4: "may ask again in" / "an administrator prompt was just
// cancelled"): that hold is waited out first. It is not admin's lockout,
// which elevd reports as "too many wrong administrator passwords".
// A call waits 5 s for its reply by default; the prompt waits for a person.
msg::set_timeout(0);
let verdict = "BLOCKED:granted";
for attempt in 0..240 {
    try {
        sys::elevd::request("conf.elevate", []);
        verdict = "BLOCKED:granted";
    } catch (e) {
        let text = e.to_string();
        let held = text.contains("may ask again in") || text.contains("prompt was just cancelled");
        if text.contains("(EAGAIN") && held {
            os::sleep(1000);
            continue;
        }
        if text.contains("(EAGAIN") || text.contains("(EBUSY") {
            verdict = "SUCCEEDED:locked";
        } else if text.contains("(EACCES") || text.contains("(EPERM") {
            verdict = "SUCCEEDED:refused";
        } else {
            verdict = "ERROR:unrecognised";
        }
    }
    break;
}
print("ACCT:ATTACK:admin_lockout:" + verdict);
