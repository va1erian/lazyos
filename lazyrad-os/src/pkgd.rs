//! The `pkgd` client: LazyRAD's [`Installer`] on LazyOS (plan P4).
//!
//! `pkgd` (`docs/packages.md` §7) is the only task that installs apps. This
//! module is an unprivileged client of `os.lazy.pkgd.v1`: it writes the built
//! `.lzp` where `pkgd` may read it, calls `Inspect` so the IDE can show the user
//! every permission and every problem, and only then `Install`. It starts an
//! installed app with `os.lazy.init.Launch`, which is how the Start menu does it.
//! Every body is built and read with the `midlc`-generated stubs
//! (`messenger-generated`); no field id or method number is written here.
//!
//! # Where the package goes
//!
//! `pkgd` reads a package *as root*, so for an unprivileged caller it only
//! accepts the boot volume root, `/tmp` and the caller's own home
//! (`libs/pkgstore/src/access.rs`). `/data/packages` (the dev fallback's
//! directory) is therefore **not** readable by `pkgd` for a normal user. The
//! installer stages the file as `/tmp/lazyrad-<system_name>-<version>.lzp`,
//! calls `pkgd`, and deletes it afterwards. `Inspect` reads at most 8 MiB, so a
//! package larger than that is refused here with a clear message instead of
//! staging it.
//!
//! The call blocks the thread that makes it (the IDE's UI thread): an install
//! extracts the package, so the window does not repaint until it returns.
//!
//! [`Transport`] is the seam: [`MessengerTransport`] talks to the real services,
//! tests script a mock.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;

use lazyrad_packager::lzp::{
    BuiltPackage, InstallError, InstallState, InstalledApp, Installer, PackageReview,
    PermissionNote,
};
use libmessenger::{Decoder, Header, Kind, Parcel, VERSION};
use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_pkgd_v1 as wire;
use xui_app::sys::{self, errno};

/// The registered service names (not the `.vN` interface names).
pub const PKGD_NAME: &str = "os.lazy.pkgd";
/// `init`'s service name.
pub const INIT_NAME: &str = "os.lazy.init";
/// The structured-error field services reply with.
const ERROR_FIELD: u16 = 15;
/// The reply buffer offered to a call.
const REPLY_BUF: usize = 64 * 1024;
/// How long to wait for a service to appear: 10 s at 100 Hz.
const CONNECT_TICKS: u64 = 1000;
/// `pkgd`'s `Inspect` refuses a file larger than this.
pub const MAX_PACKAGE_BYTES: usize = 8 * 1024 * 1024;
/// Where packages are staged for `pkgd` (readable by design).
pub const STAGING_DIR: &str = "/tmp";

/// A refused or failed call: the service's own sentence, or the transport's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Positive errno-style code from a service, or the kernel's error negated
    /// for a transport failure.
    pub code: i64,
    /// Friendly text, ready to show.
    pub text: String,
}

