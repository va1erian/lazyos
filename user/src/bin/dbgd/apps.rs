//! App swapping (docs/dbgd-plan.md, v2): stage an uploaded `.lzp`, have
//! `pkgd` install it (`InstallDebug`, which checks the SHA-256 and `dbgd`'s
//! identity again) and `init` relaunch what runs of it (`RelaunchApp`).
//! `dbgd` only carries bytes: both services decide on their own whether
//! the box allows it.

use alloc::format;
use alloc::string::String;

use dbgwire::control::{self, Upload, PACKAGE};
use dbgwire::json::{Object, Value};
use dbgwire::rpc::code;
use user::messenger::services;
use user::sys;

use super::handlers::Failure;

/// Ticks `init` has to stop and relaunch an app's instances.
const INIT_TICKS: u64 = 30 * 100;

fn text<'a>(params: &'a Value, key: &str) -> &'a str {
    params.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `app.upload`: one chunk of the package, in order.
pub(crate) fn upload(params: &Value, current: &mut Option<Upload>) -> Result<String, Failure> {
    let mut params = params.clone();
    if let Value::Object(members) = &mut params {
        members.push((String::from("name"), Value::Str(String::from(PACKAGE))));
    }
    super::control::upload(&params, current)
}

/// `app.install`: install the staged package, then (unless `relaunch` is
/// false) restart the running instances of the app it holds.
pub(crate) fn install(params: &Value, current: &mut Option<Upload>) -> Result<String, Failure> {
    let sha256 = text(params, "sha256");
    control::parse_digest(sha256)
        .map_err(|why| (code::INVALID_PARAMS, String::from(why.text())))?;
    let size = Upload::finished(current, PACKAGE)
        .map_err(|why| (code::INVALID_PARAMS, String::from(why.text())))?
        .total;
    let relaunch = !matches!(params.get("relaunch"), Some(Value::Bool(false)));
    let path = control::staged_package_path();
    let installed = user::messenger::pkgd::Client::connect()
        .map_err(|e| (code::UNAVAILABLE, format!("pkgd: {}", e.message())))
        .and_then(|pkgd| {
            pkgd.install_debug(&path, sha256).map_err(|f| {
                (
                    code::DENIED,
                    format!("pkgd refused the package: {}", f.text),
                )
            })
        });
    *current = None;
    let _ = user::files::remove(&path);
    let app = installed?;
    let mut answer = Object::new()
        .str("system_name", &app.system_name)
        .str("name", &app.name)
        .str("version", &app.version)
        .str("install_dir", &app.install_dir)
        .uint("bytes", size);
    if relaunch {
        let (stopped, started) = relaunch_app(&app.system_name)?;
        answer = answer.uint("stopped", stopped).uint("started", started);
    }
    Ok(answer.finish())
}

fn relaunch_app(app: &str) -> Result<(u64, u64), Failure> {
    let init = services::resolve_service(services::INIT_NAME)
        .map_err(|e| (code::UNAVAILABLE, format!("init: {}", e.message())))?;
    services::init::relaunch_app(&init, app, Some(sys::clock() + INIT_TICKS)).map_err(|e| {
        (
            code::DENIED,
            format!("init refused to relaunch {app}: {}", e.message()),
        )
    })
}

/// `app.relaunch`.
pub(crate) fn relaunch(params: &Value) -> Result<String, Failure> {
    let app = text(params, "app");
    if !control::valid_app_id(app) {
        return Err((
            code::INVALID_PARAMS,
            String::from("an app id is a system_name or a built-in app's id"),
        ));
    }
    let (stopped, started) = relaunch_app(app)?;
    Ok(Object::new()
        .str("app", app)
        .uint("stopped", stopped)
        .uint("started", started)
        .finish())
}
