//! `confd`'s store on the ext2 OS volume (`/conf`, issues #407 and #508).
//!
//! `confd_suite` drives the service over an in-memory [`StoreFs`]; this suite
//! binds the same `confd::Confd` to the real ext2 driver through the VFS, the
//! way the ring-3 binary binds it to its directory (`user/src/bin/confd/
//! storage.rs`), so persist-and-reload is checked against real blocks: a
//! remount (reboot), a power cut at every write of a commit, and many
//! generations of rewrites that must not leak blocks.

use super::*;
use confd::{Caller, ChangeSink, Confd, StoreFs, Value, TMP_FILE};

/// The store directory `confd` prefers: `/conf` on the OS volume.
pub(super) const DIR: &str = confd::dir::PREFERRED_DIR;
const ROOT: Caller = Caller { uid: 0 };
const ALICE: Caller = Caller { uid: 1000 };

/// [`StoreFs`] over a VFS directory: what the binary does with its syscalls.
pub(super) struct VfsStore {
    pub(super) vfs: Vfs,
    /// The directory holding the store files ([`DIR`], or a seed source).
    pub(super) dir: &'static str,
}

fn path(name: &str) -> String {
    format!("{DIR}/{name}")
}

impl VfsStore {
    fn file(&self, name: &str) -> String {
        format!("{}/{name}", self.dir)
    }
}

impl StoreFs for VfsStore {
    type Error = FsError;