/// One request/reply exchange with a Messenger service.
pub trait Transport {
    /// Calls `method` of `interface` on the service registered as `service` and
    /// returns the reply body.
    fn call(
        &self,
        service: &'static str,
        interface: u64,
        method: u32,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, Failure>;
}

/// The real transport: resolve once, keep the endpoint open for the life of the
/// task (closing a resolved handle is peer death for the service), and read the
/// structured error field, text included, from a refusal.
#[derive(Default)]
pub struct MessengerTransport {
    endpoints: RefCell<Vec<(&'static str, u64)>>,
}

impl MessengerTransport {
    fn endpoint(&self, name: &'static str) -> Result<u64, Failure> {
        if let Some((_, endpoint)) = self.endpoints.borrow().iter().find(|(n, _)| *n == name) {
            return Ok(*endpoint);
        }
        let deadline = sys::clock_ticks().saturating_add(CONNECT_TICKS);
        loop {
            match sys::msg_resolve(name) {
                Ok(endpoint) => {
                    self.endpoints.borrow_mut().push((name, endpoint));
                    return Ok(endpoint);
                }
                Err(code) if sys::clock_ticks() >= deadline => {
                    return Err(Failure {
                        code: -code.abs(),
                        text: format!("{name} is not running"),
                    });
                }
                Err(_) => sys::sleep_millis(10),
            }
        }
    }
}

impl Transport for MessengerTransport {
    fn call(
        &self,
        service: &'static str,
        interface: u64,
        method: u32,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, Failure> {
        let endpoint = self.endpoint(service)?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                // The IDE's own event receive must not trip the kernel's
                // per-channel cycle check while a call is in flight.
                flags: libmessenger::flags::ALLOW_NESTED,
                interface_id: interface,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = vec![0u8; REPLY_BUF];
        let reply = sys::msg_call(endpoint, &parcel, &mut buf, 0).map_err(|code| {
            if code == -errno::EPIPE || code == -errno::ENOENT {
                self.endpoints.borrow_mut().retain(|(n, _)| *n != service);
            }
            Failure {
                code,
                text: format!("{service} did not answer (error {code})"),
            }
        })?;
        match failure_of(&reply.body) {
            Some(failure) => Err(failure),
            None => Ok(reply.body),
        }
    }
}

/// The structured error a reply body carries, if it is one.
fn failure_of(body: &[u8]) -> Option<Failure> {
    let mut decoder = Decoder::new(body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
            let (code, text) = field.error_parts().ok()?;
            return Some(Failure {
                code: i64::from(code),
                text: text.to_owned(),
            });
        }
    }
    None
}

/// The friendly sentence for a failure: `pkgd`'s own text with the context a
/// person needs to act on it.
pub fn friendly(failure: &Failure) -> String {
    const EPERM: i64 = 1;
    const EACCES: i64 = 13;
    const EEXIST: i64 = 17;
    let text = failure.text.as_str();
    let lower = text.to_lowercase();
    if failure.code < 0 {
        return format!("The package manager (pkgd) is not available: {text}");
    }
    if lower.contains("no writable data disk") || lower.contains("data volume") {
        return "There is no writable data disk, so apps cannot be installed. Attach a data \
                disk and try again."
            .to_owned();
    }
    match failure.code {
        EPERM | EACCES => format!(
            "Only the person logged in on this desktop can install apps; this program is not \
             allowed to ({text})."
        ),
        EEXIST => "This version of the app is already installed.".to_owned(),
        _ => text.to_owned(),
    }
}

/// The calls the IDE needs, over a [`Transport`].
pub struct PkgdClient<T: Transport> {
    transport: T,
}

impl<T: Transport> PkgdClient<T> {
    /// A client over `transport`.
    pub fn new(transport: T) -> PkgdClient<T> {
        PkgdClient { transport }
    }

    fn pkgd(&self, method: u32, body: Vec<u8>) -> Result<Vec<u8>, Failure> {
        self.transport
            .call(PKGD_NAME, wire::INTERFACE_ID, method, body)
    }

    /// `Inspect(path)`.
    pub fn inspect(&self, path: &str) -> Result<wire::PackageInfo, Failure> {
        let body = wire::encode_inspect_args(&wire::InspectArgs {
            path: path.to_owned(),
        })
        .map_err(|_| bad_path())?;
        let reply = self.pkgd(wire::METHOD_INSPECT, body)?;
        wire::decode_inspect_reply(&reply)
            .map(|reply| reply.info)
            .map_err(|_| garbled())
    }

    /// `Install(path)`.
    pub fn install(&self, path: &str) -> Result<wire::Installed, Failure> {
        let body = wire::encode_install_args(&wire::InstallArgs {
            path: path.to_owned(),
        })
        .map_err(|_| bad_path())?;
        let reply = self.pkgd(wire::METHOD_INSTALL, body)?;
        wire::decode_install_reply(&reply)
            .map(|reply| reply.app)
            .map_err(|_| garbled())
    }

    /// `init.Launch(system_name)` in the caller's own session.
    pub fn launch(&self, system_name: &str) -> Result<(), Failure> {
        let body = init_wire::encode_launch_args(&init_wire::LaunchArgs {
            app: system_name.to_owned(),
            args: String::new(),
            session: 0,
        })
        .map_err(|_| bad_path())?;
        self.transport
            .call(
                INIT_NAME,
                init_wire::INTERFACE_ID,
                init_wire::METHOD_LAUNCH,
                body,
            )
            .map(|_| ())
    }
}

fn bad_path() -> Failure {
    Failure {
        code: 22,
        text: "the request could not be encoded".to_owned(),
    }
}

fn garbled() -> Failure {
    Failure {
        code: -22,
        text: "the package manager sent a reply this program cannot read".to_owned(),
    }
}

/// LazyRAD's [`Installer`] over `pkgd`.
pub struct PkgdInstaller<T: Transport> {
    client: PkgdClient<T>,
    staging: PathBuf,
}

impl PkgdInstaller<MessengerTransport> {
    /// The installer on the real services, staging in [`STAGING_DIR`].
    pub fn on_lazyos() -> Self {
        PkgdInstaller::new(MessengerTransport::default(), PathBuf::from(STAGING_DIR))
    }
}

impl<T: Transport> PkgdInstaller<T> {
    /// An installer over `transport` that stages packages in `staging`.
    pub fn new(transport: T, staging: PathBuf) -> Self {
        PkgdInstaller {
            client: PkgdClient::new(transport),
            staging,
        }
    }

