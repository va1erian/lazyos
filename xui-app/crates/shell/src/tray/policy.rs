//! Who may call `os.lazy.shell.tray.v1`, and which app a caller is
//! (docs/tray-plan.md section 8).
//!
//! The identity always comes from the kernel: the session and uid it stamped
//! on the request, and the sender's task, which `init`'s table maps to the
//! app it launched there. Nothing in the request names the app, so an app
//! cannot set another app's item.

/// Why a caller was refused, for `SHELL:TRAY:DENY ... why=<...>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deny {
    /// The shell's own session or uid is unknown: fail closed.
    Unknown,
    /// Another login session (or none: a system service).
    Session,
    /// Neither root nor the shell's user.
    Uid,
    /// Not a task `init` launched as an app (`ENOENT`).
    NotAnApp,
}

impl Deny {
    pub fn as_str(self) -> &'static str {
        match self {
            Deny::Unknown => "unknown",
            Deny::Session => "session",
            Deny::Uid => "uid",
            Deny::NotAnApp => "not-an-app",
        }
    }
}

/// The caller as the kernel stamped it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub session: u64,
}

/// The shell's own identity; `None` parts when they could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shell {
    pub uid: Option<u32>,
    pub session: Option<u64>,
}

/// Whether `caller` may use the tray of `shell`: the shell's own session,
/// and uid 0 or the shell's uid. A desktop with no login (the default image
/// starts its shell as root in session 0) has session 0, and so do the apps
/// it launches; system services are kept out by [`app_of`], which only
/// accepts a task that runs an app of the registry.
pub fn check(caller: Caller, shell: Shell) -> Result<(), Deny> {
    let (Some(uid), Some(session)) = (shell.uid, shell.session) else {
        return Err(Deny::Unknown);
    };
    if caller.session != session {
        return Err(Deny::Session);
    }
    if caller.uid != 0 && caller.uid != uid {
        return Err(Deny::Uid);
    }
    Ok(())
}

/// The app `init` runs as task `pid`, from its table's `(name, pid)` rows:
/// the key of the caller's item. Only a row naming an app of the registry
/// (`is_app`) counts, so a supervised service is no app; a row with pid 0
/// (not running) never matches.
pub fn app_of<'a>(
    rows: impl IntoIterator<Item = (&'a str, u64)>,
    pid: u64,
    is_app: impl Fn(&str) -> bool,
) -> Result<&'a str, Deny> {
    if pid == 0 {
        return Err(Deny::NotAnApp);
    }
    rows.into_iter()
        .find(|(_, row_pid)| *row_pid == pid)
        .map(|(name, _)| name)
        .filter(|name| is_app(name))
        .ok_or(Deny::NotAnApp)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHELL: Shell = Shell {
        uid: Some(1000),
        session: Some(3),
    };

    #[test]
    fn only_the_shells_session_and_user_or_root() {
        let caller = |uid, session| Caller { uid, session };
        assert_eq!(check(caller(1000, 3), SHELL), Ok(()));
        assert_eq!(check(caller(0, 3), SHELL), Ok(()));
        assert_eq!(check(caller(1001, 3), SHELL), Err(Deny::Uid));
        assert_eq!(check(caller(1000, 4), SHELL), Err(Deny::Session));
        // Root outside the shell's session (a system service) is refused.
        assert_eq!(check(caller(0, 0), SHELL), Err(Deny::Session));
        let unknown = Shell { uid: None, ..SHELL };
        assert_eq!(check(caller(0, 3), unknown), Err(Deny::Unknown));
        // A desktop with no login runs in session 0, and so do its apps.
        let no_login = Shell {
            uid: Some(0),
            session: Some(0),
        };
        assert_eq!(check(caller(0, 0), no_login), Ok(()));
        assert_eq!(check(caller(0, 3), no_login), Err(Deny::Session));
    }

    #[test]
    fn the_app_comes_from_inits_table() {
        let rows = [("logd", 9), ("os.lazy.traydemo", 41), ("idle", 0)];
        let apps = |name: &str| name.starts_with("os.lazy.") || name == "idle";
        assert_eq!(app_of(rows, 41, apps), Ok("os.lazy.traydemo"));
        assert_eq!(app_of(rows, 40, apps), Err(Deny::NotAnApp));
        assert_eq!(app_of(rows, 0, apps), Err(Deny::NotAnApp));
        // A supervised service is not an app.
        assert_eq!(app_of(rows, 9, apps), Err(Deny::NotAnApp));
    }
}
