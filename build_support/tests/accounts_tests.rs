//! The account database the image seeds (docs/accounts-plan.md U1): the
//! default accounts with Argon2id verifiers of their development passwords,
//! `admin` an ordinary uid in the `admin` group, no plaintext anywhere, and a
//! seed an update never replaces.

use accountdb::{ACCOUNTS_UID, ADMIN_GROUP};
use ext2fs::memio::MemIo;
use ext2fs::{Ext2, Geometry};
use lazyos_crypto::kdf;

use crate::accounts_seed::{database, homes, place, Seed, PASSWD, PASSWORDS};
use crate::os_image::{write_volume, OsFiles, Placement};
use crate::os_layout::{dirs, file_mode};

#[test]
fn every_account_gets_a_verifier_of_its_password() {
    let db = database(Seed::Accounts);
    let names: Vec<&str> = db.users.iter().map(|user| user.name.as_str()).collect();
    assert_eq!(names, ["admin", "user"]);
    let text = db.to_text();
    for line in PASSWORDS
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        let (name, password) = line.split_once(':').unwrap();
        let secret = db.user(name).unwrap().secret.clone().unwrap();
        let params = kdf::Params::INTERACTIVE;
        assert_eq!(
            (secret.cost.m_kib, secret.cost.t, secret.cost.p),
            (params.m_cost_kib, params.t_cost, params.p_cost)
        );
        let mut again = [0u8; 32];
        kdf::argon2id(password.as_bytes(), &secret.salt, params, &mut again).unwrap();
        assert_eq!(again, secret.hash, "{name}");
        // No file carries the password.
        assert!(!text.contains(&format!(":{password}")), "{name}");
        assert!(!db.passwd_view().contains(password), "{name}");
        assert!(
            !String::from_utf8_lossy(PASSWD).contains(password),
            "{name}"
        );
    }
}

#[test]
fn admin_is_an_ordinary_uid_in_the_admin_group() {
    let db = database(Seed::Accounts);
    let admin = db.user("admin").unwrap();
    assert_ne!(admin.uid, 0, "nobody logs in as root");
    assert!(admin.uid >= accountdb::FIRST_UID);
    assert!(admin.is_admin());
    assert!(!db.user("user").unwrap().is_admin());
    assert_eq!(db.group_view(), format!("{ADMIN_GROUP}:10:admin\n"));
    assert_eq!(db.next_uid, 1002);
}

#[test]
fn the_setup_seed_has_no_account_and_no_home() {
    let db = database(Seed::Setup);
    assert!(db.needs_setup());
    assert_eq!(db.groups.len(), 1);
    assert!(homes(&db).is_empty());
    assert_eq!(homes(&database(Seed::Accounts)).len(), 2);
}

#[test]
fn the_build_is_reproducible() {
    assert_eq!(database(Seed::Accounts), database(Seed::Accounts));
    assert_eq!(file_mode(fhs::etc::PASSWD), 0o644);
}

#[test]
fn the_database_is_a_seed_owned_by_accounts_and_the_views_are_its() {
    let db = database(Seed::Accounts);
    let mut files = OsFiles::default();
    place(&mut files, &db);
    let placed = files.files();
    let find = |path: &str| placed.iter().find(|file| file.path == path).unwrap();
    let store = find(fhs::state::ACCOUNTS_DB);
    assert_eq!(store.mode, 0o600);
    assert_eq!(store.placement, Placement::seed(ACCOUNTS_UID, ACCOUNTS_UID));
    let view = find(fhs::etc::PASSWD);
    assert_eq!(view.placement, Placement::owned(ACCOUNTS_UID, ACCOUNTS_UID));
    assert!(placed.iter().all(|file| file.path != fhs::etc::SHADOW));

    // An update keeps the database the running system wrote.
    let io = MemIo::new(8 << 20);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: (8 << 20) / 4096,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&io, geometry, "lazyos", [7; 16], 1).unwrap();
    let volume = Ext2::open(Box::new(io), || 1).unwrap();
    let layout = dirs(&homes(&db));
    let manifest = write_volume(&volume, None, &layout, &placed, 1).unwrap();
    assert!(!manifest.entries.contains_key(fhs::state::ACCOUNTS_DB));
    let meta = volume.lookup(fhs::state::ACCOUNTS_DB).unwrap();
    assert_eq!(
        (meta.uid, meta.gid, meta.mode & 0o7777),
        (ACCOUNTS_UID, ACCOUNTS_UID, 0o600)
    );
    let dir = volume.lookup(fhs::state::ACCOUNTS_DIR).unwrap();
    assert_eq!((dir.uid, dir.mode & 0o7777), (ACCOUNTS_UID, 0o700));
    let changed = b"next:1003\n".to_vec();
    volume
        .write_file(
            fhs::state::ACCOUNTS_DB,
            &changed,
            0o600,
            ACCOUNTS_UID,
            ACCOUNTS_UID,
            2,
        )
        .unwrap();
    write_volume(&volume, Some(&manifest), &layout, &placed, 3).unwrap();
    assert_eq!(volume.read_file(fhs::state::ACCOUNTS_DB).unwrap(), changed);
}
