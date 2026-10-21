//! The ramfs's own bookkeeping of names: link counts, the charge of a hard
//! link to whoever made it (so the per-owner half of the caps bounds names
//! too), the hand-over when the name the node's charge covered goes first,
//! and directories' counts (`.`, `..`).

use super::*;
use crate::fs::ramfs::{RamFs, Usage};
use crate::fs::vfs::{Filesystem, MountFlags};

const ALICE: Id = Id::new(1000, 100);
const BOB: Id = Id::new(2000, 200);

fn nlink(vfs: &mut Vfs, path: &str) -> Result<u32, String> {
    Ok(vfs.stat(Id::ROOT, path).map_err(fs_error)?.nlink)
}

pub fn counts_and_charges() -> Result<(), String> {
    let fs = Arc::new(RamFs::with_limits(1 << 20, 64));
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone(), MountFlags::default())
        .map_err(fs_error)?;
    vfs.set_umask(0);
    vfs.mkdir(Id::ROOT, "/s", 0o777).map_err(fs_error)?;
    vfs.create(ALICE, "/s/f", 0o666).map_err(fs_error)?;
    vfs.write(ALICE, "/s/f", 0, b"data").map_err(fs_error)?;
    let alice = fs.usage_of(ALICE.uid);
    let (_, live) = fs.usage();
    check!(
        alice == Usage { bytes: 4, nodes: 1 },
        "alice holds {alice:?}"
    );

    // A link is charged to the one who made it, as a node.
    vfs.link(BOB, "/s/f", "/s/g").map_err(fs_error)?;
    check!(
        fs.usage_of(BOB.uid) == Usage { bytes: 0, nodes: 1 },
        "bob's link is not charged to bob"
    );
    check!(fs.usage().1 == live + 1, "a link costs no node of the cap");
    check!(
        nlink(&mut vfs, "/s/f")? == 2 && nlink(&mut vfs, "/s/g")? == 2,
        "link counts"
    );
    let node = fs.open_node("s/g").map_err(fs_error)?.ok_or("no node")?;
    check!(
        fs.open_node("s/f").map_err(fs_error)? == Some(node),
        "two names open two nodes"
    );

    // The covered name goes first: bob's name takes its place and bob's
    // charge goes; the node stays alice's, with its bytes.
    vfs.unlink(ALICE, "/s/f").map_err(fs_error)?;
    check!(
        fs.usage_of(BOB.uid) == Usage::default(),
        "bob still pays for a name that is now the file's own"
    );
    check!(fs.usage_of(ALICE.uid) == alice, "the node's charge moved");
    check!(nlink(&mut vfs, "/s/g")? == 1, "count after unlink");
    check!(
        vfs.read_file(Id::ROOT, "/s/g").map_err(fs_error)? == b"data",
        "the other name lost the data"
    );

    // Bob may make names only up to his half of the node cap.
    let mut made = 0;
    loop {
        match vfs.link(BOB, "/s/g", &format!("/s/l{made}")) {
            Ok(()) => made += 1,
            Err(FsError::NoSpace) => break,
            Err(error) => return Err(format!("link {made}: {}", fs_error(error))),
        }
        check!(made <= 64, "links never ran out");
    }
    check!(
        made > 0 && fs.usage_of(BOB.uid).nodes <= 32,
        "bob made {made} links, holding {:?}",
        fs.usage_of(BOB.uid)
    );
    check!(
        nlink(&mut vfs, "/s/g")? == made + 1,
        "count with {made} links"
    );
    for index in 0..made {
        vfs.unlink(Id::ROOT, &format!("/s/l{index}"))
            .map_err(fs_error)?;
    }
    check!(
        fs.usage_of(BOB.uid) == Usage::default(),
        "unlinking every link did not give bob's charges back"
    );
    vfs.unlink(Id::ROOT, "/s/g").map_err(fs_error)?;
    check!(
        fs.usage_of(ALICE.uid) == Usage::default(),
        "the last name did not free the file"
    );
    check!(
        fs.read_node(node, 0, &mut [0u8; 4]) == Err(FsError::NotFound),
        "the node of a deleted file still reads"
    );
    check!(fs.usage() == (0, live - 1), "usage {:?}", fs.usage());
    Ok(())
}

/// A directory counts `.`, its entry in the parent and each child's `..`,
/// on ext2 and the ramfs alike: `mkdir`, `rmdir` and moving a directory
/// across parents keep both counts right.
pub fn directory_link_counts() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        let root = ino_links(base)?.1;
        let (d, e) = (format!("{base}/cd"), format!("{base}/cd/ce"));
        check!(path_call(SYS_MKDIR, &d, 0o755) == 0, "mkdir {d}");
        check!(ino_links(&d)?.1 == 2, "{base}: a new directory");
        check!(ino_links(base)?.1 == root + 1, "{base}: its parent");
        check!(path_call(SYS_MKDIR, &e, 0o755) == 0, "mkdir {e}");
        check!(ino_links(&d)?.1 == 3, "{base}: a parent of one");
        // Move the child up: the old parent loses its `..`, the new gains.
        let moved = format!("{base}/ce");
        let (from, to) = (cstr(&e), cstr(&moved));
        check!(
            syscall(82, from.as_ptr() as u64, to.as_ptr() as u64, 0, 0) == 0,
            "rename {e}"
        );
        check!(ino_links(&d)?.1 == 2, "{base}: the old parent");
        check!(ino_links(base)?.1 == root + 2, "{base}: the new parent");
        // A directory over an empty one: the victim's `..` goes.
        let (from, to) = (cstr(&moved), cstr(&d));
        check!(
            syscall(82, from.as_ptr() as u64, to.as_ptr() as u64, 0, 0) == 0,
            "rename over {d}"
        );
        check!(ino_links(base)?.1 == root + 1, "{base}: after the replace");
        check!(path_call(SYS_RMDIR, &d, 0) == 0, "rmdir {d}");
        check!(ino_links(base)?.1 == root, "{base}: after rmdir");
    }
    data.check_clean()
}
