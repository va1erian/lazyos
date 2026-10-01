//! Host-side tests: every binding against the in-memory mock, including the
//! failure paths (missing file, bad UTF-8, permission denied, limits, runaway
//! loops, a closed stdout).

use alloc::rc::Rc;
use alloc::string::{String, ToString};

use crate::mock::MockHost;
use crate::{build_engine, eval_source, Config, Outcome, Scope};

mod engine_tests;
mod msg_mock;
mod msg_tests;
mod os_tests;
mod repl_tests;

#[test]
fn engine_version_matches_the_exact_pin() {
    let manifest = include_str!("../../Cargo.toml");
    let pin = alloc::format!("version = \"={}\"", crate::ENGINE_VERSION);
    assert!(manifest.contains(&pin), "Cargo.toml must pin {pin}");
}

/// Run `source` with default limits; returns the outcome and the mock.
pub(crate) fn run(host: MockHost, source: &str) -> (Outcome, Rc<MockHost>) {
    run_with(host, &Config::default(), source)
}

pub(crate) fn run_with(host: MockHost, config: &Config, source: &str) -> (Outcome, Rc<MockHost>) {
    let host = Rc::new(host);
    let engine = build_engine(host.clone(), config);
    let outcome = eval_source(&engine, &mut Scope::new(), source);
    (outcome, host)
}

/// The value of a successful run, rendered like `rhai -e` prints it.
pub(crate) fn value(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Value(v) => v.to_string(),
        other => panic!("expected a value, got {other:?}"),
    }
}

/// The message of a failed run.
pub(crate) fn failure(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Failed(message) => message.clone(),
        other => panic!("expected a failure, got {other:?}"),
    }
}
