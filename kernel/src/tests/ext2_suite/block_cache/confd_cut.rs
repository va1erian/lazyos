//! `confd`'s write-temp-then-rename commit (issue #407) on a *cached* volume:
//! the `confd_store_power_cut_sweep` of `confd_store.rs`, with the power cut
//! taking the cache's unwritten blocks with it. The rename's barriers
//! (`libs/ext2fs/src/commit.rs`) are what keep the key readable.

use super::*;
use confd::{Caller, Confd, Value};

const ROOT: Caller = Caller::system(0);

/// Start `confd` on a cached mount of `disk`, as a boot would.
fn boot(disk: &'static FakeDisk) -> Result<(Arc<Ext2>, Service), String> {
    let (fs, vfs) = cached(disk, 64)?;
    let service = Confd::load(VfsStore { vfs, dir: DIR }, Quiet).map_err(fail)?;
    Ok((fs, service))
}

/// Cut the power at every write request of one commit in turn, dropping
/// whatever the cache still held: the key reads as the old value or the new
/// one, never neither, and a commit that reported success is the new value.
pub fn confd_power_cut_sweep() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    {
        let (fs, mut vfs) = cached(disk, 64)?;
        vfs.mkdir(Id::ROOT, DIR, 0o700).map_err(fs_error)?;
        fs.flush().map_err(fs_error)?;
    }
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
        // The machine stops: the cache's dirty blocks die with it (the
        // unmount's writeback meets the dead disk), then the disk comes back.
        drop((fs, service));
        disk.fail_nth_write(u32::MAX);

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
    release(disk);
    Ok(())
}