    fn read_file(&mut self, name: &str) -> Result<Option<Vec<u8>>, FsError> {
        match self.vfs.read_file(Id::ROOT, &self.file(name)) {
            Ok(data) => Ok(Some(data)),
            Err(FsError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn write_file(&mut self, name: &str, data: &[u8]) -> Result<(), FsError> {
        let path = self.file(name);
        match self.vfs.create(Id::ROOT, &path, 0o600) {
            Ok(_) => {}
            Err(FsError::Exists) => self.vfs.truncate(Id::ROOT, &path, 0)?,
            Err(error) => return Err(error),
        }
        self.vfs.write(Id::ROOT, &path, 0, data).map(|_| ())
    }

    fn fsync(&mut self, name: &str) -> Result<(), FsError> {
        self.vfs.flush(Id::ROOT, &self.file(name))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), FsError> {
        self.vfs.rename(Id::ROOT, &self.file(from), &self.file(to))
    }

    fn remove(&mut self, name: &str) -> Result<(), FsError> {
        match self.vfs.unlink(Id::ROOT, &self.file(name)) {
            Ok(()) | Err(FsError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Announcements are `confd_suite`'s business; this suite only persists.
pub(super) struct Quiet;

impl ChangeSink for Quiet {
    fn changed(&mut self, _path: &str, _deleted: bool) {}
}

pub(super) type Service = Confd<VfsStore, Quiet>;

pub(super) fn fail(error: confd::ServiceError) -> String {
    String::from(error.message())
}

/// A fresh volume with the store directory made 0700, as the image build
/// does (`build_support/os_layout.rs`), at a test mount of the OS volume.
pub(super) fn volume() -> Result<(Arc<Ext2>, Vfs, &'static FakeDisk), String> {
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    vfs.mkdir(Id::ROOT, DIR, 0o700).map_err(fs_error)?;
    fs.flush().map_err(fs_error)?;
    Ok((fs, vfs, disk))
}

/// Start `confd` on a fresh mount of `disk`, as a boot would.
pub(super) fn boot(disk: &'static FakeDisk) -> Result<(Arc<Ext2>, Service), String> {
    let (fs, vfs) = remount_disk(disk)?;
    let service = Confd::load(VfsStore { vfs, dir: DIR }, Quiet).map_err(fail)?;
    Ok((fs, service))
}

pub(super) fn value(service: &Service, key: &str, caller: Caller) -> Result<Option<Value>, String> {
    Ok(service.get(key, caller).map_err(fail)?.cloned())
}

/// Settings written through the service are all there after a remount, a
/// deleted key stays deleted, and no temporary file is left behind.
pub fn confd_store_survives_remount() -> Result<(), String> {
    task::register_kernel();
    let (fs, vfs, disk) = volume()?;
    drop((fs, vfs));

    let (fs, mut service) = boot(disk)?;
    let light = Value::Str(String::from("light"));
    service
        .set("sys/ui/mode", light.clone(), ROOT)
        .map_err(fail)?;
    service
        .set("sys/ui/accent", Value::U64(0x336699), ROOT)
        .map_err(fail)?;
    service
        .set("sys/time/clock24", Value::Bool(false), ROOT)
        .map_err(fail)?;
    service
        .set("sys/ui/anim", Value::Bool(false), ROOT)
        .map_err(fail)?;
    service.delete("sys/ui/anim", ROOT).map_err(fail)?;
    service
        .set("user/1000/theme", Value::Str(String::from("dark")), ALICE)
        .map_err(fail)?;
    fs.flush().map_err(fs_error)?;
    drop((fs, service));

    let (_fs, service) = boot(disk)?;
    check!(
        value(&service, "sys/ui/mode", ROOT)? == Some(light),
        "the mode did not survive the remount"
    );
    check!(
        value(&service, "sys/ui/accent", ROOT)? == Some(Value::U64(0x336699)),
        "the accent did not survive the remount"
    );
    check!(
        value(&service, "sys/time/clock24", ROOT)? == Some(Value::Bool(false)),
        "the clock format did not survive the remount"
    );
    check!(
        value(&service, "sys/ui/anim", ROOT)?.is_none(),
        "a deleted key came back after the remount"
    );
    check!(
        value(&service, "user/1000/theme", ALICE)? == Some(Value::Str(String::from("dark"))),
        "a user key did not survive the remount"
    );
    let (_fs2, mut vfs) = remount_disk(disk)?;
    check!(
        vfs.stat(Id::ROOT, &path(TMP_FILE)).err() == Some(FsError::NotFound),
        "a committed write left its temporary file"
    );
    Ok(())
}

/// Cut the power at every write of one commit in turn: after the next boot
/// the key reads as the old value or the new one, never as neither (a lost or
/// corrupt store), and a commit that reported success is the new value.
pub fn confd_store_power_cut_sweep() -> Result<(), String> {
    task::register_kernel();
    let (fs, vfs, disk) = volume()?;
    drop((fs, vfs));
    let (fs, mut service) = boot(disk)?;
    let old = Value::Str(String::from("dark"));
    let new = Value::Str(String::from("light"));
    service
        .set("sys/ui/mode", old.clone(), ROOT)
        .map_err(fail)?;
    service
        .set("sys/net/mtu", Value::U64(1500), ROOT)
        .map_err(fail)?;
    fs.flush().map_err(fs_error)?;
    drop((fs, service));
    let pristine = disk.data.lock().clone();

    let mut completed = false;
    for k in 1..200 {
        disk.data.lock().copy_from_slice(&pristine);
        let (fs, mut service) = boot(disk)?;
        disk.cut_power_at(k);
        let finished = service.set("sys/ui/mode", new.clone(), ROOT).is_ok();
        disk.fail_nth_write(u32::MAX);
        drop((fs, service));

        let (_fs, service) = boot(disk)?;
        let after = value(&service, "sys/ui/mode", ROOT)?;
        check!(
            after == Some(old.clone()) || after == Some(new.clone()),
            "power cut at write {k}: the key reads {after:?}"
        );
        check!(
            !finished || after == Some(new.clone()),
            "power cut at write {k}: a successful commit was lost"
        );
        check!(
            value(&service, "sys/net/mtu", ROOT)? == Some(Value::U64(1500)),
            "power cut at write {k}: an untouched key was lost"
        );
        if finished {
            completed = true;
            break;
        }
    }
    check!(completed, "the commit never completed within 200 writes");
    Ok(())
}

/// Soak: many boots, each verifying the previous generation and rewriting
/// every key. The store keeps one shape, so the volume's free blocks and
/// inodes must be exactly the same at the end: any leak in the rewrite path
/// (temporary file, rename over the old store) shows up as a drift.
pub fn confd_store_soak_generations() -> Result<(), String> {
    const GENERATIONS: u64 = 120;
    const KEYS: u64 = 24;
    task::register_kernel();
    let (fs, vfs, disk) = volume()?;
    drop((fs, vfs));

    let mut baseline = None;
    for generation in 0..GENERATIONS {
        let (fs, mut service) = boot(disk)?;
        for key in 0..KEYS {
            let path = format!("sys/soak/{key:02}");
            let expected = generation
                .checked_sub(1)
                .map(|previous| Value::U64(previous * KEYS + key));
            check!(
                value(&service, &path, ROOT)? == expected,
                "generation {generation}: {path} did not survive the remount"
            );
            service
                .set(&path, Value::U64(generation * KEYS + key), ROOT)
                .map_err(fail)?;
        }
        fs.flush().map_err(fs_error)?;
        drop((fs, service));
        let free = bitmap_free(disk, 512);
        match baseline {
            None => baseline = Some(free),
            Some(first) => check!(
                free == first,
                "generation {generation}: free (blocks, inodes) went from {first:?} to {free:?}"
            ),
        }
    }
    Ok(())
}
