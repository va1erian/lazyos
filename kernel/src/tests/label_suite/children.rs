//! Child labels (issue #832, `kernel/src/ipc/children.rs`): `app:<id>/<name>`
//! labels for an installed app's helper programs.
//!
//! The app plays a browser: `app:org.test.browser` holds the display and the
//! network, and declares a page process (`/system/bin/webprocess`, no
//! interface at all) and a network process (`/system/bin/networkprocess`,
//! just the network). The loader (`OP_ACL_LOAD`) is the real one; the entering
//! `execve` is exercised through the function `sys_execve` calls
//! ([`children::enter_on_exec`]), since a successful `execve` would replace
//! the test task's image.

use super::*;
use crate::ipc::acl::{reason, Rule};
use crate::ipc::credentials::{LabelStamp, TransitionError};
use crate::ipc::labels::{Error as LabelError, Kind};
use crate::ipc::topics::{self, fnv1a32, fnv1a64};
use crate::ipc::{authorize, children, devspawn};

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EACCES: i64 = 13;
const EINVAL: i64 = 22;

const APP: &str = "app:org.test.browser";
const OTHER: &str = "app:org.test.other";
const WEB: &str = "app:org.test.browser/webprocess";
const NET: &str = "app:org.test.browser/networkprocess";
const OTHER_WEB: &str = "app:org.test.other/webprocess";
const WEB_PATH: &str = "/system/bin/webprocess";
const NET_PATH: &str = "/system/bin/networkprocess";

const DISPLAY: &str = "os.lazy.display.v1";
const SOCKETS: &str = "os.lazy.net.socket.v1";
const CLIPBOARD: &str = "os.lazy.clipboard.v1";
/// The service name the socket interface is also served under.
const STACK: &str = "os.lazy.net.stack";

const UID: u32 = 1000;
const SESSION: u64 = 7;
/// A capability the app's tasks hold, to see the child keeps it as it is.
const CAPS: u32 = credentials::CAP_KILL;

fn any(interface: &str) -> policy::wire::LabelRule {
    policy::wire::LabelRule {
        interface_id: fnv1a64(interface),
        method: u32::MAX,
        allow: true,
    }
}

fn resolve(name: &str) -> policy::wire::LabelRule {
    policy::wire::LabelRule {
        interface_id: policy::RESOLVE_SCOPE,
        method: fnv1a32(name),
        allow: true,
    }
}

fn kernel_rule(interface: u64, method: u32) -> Rule {
    Rule {
        actor: 0,
        interface_id: interface,
        method,
        allow: true,
    }
}

/// The bring-up state of the label suite, and no program bound.
fn fresh_children() -> Result<(), String> {
    fresh()?;
    children::reset_for_tests();
    Ok(())
}

/// Load the app's own label (display, sockets and the name the socket
/// interface is served under) through the real loader.
fn load_app() -> Result<u32, String> {
    let rules = [any(DISPLAY), any(SOCKETS), resolve(STACK)];
    check!(
        load_current(APP, &rules)? == 0,
        "the app's rules were refused"
    );
    labels::lookup(APP).ok_or_else(|| String::from("the app label was not interned"))
}

/// Load both children of the browser, after [`load_app`].
fn load_children() -> Result<(u32, u32), String> {
    check!(
        load_with_program(WEB, &[], Some(WEB_PATH))? == 0,
        "the page process was refused"
    );
    check!(
        load_with_program(NET, &[any(SOCKETS), resolve(STACK)], Some(NET_PATH))? == 0,
        "the network process was refused"
    );
    let web = labels::lookup(WEB).ok_or("the page label was not interned")?;
    let net = labels::lookup(NET).ok_or("the network label was not interned")?;
    Ok((web, net))
}

/// A task of `label_id`, with the app's identity.
fn task_of(label_id: u32) -> Result<usize, String> {
    let slot = task::spawn_child("child", &service_suite::minimal_elf()).map_err(to_string)?;
    credentials::set(slot, Cred::new(UID, UID, CAPS, label_id, SESSION));
    Ok(slot)
}

fn reap(slot: usize) -> Result<(), String> {
    task::harness::finish(slot, 0);
    check!(
        task::reap_child_slot(slot).is_some(),
        "could not reap {slot}"
    );
    Ok(())
}

fn allowed(slot: usize, interface: &str) -> bool {
    !authorize(slot, fnv1a64(interface), 3, 0).denied()
}

mod behave;
mod load;
mod start;

pub(super) use behave::*;
pub(super) use load::*;
pub(super) use start::*;
