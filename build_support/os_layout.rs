//! The directories of the OS volume (docs/filesystem-plan.md F2): a
//! declarative table of path, mode, uid and gid, so the layout is reviewed as
//! data rather than inferred from code.
//!
//! The ext2 root keeps today's flat file names (F3 renames them); this table
//! only adds the directories around them. `/data` and what is below it mirrors
//! `tools/mkdisk/layout.py` so `pkgd`, `confd` and `lazyrad` find the same
//! tree on the root volume that they found on the data disk.

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

/// The sticky, world-writable mode of `/data/tmp`.
pub const STICKY_WORLD_WRITABLE: u16 = 0o1777;

/// Where the build puts the manifest of the paths it placed.
pub const MANIFEST_PATH: &str = "/system/.image-manifest";

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
/// * the mount points `/boot`, `/home` and `/transient` (root, 0755);
/// * `/system` (empty until F3) and `/apps`, `/conf`, `/logs` (empty until F4,
///   which also adjusts the owners to the uid each service runs under);
/// * the transitional `/data`: `/data/home/<user>` for each account whose home
///   is `/home/<user>` (the account's uid and gid, 0755) and `/data/tmp`
///   (1777). root's `/root` and service accounts get none: nobody logs in
///   there.
pub fn dirs(accounts: &[Account]) -> Vec<DirSpec> {
    let mut out: Vec<DirSpec> = [
        "/boot",
        "/home",
        "/transient",
        "/system",
        "/apps",
        "/conf",
        "/logs",
        "/data",
        "/data/home",
    ]
    .iter()
    .map(|path| DirSpec::new(path, 0o755, 0, 0))
    .collect();
    for account in accounts {
        if account.home == format!("/home/{}", account.name) {
            out.push(DirSpec::new(
                &format!("/data/home/{}", account.name),
                0o755,
                account.uid,
                account.gid,
            ));
        }
    }
    out.push(DirSpec::new("/data/tmp", STICKY_WORLD_WRITABLE, 0, 0));
    out
}

/// The mode of a file the build places: executables (`*.ELF`, `BUSYBOX`) are
/// 0755, everything else 0644. Both are root-owned.
pub fn file_mode(path: &str) -> u16 {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "BUSYBOX" || name.ends_with(".ELF") {
        0o755
    } else {
        0o644
    }
}
