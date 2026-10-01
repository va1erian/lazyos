//! Mount points of the VFS.

/// The root: the FAT boot volume. Written by the image build.
/// Target (F1): the ext2 OS volume.
pub const ROOT: &str = "/";

/// The ramfs for temporary files. Written by anyone. Target (F1): the
/// `/transient` ramfs (`TRANSIENT`); the Linux ABI keeps `/tmp` as its alias.
pub const TMP: &str = "/tmp";

/// The optional ext2 data volume (a second disk). Written by `confd`, `pkgd`,
/// users and `lazyrad`. Target (F7): gone; its contents move to `/conf`,
/// `/apps`, `/logs` and `/home`.
pub const DATA: &str = "/data";

/// The boot partition. Target (F1): `"/boot"`. Not used yet.
#[doc(hidden)]
pub const BOOT: &str = "/boot";

/// The home volume, one directory per user. Target (F1): `"/home"`. Not used
/// yet; today's home directories are `state::HOME_ROOT`.
#[doc(hidden)]
pub const HOME: &str = "/home";

/// The ramfs for temporary files and runtime state. Target (F1):
/// `"/transient"`. Not used yet.
#[doc(hidden)]
pub const TRANSIENT: &str = "/transient";