    /// Writes the package where `pkgd` may read it; the file is removed when the
    /// returned guard drops.
    fn stage(&self, package: &BuiltPackage) -> Result<Staged, InstallError> {
        if package.bytes.len() > MAX_PACKAGE_BYTES {
            return Err(InstallError::Refused(vec![format!(
                "The package is {} bytes; the installer reads at most {MAX_PACKAGE_BYTES}.",
                package.bytes.len()
            )]));
        }
        let path = self.staging.join(format!(
            "lazyrad-{}-{}.lzp",
            package.system_name, package.version
        ));
        fs::write(&path, &package.bytes).map_err(|error| {
            InstallError::Refused(vec![format!(
                "The package could not be written to {}: {error}",
                path.display()
            )])
        })?;
        Ok(Staged(path))
    }
}

/// A staged package file, deleted on drop.
struct Staged(PathBuf);

impl Staged {
    fn path(&self) -> &str {
        self.0.to_str().unwrap_or_default()
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn refused(failure: &Failure) -> InstallError {
    InstallError::Refused(vec![friendly(failure)])
}

impl<T: Transport> Installer for PkgdInstaller<T> {
    fn review(&self, package: &BuiltPackage) -> Result<Option<PackageReview>, InstallError> {
        let staged = self.stage(package)?;
        let info = self
            .client
            .inspect(staged.path())
            .map_err(|f| refused(&f))?;
        Ok(Some(PackageReview {
            name: info.name,
            system_name: info.system_name,
            author: info.author,
            version: info.version,
            permissions: info
                .permissions
                .into_iter()
                .map(|p| PermissionNote {
                    kind: p.kind,
                    value: p.value,
                    risk: p.risk,
                    explanation: p.explanation,
                })
                .collect(),
            problems: info.problems,
        }))
    }

    fn install(&self, package: &BuiltPackage) -> Result<InstalledApp, InstallError> {
        let staged = self.stage(package)?;
        let app = self
            .client
            .install(staged.path())
            .map_err(|f| refused(&f))?;
        Ok(InstalledApp {
            system_name: app.system_name,
            version: app.version,
            state: InstallState::Installed,
            location: Some(PathBuf::from(fhs::state::APPS_ROOT).join(app.install_dir)),
        })
    }

    fn launch(&self, system_name: &str) -> Result<(), InstallError> {
        self.client.launch(system_name).map_err(|f| {
            InstallError::Refused(vec![format!(
                "The app could not be started: {}",
                friendly(&f)
            )])
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// One scripted answer per call, in order; records what was asked.
    struct Mock {
        answers: RefCell<Vec<Result<Vec<u8>, Failure>>>,
        asked: RefCell<Vec<(&'static str, u32, Vec<u8>)>>,
    }

    impl Mock {
        fn new(answers: Vec<Result<Vec<u8>, Failure>>) -> Mock {
            Mock {
                answers: RefCell::new(answers.into_iter().rev().collect()),
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl Transport for &Mock {
        fn call(
            &self,
            service: &'static str,
            _interface: u64,
            method: u32,
            body: Vec<u8>,
        ) -> Result<Vec<u8>, Failure> {
            self.asked.borrow_mut().push((service, method, body));
            self.answers.borrow_mut().pop().expect("an unscripted call")
        }
    }

    fn package() -> BuiltPackage {
        BuiltPackage {
            bytes: b"PK-package".to_vec(),
            system_name: "user.ada.todo".into(),
            version: "1.0.0".into(),
            manifest: String::new(),
            entries: 1,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazyrad-os-pkgd-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn info(problems: Vec<&str>) -> Vec<u8> {
        wire::encode_inspect_reply(&wire::InspectReply {
            info: wire::PackageInfo {
                name: "Todo".into(),
                system_name: "user.ada.todo".into(),
                author: "Ada".into(),
                version: "1.0.0".into(),
                permissions: vec![wire::Permission {
                    kind: "interface".into(),
                    value: "os.lazy.display.v1".into(),
                    risk: "low".into(),
                    explanation: "Show windows".into(),
                }],
                problems: problems.into_iter().map(String::from).collect(),
                ..Default::default()
            },
        })
        .unwrap()
    }

    fn installed() -> Vec<u8> {
        wire::encode_install_reply(&wire::InstallReply {
            app: wire::Installed {
                system_name: "user.ada.todo".into(),
                version: "1.0.0".into(),
                install_dir: "user.ada.todo/1.0.0-abcd1234".into(),
                ..Default::default()
            },
        })
        .unwrap()
    }

    fn failure(code: i64, text: &str) -> Failure {
        Failure {
            code,
            text: text.into(),
        }
    }

    #[test]
    fn review_stages_the_package_for_pkgd_and_surfaces_permissions_and_problems() {
        let dir = scratch("review");
        let mock = Mock::new(vec![Ok(info(vec!["version is not MAJOR.MINOR.PATCH"]))]);
        let installer = PkgdInstaller::new(&mock, dir.clone());
        let review = installer.review(&package()).unwrap().expect("a review");
        assert_eq!(review.system_name, "user.ada.todo");
        assert_eq!(review.permissions.len(), 1);
        assert_eq!(review.permissions[0].risk, "low");
        assert_eq!(review.problems, ["version is not MAJOR.MINOR.PATCH"]);
        // The request named the staged path under the readable-by-design dir.
        let asked = mock.asked.borrow();
        assert_eq!(asked[0].0, PKGD_NAME);
        assert_eq!(asked[0].1, wire::METHOD_INSPECT);
        let args = wire::decode_inspect_args(&asked[0].2).unwrap();
        assert!(args.path.starts_with(dir.to_str().unwrap()));
        assert!(args.path.ends_with("lazyrad-user.ada.todo-1.0.0.lzp"));
        // The staged file is gone afterwards.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn install_returns_the_install_directory_and_cleans_up() {
        let dir = scratch("install");
        let mock = Mock::new(vec![Ok(installed())]);
        let app = PkgdInstaller::new(&mock, dir.clone())
            .install(&package())
            .unwrap();
        assert_eq!(app.state, InstallState::Installed);
        assert_eq!(
            app.location.unwrap(),
            PathBuf::from("/data/apps").join("user.ada.todo/1.0.0-abcd1234")
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn refusals_become_friendly_messages() {
        let cases = [
            (failure(1, "denied"), "Only the person logged in"),
            (failure(13, "denied"), "Only the person logged in"),
            (failure(17, "already installed"), "already installed"),
            (
                failure(19, "there is no writable data disk"),
                "no writable data disk",
            ),
            (failure(2, "package not found"), "package not found"),
            (failure(-32, "os.lazy.pkgd is not running"), "not available"),
        ];
        for (failure, expect) in cases {
            let dir = scratch("refused");
            let mock = Mock::new(vec![Err(failure.clone())]);
            let error = PkgdInstaller::new(&mock, dir)
                .install(&package())
                .unwrap_err();
            let InstallError::Refused(lines) = error else {
                panic!("expected a refusal");
            };
            assert!(lines[0].contains(expect), "{failure:?} -> {lines:?}");
        }
    }

    #[test]
    fn an_oversized_package_is_refused_before_it_is_staged() {
        let dir = scratch("big");
        let mock = Mock::new(Vec::new());
        let mut big = package();
        big.bytes = vec![0; MAX_PACKAGE_BYTES + 1];
        let error = PkgdInstaller::new(&mock, dir.clone())
            .review(&big)
            .unwrap_err();
        assert!(matches!(error, InstallError::Refused(_)));
        assert!(mock.asked.borrow().is_empty(), "pkgd was never asked");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn an_unwritable_staging_directory_is_a_clear_refusal() {
        let mock = Mock::new(Vec::new());
        let installer = PkgdInstaller::new(&mock, PathBuf::from("/nonexistent/stage"));
        let error = installer.install(&package()).unwrap_err();
        assert!(error.to_string().contains("could not be written"));
    }

    #[test]
    fn launch_calls_init_with_the_system_name_and_no_extra_args() {
        let mock = Mock::new(vec![Ok(Vec::new())]);
        PkgdInstaller::new(&mock, scratch("launch"))
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
        let mock = Mock::new(vec![Ok(vec![0xff, 0xff, 0xff])]);
        assert!(PkgdInstaller::new(&mock, scratch("garbled"))
            .review(&package())
            .is_err());
    }

    #[test]
    fn the_error_field_text_is_extracted_from_a_reply_body() {
        let mut body = libmessenger::Encoder::new();
        body.error(ERROR_FIELD, 17, "that application is already installed")
            .unwrap();
        let failure = failure_of(&body.finish()).expect("an error field");
        assert_eq!(failure.code, 17);
        assert_eq!(failure.text, "that application is already installed");
        assert!(failure_of(&info(vec![])).is_none());
    }
}
