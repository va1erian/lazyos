//! `t_time` — `Instant`/`SystemTime` (`clock_gettime`).

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn main() {
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(1));
    let elapsed = start.elapsed();

    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    let ok = elapsed >= Duration::from_micros(100) && since_epoch.as_secs() > 0;
    common::report("time", ok, "clock did not advance");
}
