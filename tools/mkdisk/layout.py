"""What a volume contains besides ``lost+found``: owned, moded directories.

There is no ``chown`` yet (the VFS attribute work is tracked separately), so
ownership has to be right when the volume is formatted. A :class:`Layout`
describes the root directory's attributes plus a tree of extra directories;
:mod:`mkdisk.tree` turns it into inodes.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from .accounts import Account, demo_accounts

MODE_MAX = 0o7777
# The driver keeps only the low 16 bits of i_uid/i_gid (`check_owner` in
# libs/ext2fs/src/layout.rs refuses more), so the formatter must too.
ID_MAX = 0xFFFF
NAME_MAX = 255
STICKY_WORLD_WRITABLE = 0o1777
# A home: only its owner may enter (docs/filesystem-plan.md F4).
PRIVATE = 0o700
HOMES = "/home"
LOST_FOUND = "/lost+found"  # made by the formatter itself


@dataclass(frozen=True)
class DirSpec:
    """One directory: where it lives and who may do what in it."""

    path: str
    mode: int = 0o755
    uid: int = 0
    gid: int = 0

    def __post_init__(self) -> None:
        check_attributes(self.path, self.mode, self.uid, self.gid)

    @property
    def parent(self) -> str:
        """The containing directory's path (``/`` for top-level entries)."""
        return self.path.rsplit("/", 1)[0] or "/"

    @property
    def name(self) -> str:
        """The last path component."""
        return self.path.rsplit("/", 1)[1]


def check_attributes(label: str, mode: int, uid: int, gid: int) -> None:
    """Reject values that would not survive the trip into an inode."""
    if not 0 <= mode <= MODE_MAX:
        raise ValueError(f"{label}: mode {mode:o} is outside 0..{MODE_MAX:o}")
    for kind, value in (("uid", uid), ("gid", gid)):
        if not 0 <= value <= ID_MAX:
            raise ValueError(f"{label}: {kind} {value} is outside 0..{ID_MAX}")


def _check_path(spec: DirSpec, seen: set[str]) -> None:
    """One directory must be new, well formed and under a listed parent."""
    path = spec.path
    if not path.startswith("/") or path == "/" or path.endswith("/") or "//" in path:
        raise ValueError(f"directory path {path!r} must be absolute and normalised")
    if not spec.name or len(spec.name.encode()) > NAME_MAX or "\0" in path:
        raise ValueError(f"directory name in {path!r} is empty, too long or has a NUL")
    if spec.name in (".", ".."):
        raise ValueError(f"{path!r} may not use . or ..")
    if path in seen or path == LOST_FOUND:
        raise ValueError(f"directory {path!r} is listed twice (or is reserved)")
    if spec.parent not in seen:
        raise ValueError(f"parent of {path!r} is not created before it")


@dataclass(frozen=True)
class Layout:
    """The root directory's attributes and the directories created under it.

    ``dirs`` must list every parent before its children; that keeps inode
    numbers deterministic and lets validation be a single pass.
    """

    root_mode: int = 0o755
    root_uid: int = 0
    root_gid: int = 0
    dirs: tuple[DirSpec, ...] = field(default_factory=tuple)

    def __post_init__(self) -> None:
        check_attributes("/", self.root_mode, self.root_uid, self.root_gid)
        seen = {"/"}
        for spec in self.dirs:
            _check_path(spec, seen)
            seen.add(spec.path)


EMPTY = Layout()


def home_dirs(accounts: list[Account]) -> tuple[DirSpec, ...]:
    """``/home/<user>`` for every account whose home lives under ``/home``.

    An account homed elsewhere (a service's ``/``) is skipped on purpose: the data
    volume hosts *user* homes, and a directory nobody logs in to is clutter.
    """
    return tuple(
        DirSpec(f"{HOMES}/{account.name}", PRIVATE, account.uid, account.gid)
        for account in accounts
        if account.home == f"{HOMES}/{account.name}")


def seeded(root_mode: int = 0o755, root_uid: int = 0, root_gid: int = 0,
           accounts: list[Account] | None = None) -> Layout:
    """The demo layout: user homes plus a sticky world-writable ``/tmp``.

    ``/data`` itself stays root's (safe by default). ``accounts`` defaults to the
    ones in ``build_support/passwd`` (the system's ``/system/etc/passwd``), so the
    seed follows the source of truth.
    """
    users = demo_accounts() if accounts is None else accounts
    dirs = [DirSpec(HOMES), *home_dirs(users), DirSpec("/tmp", STICKY_WORLD_WRITABLE)]
    return Layout(root_mode, root_uid, root_gid, tuple(dirs))


def home_volume(root_mode: int = 0o755, root_uid: int = 0, root_gid: int = 0,
                accounts: list[Account] | None = None) -> Layout:
    """The home volume: ``<user>/`` at the volume root, no ``/home`` and no ``/tmp``.

    The volume is mounted at ``/home``, so the volume root *is* ``/home`` and each
    user directory has the owner and mode of today's ``/home/<user>``. ``/tmp``
    belongs to the OS volume (``libs/fhs``), never to a home volume.
    """
    users = demo_accounts() if accounts is None else accounts
    dirs = tuple(
        DirSpec(f"/{account.name}", PRIVATE, account.uid, account.gid)
        for account in users
        if account.home == f"{HOMES}/{account.name}")
    return Layout(root_mode, root_uid, root_gid, dirs)


def describe(layout: Layout) -> str:
    """One line per directory a format will create, for confirmations and the GUI."""
    lines = [f"/ (mode {layout.root_mode:04o}, uid {layout.root_uid}, gid {layout.root_gid})"]
    lines += [f"{spec.path} (mode {spec.mode:04o}, uid {spec.uid}, gid {spec.gid})"
              for spec in layout.dirs]
    return "\n".join(lines)
