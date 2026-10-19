// Attack: read another app's secret from keyd (os.lazy.keyd.appsecret.v1, P2b),
// as the session user's own unlabelled program: it is not an installed app, so
// it has no secrets of its own to name and none of anyone else's to reach.
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// Only a recognised refusal counts as BLOCKED: any other failure is an
// ERROR, so a broken probe can never pass as proof of isolation.
let verdict = "SUCCEEDED:ok";
try {
    // `token` is the name the keyd harness's probe apps keep (tools/keyd/).
    let secret = sys::keyd_appsecret::get("token");
    verdict = "SUCCEEDED:" + secret.len() + " bytes";
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else if text.contains("(EACCES") {
        verdict = "BLOCKED:EACCES";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:appsecret_read:" + verdict);
