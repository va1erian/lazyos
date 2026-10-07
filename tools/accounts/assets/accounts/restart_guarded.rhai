// Attack (review of #659): ask elevd to restart a service a session must
// never restart (the first argument: elevd or xuid), so that an approval,
// a hold or the trusted prompt itself goes away. BLOCKED when elevd refuses
// it before any prompt (EPERM, "may not be restarted"); the scenario is
// named restart_<service>.
let args = os::args();
let name = if args.len() > 0 { args[0] } else { "elevd" };
let verdict = "SUCCEEDED:ok";
try {
    sys::elevd::request("service.restart", [name]);
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") && text.contains("may not be restarted") {
        verdict = "BLOCKED:EPERM";
    } else if text.contains("(EAGAIN") || text.contains("(ECANCELED") || text.contains("(ETIMEDOUT") || text.contains("timed out") {
        // It reached the prompt (or its hold): nothing refused it first.
        verdict = "SUCCEEDED:prompted";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:restart_" + name + ":" + verdict);
