// Attack: write confd's system settings (sys/**) as the session user.
// Prints one ACCT:ATTACK line; the harness judges it (tools/accounts/).
let verdict = "SUCCEEDED:ok";
try {
    let v = sys::confd::new_value();
    v.kind = 3;
    v.str_value = "attack";
    sys::confd::set("sys/accounts/probe", v);
    sys::confd::delete("sys/accounts/probe");
} catch (e) {
    let text = e.to_string();
    if text.len() > 40 { text = text.sub_string(0, 40); }
    verdict = "BLOCKED:" + text.replace(" ", "_");
}
print("ACCT:ATTACK:confd_sys:" + verdict);
