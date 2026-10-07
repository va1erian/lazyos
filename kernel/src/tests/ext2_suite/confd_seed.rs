//! `confd` seeding `/conf` from the F0 to F3 store in `/data/confd`, once
//! (issue #508): the same `Confd::seed_once` the ring-3 binary calls
//! (`user/src/bin/confd/storage.rs`), over real ext2 blocks and remounts.

use super::*;
use confd::{persist, Caller, Store, Value, STORE_FILE};

use super::confd_store::{boot, fail, value, volume, VfsStore};

const ROOT: Caller = Caller::system(0);
const LEGACY: &str = confd::dir::LEGACY_SEED;

/// Writes an F3-era store with `entries` in `/data/confd`, as an updated image
/// has it.
fn plant_legacy(vfs: Vfs, entries: &[(&str, Value)]) -> Result<Vfs, String> {
    let mut vfs = vfs;
    vfs.mkdir(Id::ROOT, fhs::mount::DATA, 0o755)
        .map_err(fs_error)?;
    vfs.mkdir(Id::ROOT, LEGACY, 0o700).map_err(fs_error)?;
    let mut store = Store::new();
    for (path, entry) in entries {
        store
            .set(path, entry.clone(), ROOT)
            .map_err(|_| String::from("bad seed entry"))?;
    }
    let mut legacy = VfsStore { vfs, dir: LEGACY };
    persist(&mut legacy, &store).map_err(fs_error)?;
    Ok(legacy.vfs)
}

/// One boot's seed step, as `seed_legacy` runs it.
fn seed(
    disk: &'static FakeDisk,
) -> Result<
    (
        Arc<Ext2>,
        super::confd_store::Service,
        Option<confd::Migration>,
    ),
    String,
> {
    let (fs, mut service) = boot(disk)?;
    let (_lfs, lvfs) = remount_disk(disk)?;
    let mut legacy = VfsStore {
        vfs: lvfs,
        dir: LEGACY,
    };
    let report = service.seed_once(Some(&mut legacy)).map_err(fail)?;
    fs.flush().map_err(fs_error)?;
    Ok((fs, service, report))
}

/// The first boot on `/conf` takes the legacy settings and writes the marker;
/// a setting deleted afterwards stays deleted across reboots; `/data/confd`
/// is byte-for-byte untouched.
pub fn confd_seed_from_data_once() -> Result<(), String> {
    task::register_kernel();
    let (fs, vfs, disk) = volume()?;
    let mut vfs = plant_legacy(
        vfs,
        &[
            ("sys/time/zone", Value::Str(String::from("Europe/Paris"))),
            ("sys/ui/anim", Value::Bool(false)),
        ],
    )?;
    fs.flush().map_err(fs_error)?;
    let before = vfs
        .read_file(Id::ROOT, &format!("{LEGACY}/{STORE_FILE}"))
        .map_err(fs_error)?;
    drop((fs, vfs));

    let (fs, mut service, report) = seed(disk)?;
    check!(
        report.map(|r| r.added) == Some(2),
        "the first boot did not seed both settings: {report:?}"
    );
    service.delete("sys/ui/anim", ROOT).map_err(fail)?;
    fs.flush().map_err(fs_error)?;
    drop((fs, service));

    for reboot in 0..3 {
        let (_fs, service, report) = seed(disk)?;
        check!(
            report.is_none(),
            "reboot {reboot}: the legacy store was read again"
        );
        check!(
            value(&service, "sys/ui/anim", ROOT)?.is_none(),
            "reboot {reboot}: a deleted setting came back from /data/confd"
        );
        check!(
            value(&service, "sys/time/zone", ROOT)?
                == Some(Value::Str(String::from("Europe/Paris"))),
            "reboot {reboot}: a seeded setting was lost"
        );
    }
    let (_fs, mut vfs) = remount_disk(disk)?;
    let after = vfs
        .read_file(Id::ROOT, &format!("{LEGACY}/{STORE_FILE}"))
        .map_err(fs_error)?;
    check!(before == after, "/data/confd was written");
    check!(
        vfs.stat(Id::ROOT, fhs::state::CONF_SEEDED_MARKER).is_ok(),
        "no seed marker in /conf"
    );
    Ok(())
}

/// A fresh image has no `/data/confd`: the seed is a no-op that still writes
/// the marker, and the store lives in `/conf`.
pub fn confd_seed_absent_legacy() -> Result<(), String> {
    task::register_kernel();
    let (fs, vfs, disk) = volume()?;
    drop((fs, vfs));
    let (fs, mut service) = boot(disk)?;
    let report = service.seed_once::<VfsStore>(None).map_err(fail)?;
    check!(
        report.map(|r| r.added) == Some(0),
        "an absent seed added settings"
    );
    service.set("sys/a", Value::U64(1), ROOT).map_err(fail)?;
    fs.flush().map_err(fs_error)?;
    drop((fs, service));
    let (_fs, mut vfs) = remount_disk(disk)?;
    check!(
        vfs.stat(Id::ROOT, &format!("{}/{STORE_FILE}", fhs::state::CONF_ROOT))
            .is_ok(),
        "the store is not in /conf"
    );
    check!(
        vfs.stat(Id::ROOT, fhs::state::CONF_SEEDED_MARKER).is_ok(),
        "no seed marker after an absent seed"
    );
    Ok(())
}
