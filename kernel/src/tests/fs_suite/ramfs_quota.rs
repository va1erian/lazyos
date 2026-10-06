//! Per-uid caps on a ramfs (issue #265): a node's bytes and the node itself
//! are charged to its owner, one non-root owner may hold half of each cap,
//! `chown` moves the charge, and root is bound only by the filesystem caps.

use super::*;
use crate::fs::ramfs::Usage;
use crate::fs::vfs::{Filesystem, SetAttr};

fn user(uid: u32) -> Id {
    Id { uid, gid: uid }
}

fn usage(bytes: usize, nodes: usize) -> Usage {
    Usage { bytes, nodes }
}

/// One user fills its half of the byte and node caps and is refused past
/// them with the data intact, while another user and root still have room.
pub fn ramfs_per_uid_caps() -> Result<(), String> {
    let ram = RamFs::with_limits(4096, 20);
    let (alice, bob) = (user(1001), user(1002));
    ram.create("a", 0o644, alice).map_err(fs_error)?;
    ram.write("a", 0, &[1u8; 2048]).map_err(fs_error)?;
    check!(
        ram.write("a", 2048, &[2u8]) == Err(FsError::NoSpace),
        "alice wrote past her half of the bytes"
    );
    check!(
        ram.truncate("a", 3000) == Err(FsError::NoSpace),
        "alice grew past her half by truncate"
    );
    let mut buf = [0u8; 4];
    check!(
        ram.read("a", 2044, &mut buf).map_err(fs_error)? == 4 && buf == [1; 4],
        "a refused write damaged the file"
    );
    check!(
        ram.usage_of(1001) == usage(2048, 1),
        "alice: {:?}",
        ram.usage_of(1001)
    );
    // Bob is not affected by Alice's charge.
    ram.create("b", 0o644, bob).map_err(fs_error)?;
    ram.write("b", 0, &[3u8; 1024]).map_err(fs_error)?;
    // Nodes: ten per user (half of 20).
    for index in 1..10 {
        ram.create(&format!("a{index}"), 0o644, alice)
            .map_err(fs_error)?;
    }
    check!(
        ram.create("a10", 0o644, alice) == Err(FsError::NoSpace),
        "alice created past her half of the nodes"
    );
    ram.mkdir("bd", 0o755, bob).map_err(fs_error)?;
    // Root is held only by the filesystem caps.
    ram.create("r", 0o644, Id::ROOT).map_err(fs_error)?;
    ram.write("r", 0, &[4u8; 1024]).map_err(fs_error)?;
    check!(
        ram.write("r", 1024, &[4u8]) == Err(FsError::NoSpace),
        "root wrote past the filesystem cap"
    );
    // Shrinking gives bytes back.
    ram.truncate("a", 1000).map_err(fs_error)?;
    check!(
        ram.usage_of(1001) == usage(1000, 10),
        "after truncate: {:?}",
        ram.usage_of(1001)
    );
    ram.write("a", 1000, &[5u8; 1048]).map_err(fs_error)?;
    check!(
        ram.usage_of(1002) == usage(1024, 2),
        "bob: {:?}",
        ram.usage_of(1002)
    );
    Ok(())
}

