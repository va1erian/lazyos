// title: Service watchdog with toasts (run 1: watch)
// tags: do, init, notify
// needs: second run
// expect: watching
//
// Run 1 turns service supervision events into notifications; run 2
// (11b_service_watchdog_trigger) makes one happen. Start this one first and
// leave it running; press Stop when you are done.
print("watching");
sys::init::on_service(|e| sys::notify::notify("Warn", `service ${e.topic}`, `${e.payload.state} (restarts: ${e.payload.restarts})`, (), [], (), (), ()));
msg::run();
