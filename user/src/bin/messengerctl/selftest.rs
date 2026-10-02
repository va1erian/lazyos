//! Boot-time self-tests and their machine-parseable markers.

use alloc::format;
use alloc::string::String;
use user::messenger::{self, keyd, topics_client};
use user::sys;

use super::topic_tests::{
    selftest_drop, selftest_fanout, selftest_qos, selftest_retained, selftest_unsubscribe,
    selftest_wildcard, test_parcel,
};

/// The boot-time topic conformance markers (issue #92). Silent when no broker
/// is reachable, so the plain `LAZYOS_MESSENGERCTL=1` demo is unchanged.
pub(crate) fn topic_selftest() {
    let client = match topics_client::Client::connect() {
        Ok(client) => client,
        Err(_) => return,
    };
    sys::write_str("messengerctl: topics broker detected; running selftest\n");
    marker("TOPIC:FANOUT", selftest_fanout(&client));
    marker("TOPIC:WILDCARD", selftest_wildcard(&client));
    marker("TOPIC:RETAINED", selftest_retained(&client));
    marker("TOPIC:DROP", selftest_drop(&client));
    marker("TOPIC:QOS", selftest_qos(&client));
    marker("TOPIC:UNSUB", selftest_unsubscribe(&client));
    marker("TOPIC:SECURITY", selftest_security());
}

/// PIT ticks [`selftest_security`] waits for its probe child to exit.
const SECURITY_PROBE_TICKS: u64 = 200;

/// Negative test (issue #180): a non-root task's publish under the broker's
/// reserved `system/` root must be refused, or a compromised or malicious
/// client could forge audit records under `logd`'s trusted `system/events/#`
/// feed. This process is normally uid 0, and `sys::cred_set` cannot restore a
/// dropped credential, so the probe runs in a throwaway child spawned with a
/// demoted identity (`run_forbidden_publish_probe`) instead of in this one.
fn selftest_security() -> Result<(), String> {
    let mut command = format!("{} probe=forbidden-publish", fhs::bin::MESSENGERCTL).into_bytes();
    command.push(0);
    let cred = sys::Cred::new(4200, 4200, 0, 0, 0);
    let pid = sys::spawn_as(&command, &cred).ok_or("spawn_as failed")?;
    let deadline = sys::clock() + SECURITY_PROBE_TICKS;
    loop {
        match sys::wait(deadline) {
            Some((child, status)) if child == pid => {
                return if status == 0 {
                    Ok(())
                } else {
                    Err(format!(
                        "non-root publish under system/ was not refused (status {status})"
                    ))
                };
            }
            // A different child (unrelated to this probe): keep waiting.
            Some(_) => continue,
            None => return Err(String::from("probe child timed out")),
        }
    }
}

/// Child entry point for [`selftest_security`]'s probe, spawned under a
/// demoted, non-root credential. Attempts one publish under the broker's
/// reserved `system/` root and exits `0` when the broker refused it (the
/// expected, secure outcome) or `1` otherwise (including any error that
/// prevented running the check at all, which must not be mistaken for a
/// pass). Never returns.
pub(crate) fn run_forbidden_publish_probe() -> ! {
    let denied = match topics_client::Client::connect() {
        Ok(client) => match test_parcel("forbidden") {
            Ok(payload) => matches!(
                client.publish("system/events/selftest/forbidden", &payload),
                Err(error) if error.errno() == Some(-messenger::errno::EACCES)
            ),
            Err(_) => false,
        },
        Err(_) => false,
    };
    sys::exit(if denied { 0 } else { 1 })
}

/// Print `TOPIC:<name>:PASS` or `TOPIC:<name>:FAIL:<detail>`.
fn marker(name: &str, outcome: Result<(), String>) {
    match outcome {
        Ok(()) => sys::write_str(&format!("{name}:PASS\n")),
        Err(detail) => sys::write_str(&format!("{name}:FAIL:{detail}\n")),
    }
}

/// The boot-time `keyd` reachability marker: list the service's keys and print
/// `KEYD:KEYS:PASS <count>`. `init` starts `keyd` alongside this tool, so a
/// short retry covers the registration race; a boot with no `keyd` is silent,
/// exactly like [`topic_selftest`].
pub(crate) fn keyd_selftest() {
    // keyd's Argon2id self-test can take seconds under TCG, so the retry
    // window is generous (~0.6 s of parked ticks); `KEYD:SELFTEST:PASS` from
    // the service itself is the authoritative marker, this one is a bonus.
    const ATTEMPTS: usize = 64;
    for _ in 0..ATTEMPTS {
        match keyd::Client::connect() {
            Ok(client) => {
                match client.keys() {
                    Ok(keys) => sys::write_str(&format!("KEYD:KEYS:PASS {}\n", keys.len())),
                    Err(error) => sys::write_str(&format!("KEYD:KEYS:FAIL:{}\n", error.message())),
                }
                return;
            }
            Err(_) => park_tick(),
        }
    }
}

/// Sleep one PIT tick by parking on a private channel pair with an expired
/// deadline (userspace has no sleep syscall; the topics client uses the same
/// trick). The pair is closed again so no channel leaks.
pub(super) fn park_tick() {
    if let Ok((probe, peer)) = messenger::create_pair() {
        let mut scratch = [0u8; 16];
        let _ = probe.recv_into(&mut scratch, Some(messenger::EXPIRED_DEADLINE));
        let _ = probe.close();
        let _ = peer.close();
    }
}