/// `chown` moves a node's charge to its new owner (and can put that owner
/// over its cap, since changing an owner is privileged); removing the node
/// frees the new owner's charge, and a rename over a file frees the victim's
/// owner.
pub fn ramfs_chown_moves_the_charge() -> Result<(), String> {
    let ram = RamFs::with_limits(4096, 16);
    ram.create("f", 0o644, user(1001)).map_err(fs_error)?;
    ram.write("f", 0, &[1u8; 1500]).map_err(fs_error)?;
    ram.create("g", 0o644, user(1002)).map_err(fs_error)?;
    ram.write("g", 0, &[2u8; 1500]).map_err(fs_error)?;
    let chown = SetAttr {
        uid: Some(1002),
        ..SetAttr::default()
    };
    ram.setattr("f", &chown).map_err(fs_error)?;
    check!(
        ram.usage_of(1001) == Usage::default(),
        "the old owner kept {:?}",
        ram.usage_of(1001)
    );
    check!(
        ram.usage_of(1002) == usage(3000, 2),
        "the new owner holds {:?}",
        ram.usage_of(1002)
    );
    check!(
        ram.write("g", 1500, &[0u8]) == Err(FsError::NoSpace),
        "an owner over its cap could still grow"
    );
    ram.create("h", 0o644, user(1001)).map_err(fs_error)?;
    ram.write("h", 0, &[3u8; 10]).map_err(fs_error)?;
    ram.rename("h", "f").map_err(fs_error)?;
    check!(
        ram.usage_of(1002) == usage(1500, 1),
        "the replaced file's owner holds {:?}",
        ram.usage_of(1002)
    );
    ram.unlink("f").map_err(fs_error)?;
    ram.unlink("g").map_err(fs_error)?;
    check!(
        ram.usage_of(1001) == Usage::default() && ram.usage_of(1002) == Usage::default(),
        "removal left {:?} / {:?}",
        ram.usage_of(1001),
        ram.usage_of(1002)
    );
    check!(ram.usage() == (0, 1), "the tree kept {:?}", ram.usage());
    Ok(())
}

/// Thousands of creates, writes, truncates, renames, chowns and removals by
/// several owners: every owner's charge ends at zero and so does the tree.
pub fn ramfs_per_uid_soak() -> Result<(), String> {
    const ROUNDS: usize = 3_000;
    let ram = RamFs::with_limits(64 * 1024, 64);
    let owners = [user(2001), user(2002), user(2003), Id::ROOT];
    for round in 0..ROUNDS {
        let owner = owners[round % owners.len()];
        let name = format!("f{}", round % 7);
        let other = format!("f{}", (round + 3) % 7);
        if ram.lookup(&name).is_err() {
            ram.create(&name, 0o644, owner).map_err(fs_error)?;
        }
        let len = (round * 37) % 9000;
        match ram.write(&name, 0, &alloc::vec![7u8; len]) {
            Ok(_) | Err(FsError::NoSpace) => {}
            Err(error) => return Err(format!("round {round}: write {}", error.message())),
        }
        match round % 5 {
            0 => ram.truncate(&name, (len / 2) as u64).map_err(fs_error)?,
            1 => {
                let to = owners[(round + 1) % owners.len()].uid;
                ram.setattr(
                    &name,
                    &SetAttr {
                        uid: Some(to),
                        ..SetAttr::default()
                    },
                )
                .map_err(fs_error)?;
            }
            2 if ram.lookup(&other).is_ok() => ram.rename(&name, &other).map_err(fs_error)?,
            3 => ram.unlink(&name).map_err(fs_error)?,
            _ => {}
        }
        // The owners' charges always add up to the tree (less its root).
        let (bytes, nodes) = owners.iter().fold((0, 0), |(bytes, nodes), owner| {
            let held = ram.usage_of(owner.uid);
            (bytes + held.bytes, nodes + held.nodes)
        });
        let (tree_bytes, tree_nodes) = ram.usage();
        if (bytes, nodes + 1) != (tree_bytes, tree_nodes) {
            return Err(format!(
                "round {round}: owners hold {bytes}/{nodes}, the tree {tree_bytes}/{tree_nodes}"
            ));
        }
    }
    for index in 0..7 {
        let _ = ram.unlink(&format!("f{index}"));
    }
    for owner in owners {
        check!(
            ram.usage_of(owner.uid) == Usage::default(),
            "uid {} kept {:?}",
            owner.uid,
            ram.usage_of(owner.uid)
        );
    }
    check!(ram.usage() == (0, 1), "the tree kept {:?}", ram.usage());
    Ok(())
}
