//! The handoff against a scripted transport: what it asks, in which order, and
//! how it reads the answers.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use lazyrad_packager::lzp::{build_package, PackageRequest};
use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use messenger_generated::os_lazy_mimed_v1 as mimed;

use super::*;
use crate::transport::Wait;

type Answer = Result<Vec<u8>, Failure>;

/// One scripted answer per call, in order (then `when_empty`); records what was
/// asked. The clock advances one tick per call and by the pause on a sleep.
struct Mock {
    answers: RefCell<VecDeque<Answer>>,
    when_empty: Option<Failure>,
    asked: RefCell<Vec<(&'static str, u32, Vec<u8>)>>,
    now: Cell<u64>,
}

impl Mock {
    fn new(answers: Vec<Answer>) -> Mock {
        Mock {
            answers: RefCell::new(answers.into()),
            when_empty: None,
            asked: RefCell::new(Vec::new()),
            now: Cell::new(0),
        }
    }

    /// The services asked so far, in order, as `service#method`.
    fn trail(&self) -> Vec<String> {
        self.asked
            .borrow()
            .iter()
            .map(|(service, method, _)| format!("{service}#{method}"))
            .collect()
    }
}

impl Transport for &Mock {
    fn call(
        &self,
        service: &'static str,
        _interface: u64,
        method: u32,
        body: Vec<u8>,
        _wait: Wait,
    ) -> Answer {
        self.now.set(self.now.get() + 1);
        self.asked.borrow_mut().push((service, method, body));
        match self.answers.borrow_mut().pop_front() {
            Some(answer) => answer,
            None => Err(self.when_empty.clone().expect("an unscripted call")),
        }
    }

    fn ticks(&self) -> u64 {
        self.now.get()
    }

