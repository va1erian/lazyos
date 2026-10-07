//! The privileged errands `init` runs for the account services
//! (docs/accounts-plan.md U1, U2): making, archiving and removing homes for
//! `accountsd` (`Home`), and restarting a system service for `elevd`
//! (`RestartService`).
//!
//! Each is accepted from one kernel-stamped identity alone (the service's
//! system uid, unlabelled, outside any session), never from a uid 0 caller or
//! a capability. A home change runs as `init` (root) in a BusyBox `sh` helper
//! whose arguments are positional parameters, never script text, after the
//! name and ids were validated; the reply is deferred until the helper exits
//! (`HomeJobs::finished`), so `Create` answers once the home exists.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{self, errno, services, Endpoint, Error, Message, Parcel};
use user::sys::{self, Cred as SysCred};

use super::service::{Phase, Service};

/// What one home operation does. The arguments are `$1` op, `$2` home, `$3`
/// `uid:gid`, `$4` skeleton, `$5` archive directory, `$6` archived name.
///
/// `create` makes sure the home is the account's (review of #659, H6): one
/// it already owns is kept; one owned by root is a home made before the
/// account had its own uid (`admin` was uid 0 before U1) and is handed over
/// whole (`chown -hR`, never following a link); one owned by any other uid
/// belongs to an earlier account of the same name and is set aside in the
/// archive as `<name>-<that uid>`, never reused, before a fresh home is made.
/// `archive` and `remove` of a missing home are not errors. A path that is
/// not a plain directory (a link, a file) is refused.
const SCRIPT: &str = "set -u; op=$1; home=$2; owner=$3; skel=$4; archive=$5; kept=$6
want=${owner%%:*}
case $op in
create)
  [ -L \"$home\" ] && exit 3
  if [ -d \"$home\" ]; then
    have=$(stat -c %u \"$home\") || exit 1
    [ \"$have\" = \"$want\" ] && exit 0
    if [ \"$have\" = 0 ]; then
      chown -hR \"$owner\" \"$home\" && chmod 700 \"$home\"
      exit
    fi
    aside=\"$archive/${home##*/}-$have\"
    [ -e \"$aside\" ] && exit 3
    mkdir -p -m 700 \"$archive\" && mv \"$home\" \"$aside\" || exit 1
  elif [ -e \"$home\" ]; then
    exit 3
  fi
  mkdir -m 700 \"$home\" || exit 1
  if [ -d \"$skel\" ]; then cp -a \"$skel/.\" \"$home/\" || exit 1; fi
  chown -hR \"$owner\" \"$home\" && chmod 700 \"$home\" ;;
archive)
  [ -d \"$home\" ] || exit 0
  [ -e \"$archive/$kept\" ] && exit 3
  mkdir -p -m 700 \"$archive\" && mv \"$home\" \"$archive/$kept\" ;;
remove)
  [ -d \"$home\" ] || exit 0
  rm -rf \"$home\" ;;
*) exit 2 ;;
esac";

/// One home change in flight: its helper and the request waiting for it.
struct Job {
    pid: u64,
    txn: u64,
    op: String,
    name: String,
}

/// The home changes in flight.
#[derive(Default)]
pub(super) struct HomeJobs {
    jobs: Vec<Job>,
}

/// Whether `caller` is the service with system uid `uid` (unlabelled, no
/// session).
fn is_service(caller: &SysCred, uid: u32) -> bool {
    caller.uid == uid && caller.label_id == 0 && caller.session == 0
}

impl HomeJobs {
    /// Start `Home` for `message`. `Ok(())`: the reply is deferred to
    /// [`HomeJobs::finished`]; `Err`: answer it now.
    pub(super) fn start(&mut self, message: &Message) -> messenger::Result<()> {
        if !is_service(&message.caller(), accountdb::ACCOUNTS_UID) {
            return Err(Error::Errno(-errno::EPERM));
        }
        let txn = message.txn.ok_or(Error::Errno(-errno::EINVAL))?;
        let args =
            services::init::wire::decode_home_args(&message.parcel.body).map_err(Error::Parcel)?;
        let human = accountdb::FIRST_UID..=accountdb::LAST_UID;
        if !matches!(args.op.as_str(), "create" | "archive" | "remove")
            || !accountdb::valid_account_name(&args.name)
            || !human.contains(&args.uid)
            || !human.contains(&args.gid)
        {
            return Err(Error::Errno(-errno::EINVAL));
        }
        let home = fhs::home_of(&args.name);
        let owner = format!("{}:{}", args.uid, args.gid);
        let kept = format!("{}-{}", args.name, args.uid);
        let argv = [
            "sh",
            "-c",
            SCRIPT,
            "sh",
            args.op.as_str(),
            home.as_str(),
            owner.as_str(),
            fhs::etc::SKEL,
            fhs::state::HOME_ARCHIVE,
            kept.as_str(),
        ];
        let pid = sys::spawnv(
            "sh",
            &argv,
            &[],
            sys::Personality::Linux,
            sys::SpawnCred::Inherit,
        )
        .map_err(Error::Errno)?;
        self.jobs.push(Job {
            pid,
            txn,
            op: args.op,
            name: args.name,
        });
        Ok(())
    }

    /// A child exited: when it is a home helper, answer its request and say
    /// so (`true`).
    pub(super) fn finished(&mut self, pid: u64, status: u64, server: &Endpoint) -> bool {
        let Some(index) = self.jobs.iter().position(|job| job.pid == pid) else {
            return false;
        };
        let job = self.jobs.swap_remove(index);
        let method = services::init::wire::METHOD_HOME;
        let reply = if status == 0 {
            sys::write_str(&format!("INIT:HOME:PASS op={} user={}\n", job.op, job.name));
            Parcel {
                header: services::header(services::init::INTERFACE, method),
                ..Parcel::default()
            }
        } else {
            sys::write_str(&format!(
                "INIT:HOME:FAIL op={} user={} status={status}\n",
                job.op, job.name
            ));
            services::init_error_reply(method, Error::Errno(-errno::EIO))
        };
        let _ = server.reply_or_drop(job.txn, &reply);
        true
    }
}

/// Why a `RestartService` from anyone but `elevd` is refused (`EPERM`): the
/// attack harness (`tools/accounts`) matches this text.
const RESTART_DENIED: &str = "only elevd may restart a service, once an administrator approved";

/// `RestartService` from `elevd`: kill the running task of the manifest row
/// `name`; the supervisor restarts it as after a crash. Returns that pid.
pub(super) fn restart_service(
    services: &mut [Service],
    message: &Message,
) -> messenger::Result<Parcel> {
    if !is_service(&message.caller(), accountdb::ELEVD_UID) {
        return Ok(services::refusal(
            services::init::INTERFACE,
            services::init::wire::METHOD_RESTARTSERVICE,
            errno::EPERM,
            RESTART_DENIED,
        ));
    }
    let args = services::init::wire::decode_restart_service_args(&message.parcel.body)
        .map_err(Error::Parcel)?;
    let row = services
        .iter()
        .find(|row| !row.launched && row.name == args.name)
        .ok_or(Error::Errno(-errno::ENOENT))?;
    if row.phase != Phase::Running || row.pid == 0 {
        return Err(Error::Errno(-errno::EAGAIN));
    }
    let pid = row.pid;
    sys::kill(pid, sys::SIG_KILL).map_err(Error::Errno)?;
    sys::write_str(&format!(
        "INIT:RESTART:PASS service={} pid={pid} by=elevd\n",
        args.name
    ));
    let body = services::init::wire::encode_restart_service_reply(
        &services::init::wire::RestartServiceReply { pid },
    )
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: services::header(
            services::init::INTERFACE,
            services::init::wire::METHOD_RESTARTSERVICE,
        ),
        body,
        ..Parcel::default()
    })
}
