// Attack: put the wired card in promiscuous mode as the session user, by
// calling the NIC driver's owner method `SetRxMode` on eth0 directly. Only
// the stack (`_netd`), the driver's own identity and root may call an owner
// method (`netpolicy::may_own_nic`); a session that could, could also
// attach its rings before `netd` and read every frame. EPERM is the
// driver's identity check; EACCES would only be "not the attached owner",
// which a session that raced `netd` would pass, so it is not proof here.
// Only a recognised refusal counts as BLOCKED: any other failure is an
// ERROR, so a broken probe can never pass as proof of isolation.
let verdict = "SUCCEEDED:answered";
try {
    msg::connect("os.lazy.net.nic.v1", "os.lazy.net.nic/eth0").invoke("SetRxMode", [2]);
} catch (e) {
    let text = e.to_string();
    if text.contains("(EPERM") {
        verdict = "BLOCKED:EPERM";
    } else if text.contains("(EACCES") {
        verdict = "SUCCEEDED:owner-check-only";
    } else {
        verdict = "ERROR:unrecognised";
    }
}
print("ACCT:ATTACK:nic_rx_mode:" + verdict);
