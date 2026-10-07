// The admin_lockout scenario's background half (review of #659, H5): keep
// failing Authenticate("admin", ...) as the session for two minutes, once
// every 200 ms. Before the fix every failure counted against the name
// admin, so this kept admin locked for logind and elevd; now it counts
// against the session's own uid only. Prints nothing (attack.sh discards
// its output either way).
let accounts = msg::connect("os.lazy.accounts.v1", "os.lazy.accountsd");
let start = os::clock();
let i = 0;
while os::clock() - start < 120.0 {
    try {
        accounts.invoke("Authenticate", ["admin", "wrong-guess-" + i]);
    } catch (e) {
    }
    i += 1;
    os::sleep(200);
}
