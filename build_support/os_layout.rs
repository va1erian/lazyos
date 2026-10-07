//! The directories of the OS volume (docs/filesystem-plan.md F2): a
//! declarative table of path, mode, uid and gid, so the layout is reviewed as
//! data rather than inferred from code.
//!
//! Every file the build places lives below a directory of this table (F3:
//! programs in `/system/bin`, data in `/system/etc` and `/system/share`, the
//! documentation in `/docs/os`); nothing but directories sits at the root.
//! Since F4 each service keeps its state at its place in the tree (`/conf`,
//! `/logs`, `/apps`, `/docs/apps`) and the build seeds nothing under `/data`:
//! an update removes the old `/data/home/<user>` and `/data/tmp` only when they
//! are empty, so user files there survive until F7 migrates them.
//!
//! An update applies each directory's mode and owner again, so an image built
//! before a change of this table converges to it.

/// One directory: where it lives and who may do what in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirSpec {
    pub path: String,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
}

impl DirSpec {
    fn new(path: &str, mode: u16, uid: u32, gid: u32) -> DirSpec {
        DirSpec {
            path: path.to_string(),
            mode,
            uid,
            gid,
        }
    }
}

/// A directory only its owner may enter.
pub const PRIVATE: u16 = 0o700;

/// Where the build puts the manifest of the paths it placed.
pub const MANIFEST_PATH: &str = fhs::system::IMAGE_MANIFEST;

/// One account of the embedded `PASSWD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
}

/// The accounts of a passwd-style file (`name:uid:gid:secret:home:shell`).
/// A malformed line is skipped: the file is the build's own input, but the
/// layout must never panic on it.
pub fn parse_passwd(text: &str) -> Vec<Account> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?.trim();
            let uid = fields.next()?.trim().parse().ok()?;
            let gid = fields.next()?.trim().parse().ok()?;
            let _secret = fields.next()?;
            let home = fields.next()?.trim();
            // A name becomes a path component, so it must be one.
            let plain = !name.is_empty()
                && name != "."
                && name != ".."
                && !name.contains(['/', '\n', '\0']);
            plain.then(|| Account {
                name: name.to_string(),
                uid,
                gid,
                home: home.to_string(),
            })
        })
        .collect()
}

/// Every directory the image creates, parents before children.
///
/// * the mount points `/boot`, `/home`, `/transient` and `/mnt` (root, 0755);
/// * `/system` with `bin`, `etc`, `share` and `packages` (root, 0755; F5 fills
///   `packages`);
/// * each service's state (the table below);
/// * the transitional `/data` itself (root, 0755; nothing writes below it
///   since F4, it stays the opt-in data disk's mount point and the `confd`
///   seed until F7 removes it);
/// * a home for each account of the embedded passwd whose home is
///   `/home/<name>`: 0700, owned by the account's uid and gid. These are the
///   homes without a home volume; a mounted `/home` volume hides them.
///   Accounts whose home lies elsewhere (a service account's, say) get none.
pub fn dirs(accounts: &[Account]) -> Vec<DirSpec> {
    let mut out: Vec<DirSpec> = [
        fhs::mount::BOOT,
        fhs::mount::HOME,
        fhs::mount::TRANSIENT,
        fhs::mount::MNT,
        fhs::SYSTEM,
        fhs::SYSTEM_BIN,
        fhs::SYSTEM_ETC,
        fhs::SYSTEM_SHARE,
        fhs::SYSTEM_PACKAGES,
    ]
    .iter()
    .map(|path| DirSpec::new(path, 0o755, 0, 0))
    .collect();
    // Service state. Every service involved runs as uid 0 today
    // (`user/src/bin/init/state.rs`); when #446/#447 give them their own
    // uids, these owners change with them.
    out.extend([
        // Only `confd` reads the raw store; everyone else goes through it.
        DirSpec::new(fhs::state::CONF_ROOT, PRIVATE, 0, 0),
        // Per-service state dirs, each created by its owner.
        DirSpec::new(fhs::state::CONF_SVC, PRIVATE, 0, 0),
        // `logd`'s journals and `pkgd`'s audit log carry every user's
        // activity: only root (and group root) may enter.
        DirSpec::new(fhs::state::LOGS_ROOT, 0o750, 0, 0),
        // Written only by `pkgd`.
        DirSpec::new(fhs::state::APPS_ROOT, 0o755, 0, 0),
        DirSpec::new(fhs::docs::DOCS_ROOT, 0o755, 0, 0),
        DirSpec::new(fhs::docs::DOCS_APPS, 0o755, 0, 0),
        DirSpec::new(fhs::mount::DATA, 0o755, 0, 0),
    ]);
    for account in accounts {
        if account.home == fhs::home_of(&account.name) {
            out.push(DirSpec::new(
                &account.home,
                PRIVATE,
                account.uid,
                account.gid,
            ));
        }
    }
    out
}

/// The mode of a file the build places, from where it goes: 0755 for anything
/// under `/system/bin` (the programs), 0600 for the shadow file, 0644 for
/// everything else. All are root-owned. A name never decides it, so a data
/// file cannot become executable by being called `*.ELF`.
pub fn file_mode(path: &str) -> u16 {
    // The password verifiers: root only (issue #447).
    if path == fhs::etc::SHADOW {
        return 0o600;
    }
    let in_bin = path
        .trim_start_matches('/')
        .strip_prefix(fhs::SYSTEM_BIN.trim_start_matches('/'))
        .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1);
    if in_bin {
        0o755
    } else {
        0o644
    }
}