    fn pause(&self, millis: u64) {
        self.now.set(self.now.get() + millis.div_ceil(10));
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lazyrad-os-handoff-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A real package built by LazyRAD's packager from a one-module project.
fn package(dir: &Path) -> BuiltPackage {
    let project = dir.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("todo.lrp"),
        "name = \"todo\"\nversion = \"1.0\"\nstartup = \"m0\"\n\n[[items]]\nkind = \"module\"\n\
         name = \"m0\"\ncode = \"m0.rhai\"\n",
    )
    .unwrap();
    fs::write(project.join("m0.rhai"), "fn f() { 1 }\n").unwrap();
    let mut player = vec![0u8; 128];
    player[..4].copy_from_slice(b"\x7fELF");
    player[4] = 2;
    player[5] = 1;
    player[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
    build_package(&PackageRequest {
        project: &project.join("todo.lrp"),
        player: &player,
        author: "Ada",
        system_name: None,
        description: Some("handoff test"),
        icons: None,
        check: None,
        permissions: None,
    })
    .unwrap()
}

fn digest_of(package: &BuiltPackage) -> String {
    HandoffInstaller::<&Mock>::assess(package).digest
}

fn ok(body: Result<Vec<u8>, libmessenger::Error>) -> Answer {
    Ok(body.unwrap())
}

fn subscribed() -> Answer {
    ok(topics::encode_subscribe_reply(&topics::SubscribeReply {
        subscription: 7,
    }))
}

fn opened(launched: bool) -> Answer {
    ok(mimed::encode_open_reply(&mimed::OpenReply {
        app: "installer".into(),
        mime: "application/x-lazyos-package".into(),
        launched,
        ..Default::default()
    }))
}

fn nothing_yet() -> Answer {
    Err(Failure {
        code: -110,
        text: "timed out".into(),
    })
}

fn event(op: &str, package: &BuiltPackage, digest: &str, ok_: bool, detail: &str) -> Answer {
    // As the broker carries it: inside `central`'s wrapper parcel.
    let inner = pkgd::encode_pkg_event(&pkgd::PkgEvent {
        op: op.into(),
        system_name: package.system_name.clone(),
        version: package.version.clone(),
        install_dir: format!("{}/{}-abcd1234", package.system_name, package.version),
        digest: digest.into(),
        ok: ok_,
        detail: detail.into(),
        ..Default::default()
    })
    .unwrap();
    let payload = rhai_lazy::msg::topics::wrap(&inner).unwrap();
    ok(topics::encode_next_event_reply(&topics::NextEventReply {
        event: topics::Event {
            topic: format!("system/events/pkg/{op}"),
            payload,
            ..Default::default()
        },
    }))
}

fn unsubscribed() -> Answer {
    Ok(Vec::new())
}

fn refusal(error: InstallError) -> String {
    let InstallError::Refused(lines) = error else {
        panic!("expected a refusal, got {error:?}");
    };
    lines.join("\n")
}

#[test]
fn review_is_made_in_process_and_asks_no_service() {
    let dir = scratch("review");
    let mock = Mock::new(Vec::new());
    let built = package(&dir);
    let review = HandoffInstaller::new(&mock, dir.clone(), 100)
        .review(&built)
        .unwrap()
        .expect("a review");
    assert_eq!(review.system_name, built.system_name);
    assert_eq!(review.version, built.version);
    assert!(!review.permissions.is_empty(), "the display at least");
    assert!(review.problems.is_empty(), "{:?}", review.problems);
    assert!(mock.asked.borrow().is_empty(), "nothing left the process");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "only the project");
}

#[test]
fn install_hands_the_package_to_the_installer_and_follows_its_event() {
    let dir = scratch("install");
    let built = package(&dir);
    let digest = digest_of(&built);
    let mock = Mock::new(vec![
        subscribed(),
        opened(true),
        nothing_yet(),
        // Someone else's record, then ours.
        event("install", &built, "not-this-package", true, ""),
        event("install", &built, &digest, true, ""),
        unsubscribed(),
    ]);
    let app = HandoffInstaller::new(&mock, dir.clone(), 1000)
        .install(&built)
        .unwrap();
    assert_eq!(app.state, InstallState::Installed);
    assert_eq!(app.system_name, built.system_name);
    assert_eq!(
        app.location.unwrap(),
        PathBuf::from(fhs::state::APPS_ROOT).join(format!("{}/1.0.0-abcd1234", built.system_name))
    );
    // Subscribed before the Installer was started, polled, then released.
    let broker = |method: u32| format!("{BROKER_NAME}#{method}");
    assert_eq!(
        mock.trail(),
        [
            broker(topics::METHOD_SUBSCRIBE),
            format!("{MIMED_NAME}#{}", mimed::METHOD_OPEN),
            broker(topics::METHOD_NEXTEVENT),
            broker(topics::METHOD_NEXTEVENT),
            broker(topics::METHOD_NEXTEVENT),
            broker(topics::METHOD_UNSUBSCRIBE),
        ]
    );
    let asked = mock.asked.borrow();
    let open = mimed::decode_open_args(&asked[1].2).unwrap();
    assert!(
        open.path.starts_with(dir.to_str().unwrap()),
        "{}",
        open.path
    );
    assert!(open
        .path
        .ends_with(&format!("lazyrad-{}-1.0.0.lzp", built.system_name)));
    assert_eq!(open.verb, INSTALL_VERB);
    let subscribe = topics::decode_subscribe_args(&asked[0].2).unwrap();
    assert_eq!(subscribe.filter, "system/events/pkg/+");
    // The staged file is gone afterwards.
    assert!(!Path::new(&open.path).exists());
}

#[test]
fn a_denied_install_is_the_installers_sentence() {
    let dir = scratch("denied");
    let built = package(&dir);
    let digest = digest_of(&built);
    let mock = Mock::new(vec![
        subscribed(),
        opened(true),
        event(
            "denied",
            &built,
            &digest,
            false,
            "that application is already installed",
        ),
        unsubscribed(),
    ]);
    let error = HandoffInstaller::new(&mock, dir, 1000)
        .install(&built)
        .unwrap_err();
    assert_eq!(
        refusal(error),
        "This version of the app is already installed."
    );
}

#[test]
fn a_failed_install_event_is_a_refusal_with_its_detail() {
    let dir = scratch("failed");
    let built = package(&dir);
    let digest = digest_of(&built);
    let mock = Mock::new(vec![
        subscribed(),
        opened(true),
        event(
            "install",
            &built,
            &digest,
            false,
            "the system volume is not writable",
        ),
        unsubscribed(),
    ]);
    let error = HandoffInstaller::new(&mock, dir, 1000)
        .install(&built)
        .unwrap_err();
    assert_eq!(refusal(error), "the system volume is not writable");
}

#[test]
fn an_installer_that_never_answers_ends_the_wait_with_a_refusal() {
    let dir = scratch("silent");
    let built = package(&dir);
    let mut mock = Mock::new(vec![subscribed(), opened(true)]);
    mock.when_empty = Some(Failure {
        code: -110,
        text: "timed out".into(),
    });
    let error = HandoffInstaller::new(&mock, dir.clone(), 50)
        .install(&built)
        .unwrap_err();
    let text = refusal(error);
    assert!(text.contains("reported nothing"), "{text}");
    assert!(text.contains("cancelled"), "{text}");
    // The wait was bounded by the clock, not by a call count.
    assert!(mock.now.get() >= 50, "{}", mock.now.get());
    assert!(mock
        .trail()
        .last()
        .unwrap()
        .ends_with(&format!("#{}", topics::METHOD_UNSUBSCRIBE)));
    assert_eq!(
        fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "lzp")
            })
            .count(),
        0,
        "the staged file is removed"
    );
}

