//! Soaks: many link/unlink/rename cycles over open and closed files, and many
//! shared mappings through one name with the other unlinked, must give back
//! every block, inode, ramfs node, descriptor and registry entry, and leave
//! the volume consistent.

use super::*;

const ROUNDS: usize = 120;
const MAP_ROUNDS: usize = 40;

/// One cycle on `base`: a file gets three names, loses two of them (one while
/// open, one by a rename over it), is renamed, gets a name back and is
/// unlinked; the descriptor reads its data throughout.
fn cycle(base: &str, round: usize) -> Result<(), String> {
    let name = |tag: &str| format!("{base}/k{tag}");
    let tag = format!("cycle {round:05}");
    put(&name("a"), tag.as_bytes())?;
    check!(link(&name("a"), &name("b")) == 0, "round {round}: link b");
    check!(link(&name("b"), &name("c")) == 0, "round {round}: link c");
    let fd = open(&name("c"), O_RDWR);
    check!(fd < 64, "round {round}: open gave {fd:#x}");
    check!(
        path_call(SYS_UNLINK, &name("c"), 0) == 0,
        "round {round}: unlink c"
    );
    put(&name("t"), b"other")?;
    let (t, b) = (cstr(&name("t")), cstr(&name("b")));
    check!(
        syscall(82, t.as_ptr() as u64, b.as_ptr() as u64, 0, 0) == 0,
        "round {round}: rename over b"
    );
    let (a, d) = (cstr(&name("a")), cstr(&name("d")));
    check!(
        syscall(82, a.as_ptr() as u64, d.as_ptr() as u64, 0, 0) == 0,
        "round {round}: rename a to d"
    );
    check!(fd_links(fd)? == 1, "round {round}: count");
    check!(link(&name("d"), &name("e")) == 0, "round {round}: link e");
    check!(
        pread(fd, tag.len(), 0) == Ok(tag.clone().into_bytes()),
        "round {round}: the descriptor's file"
    );
    for gone in ["b", "d", "e"] {
        check!(
            path_call(SYS_UNLINK, &name(gone), 0) == 0,
            "round {round}: unlink {gone}"
        );
    }
    check!(
        fd_links(fd)? == 0,
        "round {round}: count after the last name"
    );
    check!(close(fd) == 0, "round {round}: close");
    Ok(())
}

pub fn link_unlink_rename_cycles() -> Result<(), String> {
    let data = Data::new(0)?;
    let space = free_space()?;
    let tmp = tmp_usage()?;
    for round in 0..ROUNDS {
        for base in BASES {
            cycle(base, round)?;
        }
    }
    check!(
        free_space()? == space,
        "/data leaked: {space:?} -> {:?}",
        free_space()?
    );
    check!(
        tmp_usage()? == tmp,
        "/tmp leaked: {tmp:?} -> {:?}",
        tmp_usage()?
    );
    for base in BASES {
        check!(parked_in(base)? == 0, "{base}: a parked file was left");
    }
    data.check_clean()
}

pub fn mappings_through_names() -> Result<(), String> {
    let data = Data::new(0)?;
    let space = free_space()?;
    let tmp = tmp_usage()?;
    let frames = mem::frame_stats().free;
    for round in 0..MAP_ROUNDS {
        for base in BASES {
            mapping::map_unlink_store(base, round)?;
        }
    }
    check!(free_space()? == space, "/data leaked");
    check!(tmp_usage()? == tmp, "/tmp leaked");
    check!(
        mem::frame_stats().free + 8 >= frames,
        "frames leaked: {frames} -> {}",
        mem::frame_stats().free
    );
    data.check_clean()
}
