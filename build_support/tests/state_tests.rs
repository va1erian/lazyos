//! The state the running system owns (`os_state.rs`, review of #659): the
//! account database leaves `/conf`, confd's store is private, and the
//! accounts' homes are seeds an update never re-creates or re-chowns.

use accountdb::ACCOUNTS_UID;
use ext2fs::memio::MemIo;
use ext2fs::{AttrChange, Ext2, FileKind, Geometry};

use crate::accounts_seed::{database, homes, place, Seed};
use crate::os_image::{write_volume, OsFile, OsFiles};
use crate::os_layout::{dirs, DirSpec};

fn volume() -> Ext2 {
    let io = MemIo::new(8 << 20);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: (8 << 20) / 4096,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&io, geometry, "lazyos", [9; 16], 1).unwrap();
    Ext2::open(Box::new(io), || 1).unwrap()
}

/// The default accounts' layout and files, as `build.rs` places them.
fn build() -> (Vec<DirSpec>, Vec<OsFile>) {
    let db = database(Seed::Accounts);
    let mut files = OsFiles::default();
    place(&mut files, &db);
    (dirs(&homes(&db)), files.files())
}

fn mode_owner(volume: &Ext2, path: &str) -> (u16, u32) {
    let meta = volume.lookup(path).unwrap();
    (meta.mode & 0o7777, meta.uid)
}

/// Every path below `dir`, depth first.
fn walk(volume: &Ext2, dir: &str, out: &mut Vec<String>) {
    for entry in volume.readdir(dir).unwrap() {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let path = format!("{dir}/{}", entry.name);
        out.push(path.clone());
        if entry.kind == FileKind::Dir {
            walk(volume, &path, out);
        }
    }
}

/// No user but root can read, write or cross anything of confd's.
fn assert_conf_private(volume: &Ext2) {
    assert_eq!(mode_owner(volume, fhs::state::CONF_ROOT), (0o700, 0));
    let mut all = Vec::new();
    walk(volume, fhs::state::CONF_ROOT, &mut all);
    for path in all {
        let (mode, _) = mode_owner(volume, &path);
        assert_eq!(mode & 0o077, 0, "{path} is open to others ({mode:o})");
    }
}

/// A volume as a build of the first U1 commits left it: the account database
/// in `/conf/accounts`, `/conf` 0711 and confd's store 0644.
fn u1_era_volume(db_text: &[u8]) -> Ext2 {
    let volume = volume();
    volume.mkdir_p(fhs::state::CONF_ROOT, 0o711, 0, 0).unwrap();
    let conf = fhs::state::CONF_ROOT;
    for name in ["store", "store.corrupt", ".seeded-from-data"] {
        let path = format!("{conf}/{name}");
        volume
            .write_file(&path, b"sys/x=1", 0o644, 0, 0, 1)
            .unwrap();
    }
    let legacy = fhs::state::LEGACY_ACCOUNTS_DIR;
    volume
        .mkdir_p(legacy, 0o700, ACCOUNTS_UID, ACCOUNTS_UID)
        .unwrap();
    let db = fhs::state::LEGACY_ACCOUNTS_DB;
    volume
        .write_file(db, db_text, 0o600, ACCOUNTS_UID, ACCOUNTS_UID, 1)
        .unwrap();
    let leftover = format!("{legacy}/db.new");
    volume
        .write_file(&leftover, b"half", 0o600, ACCOUNTS_UID, ACCOUNTS_UID, 1)
        .unwrap();
    volume
}

#[test]
fn a_fresh_volume_keeps_conf_private_and_the_database_outside_it() {
    let volume = volume();
    let (layout, files) = build();
    write_volume(&volume, None, &layout, &files, 1).unwrap();
    assert_conf_private(&volume);
    assert_eq!(
        mode_owner(&volume, fhs::state::ACCOUNTS_DIR),
        (0o700, ACCOUNTS_UID)
    );
    assert_eq!(
        mode_owner(&volume, fhs::state::ACCOUNTS_DB),
        (0o600, ACCOUNTS_UID)
    );
    // The seed homes, with the seed's owners.
    assert_eq!(mode_owner(&volume, "/home/admin"), (0o700, 1001));
    assert_eq!(mode_owner(&volume, "/home/user"), (0o700, 1000));
}

#[test]
fn an_update_moves_the_database_out_of_conf_and_makes_the_store_private() {
    let kept = b"next:1004\ngroup:admin:10\n".to_vec();
    let volume = u1_era_volume(&kept);
    let (layout, files) = build();
    write_volume(&volume, None, &layout, &files, 2).unwrap();
    assert_conf_private(&volume);
    // The running system's database moved, not replaced by the seed.
    assert_eq!(volume.read_file(fhs::state::ACCOUNTS_DB).unwrap(), kept);
    assert_eq!(
        mode_owner(&volume, fhs::state::ACCOUNTS_DB),
        (0o600, ACCOUNTS_UID)
    );
    assert!(volume.lookup(fhs::state::LEGACY_ACCOUNTS_DIR).is_err());
    // The store keeps its bytes, only its mode changed.
    let store = format!("{}/store", fhs::state::CONF_ROOT);
    assert_eq!(volume.read_file(&store).unwrap(), b"sys/x=1");
    // A database already moved wins over a stale one left behind.
    let stale = fhs::state::LEGACY_ACCOUNTS_DB;
    volume
        .mkdir_p(fhs::state::LEGACY_ACCOUNTS_DIR, 0o700, 0, 0)
        .unwrap();
    volume.write_file(stale, b"stale", 0o600, 0, 0, 3).unwrap();
    write_volume(&volume, None, &layout, &files, 4).unwrap();
    assert_eq!(volume.read_file(fhs::state::ACCOUNTS_DB).unwrap(), kept);
    assert!(volume.lookup(fhs::state::LEGACY_ACCOUNTS_DIR).is_err());
}

#[test]
fn an_update_never_recreates_or_rechowns_a_home() {
    let volume = volume();
    let (layout, files) = build();
    let manifest = write_volume(&volume, None, &layout, &files, 1).unwrap();
    // The running system deleted `user` (home removed) and an admin gave
    // `/home/admin` to another uid.
    volume.remove_tree("/home/user").unwrap();
    let change = AttrChange {
        uid: Some(1005),
        gid: Some(1005),
        mode: Some(0o750),
        ..AttrChange::default()
    };
    volume.setattr("/home/admin", &change).unwrap();
    write_volume(&volume, Some(&manifest), &layout, &files, 2).unwrap();
    assert!(
        volume.lookup("/home/user").is_err(),
        "a deleted home came back"
    );
    assert_eq!(mode_owner(&volume, "/home/admin"), (0o750, 1005));
}

#[test]
fn a_volume_without_a_database_gets_missing_homes_but_keeps_existing_ones() {
    // A pre-U1 volume: no database, `/home/admin` owned by root (admin was
    // uid 0). The update seeds the database and the missing home; the
    // existing one is left for `accountsd` to hand over at boot (it is
    // owned by root, which no account is).
    let volume = volume();
    volume.mkdir_p("/home/admin", 0o700, 0, 0).unwrap();
    let (layout, files) = build();
    write_volume(&volume, None, &layout, &files, 1).unwrap();
    assert!(volume.lookup(fhs::state::ACCOUNTS_DB).is_ok());
    assert_eq!(mode_owner(&volume, "/home/admin"), (0o700, 0));
    assert_eq!(mode_owner(&volume, "/home/user"), (0o700, 1000));
}
