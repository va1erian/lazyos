//! `flaky` (`/system/bin/flaky`): the supervisor's crash-test service (issue #93).
//!
//! `init` starts this program through the native `spawn` syscall and appends
//! `attempt=<n>` to its manifest arguments. On the first attempt it parks for a
//! short while and exits with status 3, which exercises the supervisor's
//! wait/reap/backoff/restart path in a plain boot log; from the second attempt
//! on it reports itself healthy to `healthd` and idles forever, so the same log
//! shows the service recovered.
//!
//! The program is tiny on purpose: it is evidence, not a real service.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::panic::PanicInfo;
use user::messenger::{self, services, Error};
use user::sys;

/// PIT ticks the first attempt stays up before crashing (100 Hz).
const CRASH_DELAY_TICKS: u64 = 20;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let args = service_args();
    let attempt = parse_attempt(&args);
    sys::write_str(&format!("flaky: starting (attempt {attempt})\n"));

    if attempt <= 1 {
        // Park on a channel we never write to; a deadline wake is the interim
        // sleep until a native sleep syscall lands.
        if let Ok((_keep, waiter)) = messenger::create_pair() {
            let _ = waiter.recv(Some(sys::clock() + CRASH_DELAY_TICKS));
        }
        sys::write_str("flaky: crashing on purpose (first attempt)\n");
        sys::exit(3);
    }

    report_healthy(attempt);
    idle_forever()
}

/// Read this task's manifest argument string.
fn service_args() -> alloc::string::String {
    let mut buffer = [0u8; 128];
    let length = sys::service_args(&mut buffer).min(buffer.len());
    alloc::string::String::from_utf8_lossy(&buffer[..length]).into_owned()
}

/// The `attempt=<n>` supervisor marker (1 when absent).
fn parse_attempt(args: &str) -> u64 {
    for token in args.split_whitespace() {
        if let Some(value) = token.strip_prefix("attempt=") {
            return value.parse().unwrap_or(1);
        }
    }
    1
}

/// Publish `health/flaky` to `healthd`, retrying while it is still coming up.
fn report_healthy(attempt: u64) {
    let Ok(endpoint) = services::resolve_service(services::HEALTHD_NAME) else {
        return;
    };
    let detail = format!("crash test recovered after attempt {attempt}");
    let Ok(request) = services::health_report_request("flaky", "ok", &detail) else {
        return;
    };
    for _ in 0..5u32 {
        if endpoint.call(&request, Some(sys::clock() + 10)).is_ok() {
            sys::write_str("flaky: reported healthy to healthd\n");
            return;
        }
    }
}

/// Idle in deadline sleeps; the interesting work happens in the boot log.
fn idle_forever() -> ! {
    let Ok((_keep, waiter)) = messenger::create_pair() else {
        sys::exit(0);
    };
    // Reused buffer: the user bump allocator never reclaims per-call buffers.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        // A timeout is the expected outcome; any other error means the keep
        // end died with the address space, which cannot happen here.
        if let Err(Error::Errno(code)) = waiter.recv_with(&mut buffer, Some(sys::clock() + 50)) {
            if code != -messenger::errno::ETIMEDOUT {
                break;
            }
        }
    }
    sys::exit(0)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
