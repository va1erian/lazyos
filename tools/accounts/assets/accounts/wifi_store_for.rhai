// Attack: overwrite admin's Wi-Fi passphrase, plant a machine-wide one, and
// delete admin's, as the session user, through keyd's `StoreSecretFor` and
// `DeleteSecretFor` (WP2). They are wlanmd's alone, and only for Wi-Fi
// profile ids in the `user` scope; the name here is one, so only the caller
// check can refuse the first and the last. Every attempt must be refused
// with EPERM. Only a recognised refusal counts as BLOCKED: any other failure
// is an ERROR, so a broken probe can never pass as proof of isolation.

// The verdict of one attempt from its error text.
fn refusal(text) {
    if text.contains("(EPERM") { "BLOCKED:EPERM" } else { "ERROR:unrecognised" }
}

let id = "w0123456789ab";
let results = [];
try {
    sys::keyd::store_secret_for("user", id, "acct-probe-pass", 1001);
    results.push("SUCCEEDED:stored");
} catch (e) {
    results.push(refusal(e.to_string()));
}
try {
    sys::keyd::store_secret_for("system", id, "acct-probe-pass", 0);
    results.push("SUCCEEDED:stored-system");
} catch (e) {
    results.push(refusal(e.to_string()));
}
try {
    sys::keyd::delete_secret_for("user", id, 1001);
    results.push("SUCCEEDED:deleted");
} catch (e) {
    results.push(refusal(e.to_string()));
}
let verdict = "BLOCKED:EPERM";
for one in results {
    if !one.starts_with("BLOCKED") {
        verdict = one;
        break;
    }
}
print("ACCT:ATTACK:wifi_store_for:" + verdict);