#[test]
fn an_installer_that_did_not_start_is_reported() {
    let dir = scratch("nostart");
    let built = package(&dir);
    let mock = Mock::new(vec![subscribed(), opened(false), unsubscribed()]);
    let error = HandoffInstaller::new(&mock, dir, 1000)
        .install(&built)
        .unwrap_err();
    assert!(refusal(error).contains("could not be started"));
}

#[test]
fn a_package_with_problems_is_refused_before_anything_is_staged_or_asked() {
    let dir = scratch("problems");
    let mock = Mock::new(Vec::new());
    let mut broken = package(&dir);
    broken.bytes = b"PK-not-an-archive".to_vec();
    let installer = HandoffInstaller::new(&mock, dir.clone(), 100);
    let review = installer.review(&broken).unwrap().unwrap();
    assert!(!review.problems.is_empty());
    assert!(installer.install(&broken).is_err());
    assert!(mock.asked.borrow().is_empty());
    assert!(fs::read_dir(&dir).unwrap().all(|e| e
        .unwrap()
        .path()
        .extension()
        .is_none_or(|x| x != "lzp")));
}

#[test]
fn an_oversized_package_is_a_problem() {
    let dir = scratch("big");
    let mock = Mock::new(Vec::new());
    let mut big = package(&dir);
    big.bytes.resize(MAX_PACKAGE_BYTES + 1, 0);
    let review = HandoffInstaller::new(&mock, dir, 100)
        .review(&big)
        .unwrap()
        .unwrap();
    assert!(
        review.problems.iter().any(|p| p.contains("reads at most")),
        "{:?}",
        review.problems
    );
}

#[test]
fn an_unwritable_staging_directory_is_a_clear_refusal() {
    let dir = scratch("unwritable");
    let built = package(&dir);
    let mock = Mock::new(Vec::new());
    let installer = HandoffInstaller::new(&mock, PathBuf::from("/nonexistent/stage"), 100);
    let error = installer.install(&built).unwrap_err();
    assert!(error.to_string().contains("could not be written"));
    assert!(mock.asked.borrow().is_empty());
}

#[test]
fn launch_calls_init_with_the_system_name_and_no_extra_args() {
    let mock = Mock::new(vec![Ok(Vec::new())]);
    HandoffInstaller::new(&mock, scratch("launch"), 100)
        .launch("user.ada.todo")
        .unwrap();
    let asked = mock.asked.borrow();
    assert_eq!(asked[0].0, INIT_NAME);
    assert_eq!(asked[0].1, init_wire::METHOD_LAUNCH);
    let args = init_wire::decode_launch_args(&asked[0].2).unwrap();
    assert_eq!(args.app, "user.ada.todo");
    assert_eq!(args.args, "");
    assert_eq!(args.session, 0);
}

#[test]
fn a_garbled_reply_is_an_error_not_a_panic() {
    let dir = scratch("garbled");
    let built = package(&dir);
    let mock = Mock::new(vec![Ok(vec![0xff, 0xff, 0xff])]);
    assert!(HandoffInstaller::new(&mock, dir, 100)
        .install(&built)
        .is_err());
}

#[test]
fn failures_become_friendly_sentences() {
    let service_down = Failure {
        code: -32,
        text: "os.lazy.mimed is not running".into(),
    };
    assert!(friendly(&service_down).contains("not available"));
    let plain = Failure {
        code: 2,
        text: "package not found".into(),
    };
    assert_eq!(friendly(&plain), "package not found");
}
