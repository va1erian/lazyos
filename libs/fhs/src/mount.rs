//! Mount points of the VFS.

/// The root: the FAT boot volume. Written by the image build.
/// Target (F1): the ext2 OS volume.
pub const ROOT: &str = "/";

/// The ramfs for temporary files. Written by anyone. Target (F1): the
/// `/transient` ramfs (`TRANSIENT`); the Linux ABI keeps `/tmp` as its alias.
pub const TMP: &str = "/tmp";

/// The transitional `/data` tree (a directory on the OS volume, or the
/// optional data disk). Nothing new is written there since F4 but `lazyrad`'s
/// files (until it moves to the home); `confd` reads
/// `state::LEGACY_DATA_CONFD` once as a seed. Target (F7): gone; its contents
/// move to `/conf`, `/apps`, `/logs` and `/home`.
pub const DATA: &str = "/data";

/// The boot partition. Target (F1): `"/boot"`. Not used yet.
#[doc(hidden)]
pub const BOOT: &str = "/boot";

/// The home volume, one directory per user (`state::HOME_ROOT`); without one,
/// the OS volume's own `/home` directories.
pub const HOME: &str = "/home";

/// The ramfs for temporary files and runtime state. Target (F1):
/// `"/transient"`. Not used yet.
#[doc(hidden)]
pub const TRANSIENT: &str = "/transient";
