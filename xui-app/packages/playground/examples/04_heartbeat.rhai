// title: Heartbeat
// tags: listen, topics, time
// needs: none
// expect: tick 1700000000 (Europe/Paris)
//
// The clock service publishes a retained time/tick each minute (and when the
// zone or the clock changes), so the first line arrives at once. Watch for ten
// seconds.
sys::timed::on_tick(|e| print(`tick ${e.payload.unix} (${e.payload.zone_name})`));
msg::run(10000);
