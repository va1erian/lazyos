// Attack: ask keyd for another user's Wi-Fi key, as the session user
// (docs/wifi-prerequisites-plan.md WP2). WifiPmk is wlanmd's alone, whoever's
// secret it names; admin's uid is named so a keyd that only checked the owner
// would still answer here. Only a recognised refusal counts as BLOCKED: any
// other failure is an ERROR, so a broken probe can never pass as proof.
let verdict = "SUCCEEDED:answered";
try {
    sys::keyd::wifi_pmk("user", "home", "IEEE", 1001);
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:wifi_pmk:" + verdict);
