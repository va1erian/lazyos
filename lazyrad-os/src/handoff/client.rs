//! The service calls the install handoff makes, over a [`Transport`], with the
//! `midlc`-generated stubs only (`idl/mimed.midl`, `idl/init.midl`,
//! `idl/pkgd.midl`): no field id or method number is written here.

use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_mimed_v1 as mimed;

use crate::transport::{Failure, PkgEvents, Transport, Wait};

/// The registered service names (not the `.vN` interface names), declared by
/// `service` in the `.midl` files.
pub const MIMED_NAME: &str = mimed::SERVICE_NAME;
/// `init`'s service name.
pub const INIT_NAME: &str = init_wire::SERVICE_NAME;

/// The calls the IDE makes, over a [`Transport`].
pub struct Client<T: Transport> {
    pub(super) transport: T,
}

impl<T: Transport> Client<T> {
    /// A client over `transport`.
    pub fn new(transport: T) -> Client<T> {
        Client { transport }
    }

    /// `mimed.Open(path, verb)`: `mimed` resolves the app registered for the
    /// verb and asks `init` to launch it in this caller's session.
    pub fn open(&self, path: &str, verb: &str) -> Result<mimed::OpenReply, Failure> {
        let body = mimed::encode_open_args(&mimed::OpenArgs {
            path: path.to_owned(),
            verb: verb.to_owned(),
        })
        .map_err(|_| bad_request())?;
        let reply = self.transport.call(
            MIMED_NAME,
            mimed::INTERFACE_ID,
            mimed::METHOD_OPEN,
            body,
            Wait::Forever,
        )?;
        mimed::decode_open_reply(&reply).map_err(|_| garbled("the file-type service"))
    }

    /// `init.Launch(system_name)` in the caller's own session.
    pub fn launch(&self, system_name: &str) -> Result<(), Failure> {
        let body = init_wire::encode_launch_args(&init_wire::LaunchArgs {
            app: system_name.to_owned(),
            args: String::new(),
            session: 0,
        })
        .map_err(|_| bad_request())?;
        self.transport
            .call(
                INIT_NAME,
                init_wire::INTERFACE_ID,
                init_wire::METHOD_LAUNCH,
                body,
                Wait::Forever,
            )
            .map(|_| ())
    }

    /// Follows `system/events/pkg/+`, the package manager's audit events, from
    /// now on: made before the Installer starts, so its event cannot be missed.
    pub fn follow_pkg_events(&self) -> Box<dyn PkgEvents + '_> {
        self.transport.follow_pkg_events()
    }
}

fn bad_request() -> Failure {
    Failure {
        code: 22,
        text: "the request could not be encoded".to_owned(),
    }
}

fn garbled(who: &str) -> Failure {
    Failure {
        code: -22,
        text: format!("{who} sent a reply this program cannot read"),
    }
}
