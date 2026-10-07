// Attack: Open the trusted administrator prompt itself (only elevd may, U2).
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// Only a recognised refusal counts as BLOCKED: any other failure is an
// ERROR, so a broken probe can never pass as proof of isolation.
let verdict = "SUCCEEDED:ok";
try {
    msg::connect("os.lazy.display.prompt.v1", "os.lazy.display.v1").invoke("Prompt", ["Install everything", 1000, "user", 0, "admin", ""]);
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
print("ACCT:ATTACK:prompt_spoof:" + verdict);
