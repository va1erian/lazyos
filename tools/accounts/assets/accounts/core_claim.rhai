// Attack (review of #659): have an administrator approve "an install" that
// is really a core app's replacement. corereplace.lzp claims os.lazy.counter
// (a core app) at 99.0.0; asked as pkg.install, elevd inspects it first and
// must refuse before any prompt (EPERM, "only pkg.update-core may"): a core
// app is replaced only by pkg.update-core, whose prompt names the app and
// both versions. BLOCKED only for that refusal.
let verdict = "SUCCEEDED:ok";
try {
    sys::elevd::request("pkg.install", ["/system/share/accounts/corereplace.lzp"]);
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") && text.contains("core app") {
        verdict = "BLOCKED:EPERM";
    } else if text.contains("(EAGAIN") || text.contains("(ECANCELED") || text.contains("(ETIMEDOUT") || text.contains("timed out") {
        // It reached the prompt (or its hold): nothing refused it first.
        verdict = "SUCCEEDED:prompted";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:core_claim:" + verdict);
