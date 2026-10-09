# `os.lazy.mount.v1`

Interface id: `0x9d456629506ac305`

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

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Mount | 1041399898 | sync | `(name: String, host: String, port: U32, user: String, password: String, kind: String, share: String) -> (path: String)` |
| Unmount | 2047171115 | sync | `(name: String) -> ()` |
| List | 220805025 | sync | `() -> (mounts: Array<MountInfo>)` |

## struct `MountInfo`

- `name: String`
- `kind: String`
- `host: String`
- `port: U32`
- `user: String`
- `path: String`
- `state: String`
- `detail: String`
- `owner: U32`
- `share: String`
