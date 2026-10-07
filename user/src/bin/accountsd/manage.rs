//! Account management: `Create`, `Delete`, `SetPassword` and `SetAdmin`.
//!
//! Each request is authorized first (`accountdb::policy`), then applied to a
//! copy of the database, which replaces the live one only once it is on disk:
//! a refused or failed change leaves the database as it was. Passwords go to
//! `keyd`, which returns the verifier the database stores; homes are made,
//! archived or removed by `init` (`init.Home`).

use alloc::format;
use alloc::string::String;

use accountdb::ops::OpError;
use accountdb::policy::{authorize, Allowed, Change, Who};
use accountdb::{Db, Verifier};
use user::messenger::accounts::{wire, UserRecord};
use user::messenger::{errno, keyd, services, Message};
use user::sys;

use super::{authn, record, store, Refusal, State};

/// Longest password accepted (what `keyd` takes).
const SECRET_MAX: usize = 64;
/// How long `init` may take to make or remove a home (PIT ticks): a copy of
/// the skeleton, or a recursive removal of a large home under emulation.
const HOME_TICKS: u64 = 6000;

/// `Create`.
pub(crate) fn create(
    state: &mut State,
    message: &Message,
    args: &wire::CreateArgs,
) -> Result<UserRecord, Refusal> {
    let who = authorized(state, message, Change::Create { admin: args.admin })?;
    check_secret(&args.secret)?;
    let mut db = database(state)?.clone();
    let mut user = db
        .create(&args.name, args.admin, None)
        .map_err(op_refusal)?;
    let verifier = provision(&args.name, &args.secret)?;
    db.set_secret(&args.name, verifier.clone())
        .map_err(op_refusal)?;
    user.secret = Some(verifier);
    if let Err(text) = store::persist(&db) {
        forget(&args.name);
        return Err(Refusal(errno::EIO, text));
    }
    state.db = Ok(db);
    sys::write_str(&format!(
        "ACCOUNTS:CREATE:PASS user={} uid={} admin={} by={}\n",
        user.name,
        user.uid,
        u8::from(args.admin),
        by(who)
    ));
    home("create", &user.name, user.uid, user.gid)?;
    Ok(record(&user))
}

/// `Delete`: the account, its verifier, and its home as asked.
pub(crate) fn delete(
    state: &mut State,
    message: &Message,
    args: &wire::DeleteArgs,
) -> Result<(), Refusal> {
    let who = authorized(state, message, Change::Delete)?;
    let op = match args.home.as_str() {
        "keep" => None,
        "archive" => Some("archive"),
        "remove" => Some("remove"),
        _ => {
            return Err(Refusal::new(
                errno::EINVAL,
                "home must be keep, archive or remove",
            ))
        }
    };
    let mut db = database(state)?.clone();
    let user = db.delete(&args.name).map_err(op_refusal)?;
    store::persist(&db).map_err(|text| Refusal(errno::EIO, text))?;
    state.db = Ok(db);
    forget(&user.name);
    sys::write_str(&format!(
        "ACCOUNTS:DELETE:PASS user={} uid={} home={} by={}\n",
        user.name,
        user.uid,
        args.home,
        by(who)
    ));
    match op {
        Some(op) => home(op, &user.name, user.uid, user.gid),
        None => Ok(()),
    }
}

/// `SetPassword`: a user's own with the old one, anyone's from `elevd`.
pub(crate) fn set_password(
    state: &mut State,
    message: &Message,
    args: &wire::SetPasswordArgs,
) -> Result<(), Refusal> {
    let caller = message.caller();
    let change = Change::SetPassword {
        target: &args.name,
        with_old: args.old.is_some(),
    };
    let who = authn::who(&caller);
    let allowed = authorize(database(state)?, who, change).map_err(denied)?;
    if allowed == Allowed::WithOldPassword {
        let old = args.old.as_deref().unwrap_or("");
        if !authn::check(state, &caller, &args.name, old)? {
            return Err(Refusal::new(errno::EACCES, "the current password is wrong"));
        }
    }
    check_secret(&args.secret)?;
    let mut db = database(state)?.clone();
    if db.user(&args.name).is_none() {
        return Err(op_refusal(OpError::NotFound));
    }
    let verifier = provision(&args.name, &args.secret)?;
    db.set_secret(&args.name, verifier).map_err(op_refusal)?;
    store::persist(&db).map_err(|text| Refusal(errno::EIO, text))?;
    state.db = Ok(db);
    sys::write_str(&format!(
        "ACCOUNTS:PASSWORD:PASS user={} by={}\n",
        args.name,
        by(who)
    ));
    Ok(())
}

