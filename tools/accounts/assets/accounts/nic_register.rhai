// Attack: register a NIC driver's name (os.lazy.net.nic/<ifname>) as the
// session user, to be handed netd's frame rings (DNS queries, traffic) and
// to make netd believe a card it describes. Only the NIC driver identities
// (netpolicy) may hold a name in that namespace; the registry refuses the
// rest before it looks at the endpoint, so the endpoint here is a dummy: a
// refusal is EACCES, and any other error means the name policy let the
// request through (the endpoint check failed after it).
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
// Only a recognised refusal counts as BLOCKED: any other failure is an
// ERROR, so a broken probe can never pass as proof of isolation.
let verdict = "BLOCKED:EACCES";
let names = ["os.lazy.net.nic/eth9", "os.lazy.net.nic/eth0", "os.lazy.net.nic"];
for name in names {
    let one = "";
    try {
        sys::messenger_registry::register(name, 0, [], 0, []);
        one = "SUCCEEDED:registered";
    } catch (e) {
        let text = e.to_string();
        if text.contains("(EACCES") {
            one = "BLOCKED:EACCES";
        } else if text.contains("(EBADF") || text.contains("(EINVAL") {
            one = "SUCCEEDED:policy-passed";
        } else {
            one = "ERROR:unrecognised";
        }
    }
    if !one.starts_with("BLOCKED") {
        verdict = one;
        break;
    }
}
print("ACCT:ATTACK:nic_register:" + verdict);
