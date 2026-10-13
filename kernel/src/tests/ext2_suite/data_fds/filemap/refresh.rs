//! A change behind the cache must not clobber a page the user stored to.

use super::*;
use crate::fs::vfs::Filesystem;

/// A change behind the cache does not overwrite a page that was stored to.
pub fn refresh_keeps_a_stored_page() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/kept", &pattern(PAGE as usize, 9))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw("/data/kept")?;
    check!(
        mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    space.store(BASE + 4, b"user store")?;
    data.fs
        .write("kept", 100, b"native")
        .map_err(|_| "native write")?;
    crate::fs::openfile::content_changed("/data/kept", true);
    check!(
        space.load(BASE + 4, 10)? == b"user store",
        "a refresh overwrote a stored page"
    );
    check!(munmap(BASE, PAGE) == 0, "munmap");
    close(fd);
    drop(space);
    data.check_clean()
}
