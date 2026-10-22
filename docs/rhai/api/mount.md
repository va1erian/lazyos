# `sys::mount`

The Rhai module for the Messenger interface `os.lazy.mount.v1`. Wire-level reference: [os.lazy.mount.v1](../../idl/os.lazy.mount.v1.md).

The network mount service (docs/smb-plan.md §3.4): `mountd` starts and
supervises one user-space filesystem daemon per mount (`ftpfuse` for an
FTP server, `smbfuse` for an SMB share),
each serving `/mnt/<name>` through the FUSE mechanism, so a program that
may not register a filesystem itself (an installed app has no
capabilities) can still ask for one.

A mount belongs to its requester: its files are reported as owned by the
caller's kernel-stamped uid and gid, and only that uid (or root) may
unmount it. `Mount` answers at once with the mount in state `connecting`;
the daemon then logs in and the state becomes `mounted`, or `failed` with
a reason in `detail`. Callers poll `List`. A failed mount stays listed
until it is unmounted, so its reason can be read. Failures of a request
itself are returned as a structured error field (errno-style code,
friendly text): `EINVAL` for a malformed argument, `EEXIST` for a name in
use, `ENOENT` for an unknown name, `EPERM` for another user's mount,
`EAGAIN` when every slot is taken.

The service also automounts removable volumes (docs/usb-hotplug-plan.md):
a FAT USB stick plugged in while someone is logged in is served at
`/media/<label>` by a `fatfuse` as that user. `Volumes` lists them and
`Eject` finishes one: the daemon flushes everything, closes the filesystem
(the dirty flag is cleared) and unmounts, and only then is the stick safe
to pull. An eject is refused while a file is open on the volume, and the
refusal says who holds it.

## Functions

### `mount(name, host, port, user, password, kind, share)`

`Mount(name: String, host: String, port: U32, user: String, password: String, kind: String, share: String) -> (path: String)`

The mount's name: it is served at `path`, `/mnt/<name>`.
The filesystem daemon's protocol (`ftp` or `smb`).
The server's host name or address, as given.
The server's TCP port.
The account it logs in as (`anonymous` when none was given).
Where it is mounted.
`connecting`, `mounted` or `failed`.
Why a mount failed, empty otherwise.
The requester's uid: the files' owner.
The SMB share, empty for an FTP mount.
Mount a server at `/mnt/<name>`, logged in as `user` with `password`.
`kind` is `ftp` (or empty) for the FTP server `host:port`, where an
empty `user` is the anonymous login, or `smb` for the SMB 2.1 share
`\\host\share`, which needs a `user` (the NTLM domain comes from the
server). `name` is 1 to 32 of `a-z`, `0-9`, `-` and `_`; `host` 1 to
253 of letters, digits, `.` and `-`, not starting with `-` and with no
`:` (the port goes in `port`, 1 to 65535, 0 meaning 21 for FTP and 445
for SMB); `share` (SMB only) 1 to 80 characters with none of
`\ / : * ? " < > |`; `user` and `password` at most 128 bytes with no
control character, and a `password` only with a `user`. An SMB
password reaches `smbfuse` through its environment, never its
arguments. `path` is the mount point the daemon will serve.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `host` | `String` | string |
| `port` | `U32` | int |
| `user` | `String` | string |
| `password` | `String` | string |
| `kind` | `String` | string |
| `share` | `String` | string |

Returns `path`: `String` (string).

```rhai
let result = sys::mount::mount(name, host, port, user, password, kind, share);
```

### `unmount(name)`

`Unmount(name: String) -> ()`

Stop the mount `name` and forget it (a failed one is just forgotten).
The daemon is stopped at once; the kernel takes the dead mount out of
`/mnt` within a few seconds, and until then it fails every call.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |

Returns `()`.

```rhai
let result = sys::mount::unmount(name);
```

### `list()`

`List() -> (mounts: Array<MountInfo>)`

Every mount the service knows about, in the order they were asked for.

Returns `mounts`: `Array<MountInfo>` (array of map (`MountInfo`)).

```rhai
let result = sys::mount::list();
```

### `volumes()`

`Volumes() -> (volumes: Array<VolumeInfo>)`

The disk, `usb<n>`: what `Eject` names.
The mount's name, the volume's label made safe for a path; empty
until the volume is mounted.
Where it is mounted, `/media/<name>`; empty until mounted.
`starting` (a daemon is looking for a volume), `mounted`,
`ejecting`, `ejected` (flushed and unmounted: safe to pull),
`removed` (the stick was pulled while mounted), `unsupported`
(no FAT volume on it) or `failed` (see `detail`).
Why, for `failed`, `unsupported` and `removed`; empty otherwise.
The session user it belongs to: its files' owner.
The stick is not write protected.
The removable volumes of the caller (all of them for root), oldest
plug first. An ended volume stays listed until its stick is pulled,
so its outcome can be read.

Returns `volumes`: `Array<VolumeInfo>` (array of map (`VolumeInfo`)).

```rhai
let result = sys::mount::volumes();
```

### `eject(disk, force)`

`Eject(disk: String, force: Bool) -> (state: String, detail: String)`

Eject the volume on `disk` (`usb<n>`): flush, close and unmount it,
then let the stick be pulled. Answers at once; the answer's `state`
is `ejecting` (poll `Volumes` for `ejected`), `ejected` (nothing was
mounted, or it already was), or `busy` with the holders in `detail`:
one line per program and open file or working directory. `force`
ejects anyway: open files then fail with an I/O error. Failures of the
request itself are `ENOENT` (no such disk), `EPERM` (another user's
volume), `EINVAL` (not a disk name) and `EAGAIN` (still starting:
ask again in a moment).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `disk` | `String` | string |
| `force` | `Bool` | bool |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `state` | `String` | string |
| `detail` | `String` | string |

```rhai
let result = sys::mount::eject(disk, force);
```

## Types

### `MountInfo`

One mount the service knows about.

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `kind` | `String` | string |
| `host` | `String` | string |
| `port` | `U32` | int |
| `user` | `String` | string |
| `path` | `String` | string |
| `state` | `String` | string |
| `detail` | `String` | string |
| `owner` | `U32` | int |
| `share` | `String` | string |

```rhai
let value = sys::mount::new_mount_info();
```

### `VolumeInfo`

One removable volume the service knows about.

| Name | IDL type | Script value |
|---|---|---|
| `disk` | `String` | string |
| `name` | `String` | string |
| `path` | `String` | string |
| `state` | `String` | string |
| `detail` | `String` | string |
| `owner` | `U32` | int |
| `writable` | `Bool` | bool |

```rhai
let value = sys::mount::new_volume_info();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.mount.v1 on the wire](../../idl/os.lazy.mount.v1.md)
