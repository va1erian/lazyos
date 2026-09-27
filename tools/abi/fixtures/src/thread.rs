//! `t_thread` — `std::thread` + `Mutex` (`clone`/`futex`).

mod common;

use std::sync::{Arc, Mutex};

fn main() {
    let counter = Arc::new(Mutex::new(0u64));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let counter = Arc::clone(&counter);
            std::thread::spawn(move || {
                for _ in 0..1000 {
                    *counter.lock().unwrap() += 1;
                }
            })
        })
        .collect();

    for handle in handles {
        if handle.join().is_err() {
            common::fail("thread", "join failed");
        }
    }

    let total = *counter.lock().unwrap();
    common::report("thread", total == 4000, "lost increments");
}