/// `SetAdmin`.
pub(crate) fn set_admin(
    state: &mut State,
    message: &Message,
    args: &wire::SetAdminArgs,
) -> Result<(), Refusal> {
    let who = authorized(state, message, Change::SetAdmin)?;
    let mut db = database(state)?.clone();
    db.set_admin(&args.name, args.admin).map_err(op_refusal)?;
    store::persist(&db).map_err(|text| Refusal(errno::EIO, text))?;
    state.db = Ok(db);
    sys::write_str(&format!(
        "ACCOUNTS:ADMIN:PASS user={} admin={} by={}\n",
        args.name,
        u8::from(args.admin),
        by(who)
    ));
    Ok(())
}

/// Authorize `change` for the sender of `message`, logging a refusal.
fn authorized(state: &State, message: &Message, change: Change<'_>) -> Result<Who, Refusal> {
    let caller = message.caller();
    let who = authn::who(&caller);
    match authorize(database(state)?, who, change) {
        Ok(_) => Ok(who),
        Err(refusal) => {
            sys::write_str(&format!(
                "ACCOUNTS:DENIED change={change:?} caller_uid={} label={} session={}\n",
                caller.uid, caller.label_id, caller.session
            ));
            Err(denied(refusal))
        }
    }
}

fn denied(refusal: accountdb::policy::Denied) -> Refusal {
    Refusal::new(errno::EPERM, refusal.0)
}

fn op_refusal(error: OpError) -> Refusal {
    Refusal::new(error.errno(), error.message())
}

fn database(state: &State) -> Result<&Db, Refusal> {
    state
        .db
        .as_ref()
        .map_err(|_| Refusal::new(errno::EIO, "no account database"))
}

/// A password is not empty, fits `keyd`, and holds no control character.
fn check_secret(secret: &str) -> Result<(), Refusal> {
    if secret.is_empty() || secret.len() > SECRET_MAX || secret.chars().any(char::is_control) {
        return Err(Refusal::new(
            errno::EINVAL,
            "a password is 1 to 64 characters, without control characters",
        ));
    }
    Ok(())
}

/// Have `keyd` derive `name`'s verifier from `secret` (and start answering
/// for it); the verifier the database keeps.
fn provision(name: &str, secret: &str) -> Result<Verifier, Refusal> {
    let unavailable = |_| Refusal::new(errno::EIO, "the key service could not store the password");
    let client = keyd::Client::connect().map_err(unavailable)?;
    let text = client.provision(name, secret).map_err(unavailable)?;
    Verifier::parse(&text).ok_or_else(|| Refusal::new(errno::EIO, "keyd returned no verifier"))
}

/// Have `keyd` drop `name`'s verifier (best effort: an unknown name is fine).
fn forget(name: &str) {
    if let Ok(client) = keyd::Client::connect() {
        let _ = client.forget(name);
    }
}

/// Ask `init` to apply `op` to the home of `name`.
fn home(op: &str, name: &str, uid: u32, gid: u32) -> Result<(), Refusal> {
    let result = services::resolve_service(services::INIT_NAME).and_then(|init| {
        services::init::home(&init, op, name, uid, gid, Some(sys::clock() + HOME_TICKS))
    });
    match result {
        Ok(()) => {
            sys::write_str(&format!("ACCOUNTS:HOME:PASS op={op} user={name}\n"));
            Ok(())
        }
        Err(error) => {
            sys::write_str(&format!(
                "ACCOUNTS:HOME:FAIL op={op} user={name} error={}\n",
                error.message()
            ));
            Err(Refusal(
                errno::EIO,
                String::from("the account changed, but its home could not be updated"),
            ))
        }
    }
}

/// Who made a change, for the serial record.
fn by(who: Who) -> String {
    match who {
        Who::Elevd => String::from("elevd"),
        Who::Greeter => String::from("setup"),
        Who::User { uid } => format!("uid={uid}"),
    }
}
