// title: Service watchdog with toasts (run 2: trigger)
// tags: do, init, elevd
// needs: second run, admin login
// after: 11_service_watchdog
// expect: asked elevd to restart inputd
//
// Run 2 makes a service restart. Restarting a service is a privileged change:
// it goes through elevd, which shows the trusted prompt. Approve it as
// admin / nimda. Without an administrator it is refused and the error is
// printed.
try {
    sys::elevd::request("service.restart", ["inputd"]);
    print("asked elevd to restart inputd");
} catch (e) {
    print(`refused: ${e}`);
}
