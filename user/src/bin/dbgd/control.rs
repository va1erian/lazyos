//! The control tier (docs/dbgd-plan.md, v2): restart a service, and hot
//! reload one from a binary the client uploads.
//!
//! `dbgd` holds no authority of its own here: it stages the upload in its
//! own ramfs directory and asks `init`, which checks `dbgd`'s identity,
//! the box's `diag.dbg.control=1`, the service name and the binary's
//! SHA-256 again, and owns the trial and the rollback. The rules shared by
//! both sides are `dbgwire::control`.

use alloc::format;
use alloc::string::String;

use dbgwire::control::{self, Upload, Write};
use dbgwire::json::{self, Object, Value};
use dbgwire::rpc::code;
use user::messenger::services;
use user::sys;

use super::handlers::Failure;

/// Ticks `init` has to copy, check and restart (a few MiB on the ramfs).
const INIT_TICKS: u64 = 30 * 100;

fn text<'a>(params: &'a Value, key: &str) -> &'a str {
    params.get(key).and_then(Value::as_str).unwrap_or("")
}

fn number(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn refused(why: control::Refusal) -> Failure {
    let code = match why {
        control::Refusal::Never => code::DENIED,
        _ => code::INVALID_PARAMS,
    };
    (code, String::from(why.text()))
}

/// The staging directory, made at start when control is on. `init`
/// (root) reads it; nobody else needs to.
pub(crate) fn prepare() {
    let _ = user::files::mkdir(fhs::state::DBGD_STAGE);
    let _ = user::files::chmod(fhs::state::DBGD_STAGE, 0o700);
}

fn init() -> Result<user::messenger::Endpoint, Failure> {
    services::resolve_service(services::INIT_NAME)
        .map_err(|e| (code::UNAVAILABLE, format!("init: {}", e.message())))
}

fn init_failed(what: &str, name: &str, error: user::messenger::Error) -> Failure {
    (
        code::DENIED,
        format!(
            "init refused to {what} {name}: {} (see INIT:RELOAD lines in the programs log)",
            error.message()
        ),
    )
}

fn deadline() -> Option<u64> {
    Some(sys::clock() + INIT_TICKS)
}

/// `service.restart`.
pub(crate) fn restart(params: &Value) -> Result<String, Failure> {
    let name = text(params, "name");
    control::reloadable(name).map_err(refused)?;
    let pid = services::init::reload_service(&init()?, name, "", "", 0, deadline())
        .map_err(|e| init_failed("restart", name, e))?;
    Ok(Object::new()
        .str("name", name)
        .uint("stopped_pid", pid)
        .finish())
}

/// `service.upload`: one chunk, in order.
pub(crate) fn upload(params: &Value, current: &mut Option<Upload>) -> Result<String, Failure> {
    let name = text(params, "name");
    let bytes = control::base64_decode(text(params, "data")).map_err(refused)?;
    let offset = number(params, "offset", 0);
    let total = number(params, "total", 0);
    let write =
        Upload::accept(current, name, offset, total, bytes.len() as u64).map_err(refused)?;
    let path = control::staged_path(name);
    let written = match write {
        Write::Create => user::files::write_file(&path, &bytes),
        Write::Append => user::files::append_file(&path, &bytes),
    };
    if let Err(errno) = written {
        *current = None;
        let _ = user::files::remove(&path);
        return Err((
            code::UNAVAILABLE,
            format!("{path}: {}", user::files::describe(errno)),
        ));
    }
    let received = current.as_ref().map_or(0, |up| up.received);
    Ok(Object::new()
        .str("name", name)
        .uint("received", received)
        .uint("total", total)
        .bool("complete", received == total)
        .finish())
}

/// `service.reload`: hand the finished upload to `init`.
pub(crate) fn reload(params: &Value, current: &mut Option<Upload>) -> Result<String, Failure> {
    let name = text(params, "name");
    control::reloadable(name).map_err(refused)?;
    let sha256 = text(params, "sha256");
    control::parse_digest(sha256).map_err(refused)?;
    let size = Upload::finished(current, name).map_err(refused)?.total;
    let trial_ms = number(params, "trial_ms", control::TRIAL_DEFAULT_MS);
    let path = control::staged_path(name);
    let result =
        services::init::reload_service(&init()?, name, &path, sha256, trial_ms as u32, deadline());
    // `init` has its own copy (or refused): the staged file is done either way.
    *current = None;
    let _ = user::files::remove(&path);
    let pid = result.map_err(|e| init_failed("reload", name, e))?;
    Ok(Object::new()
        .str("name", name)
        .uint("bytes", size)
        .str("sha256", sha256)
        .uint("trial_ms", trial_ms)
        .uint("stopped_pid", pid)
        .finish())
}

/// `service.revert`.
pub(crate) fn revert(params: &Value) -> Result<String, Failure> {
    let name = text(params, "name");
    control::reloadable(name).map_err(refused)?;
    let pid = services::init::revert_service(&init()?, name, deadline())
        .map_err(|e| init_failed("revert", name, e))?;
    Ok(Object::new()
        .str("name", name)
        .uint("stopped_pid", pid)
        .finish())
}

/// `service.reloads`.
pub(crate) fn reloads() -> Result<String, Failure> {
    let rows = services::init::reloads(&init()?, deadline())
        .map_err(|e| (code::UNAVAILABLE, format!("init: {}", e.message())))?;
    let rows = rows.iter().map(|r| {
        Object::new()
            .str("name", &r.name)
            .str("state", &r.state)
            .str("sha256", &r.sha256)
            .uint("pid", r.pid)
            .str("detail", &r.detail)
            .finish()
    });
    Ok(Object::new().raw("reloads", &json::array(rows)).finish())
}
