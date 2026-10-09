# SMB harness (`tools/smb/`)

Verification for the SMB 2.1 client of [docs/smb-plan.md](../../docs/smb-plan.md)
stage F2 (`libs/smbwire` and the guest command `smb`, `LAZYOS_SMB=1`) and of
stage F3, the share mounted as a directory (`libs/smbfs` and `smbfuse`, in
every `LAZYOS_NETD=1` image). As with
networking and TLS, serial markers only say *when*; the verdict is what the
servers recorded and what crossed the wire.

| File | Role |
|---|---|
| `run.py` | Build `LAZYOS_CLI=1 LAZYOS_NETD=1 LAZYOS_SMB=1`, start nine harness servers, boot headless with a capture, run the checks, judge |
| `checks.py` | The servers (one per behaviour), the twelve checks typed at the console, and the session script |
| `judge.py` | Markers per check, the main server's directory, every server's record, the leak scan |
| `smb_pcap.py` | The wire judge: secrets, dialect, share, signatures, uploads only in `WRITE`, downloads only in `READ` |
| `smbserver.py`, `smbfiles.py`, `smbproto.py`, `ntlm.py` | A standard-library SMB 2.1 server over a real directory (NTLMv2 with its own MD4, signing both ways, a request record, misbehaviour switches) |
| `samba_interop.py` | `libs/smbwire`'s host client `smbcat` against real Samba 4.19 in Docker |
| `licenses.py` | Every crate linked into `smbwire` has a GPLv2-compatible licence |
| `fuse_run.py` | F3: build `LAZYOS_CLI=1 LAZYOS_NETD=1`, mount two servers' shares with `smbfuse`, use them with BusyBox, judge the servers' directories, records, the capture and the leak scan |
| `fuse_checks.py` | F3's session (mounts and steps) and its judges (step statuses and output, the servers' trees) |
| `test_smbserver.py`, `test_judge.py`, `test_fuse_judge.py` | The server against `smbcat`; the judges fail when they should |

```bash
python tools/smb/run.py                 # build, boot, judge (shots/smb/)
python tools/smb/run.py --no-build      # reuse target/lazyos.img
python tools/smb/test_smbserver.py
python tools/smb/test_judge.py
python tools/smb/fuse_run.py            # F3: the share as a directory (shots/smbfuse/)
python tools/smb/test_fuse_judge.py
cargo test -p smbfs                     # the FUSE operations against the in-memory server
python tools/smb/samba_interop.py       # needs Docker
python tools/smb/smbserver.py DIR --port 1445 --password PW   # a server to try `smb` by hand
cargo run -p smbwire --example smbcat -- 127.0.0.1:1445 share chaton selftest   # LAZYOS_SMB_PASSWORD=PW
```

## The checks

Each check is one `smb` run against one server; the password is typed at
`smb`'s prompt with the session step `{"type_secret": "LAZYOS_SMB_PASSWORD"}`,
so no password is in `session.json`. Four must succeed:

* `transfer` (plain server): `ls`, `get` to stdout and checksummed, a 300 KB
  `put -g`, a `put` of a file the shell wrote, `mkdir`, `mv`, `rm`, `rmdir`,
  `cd`, `df`. The server's directory afterwards must hold exactly those bytes.
* `signed` (server requires signing), `sign` (`--sign` against a server that
  does not require it), `raw` (raw NTLMSSP, no server timestamp, so the client
  clock and LMv2 are used): each server must have verified every request's
  signature where signing is on.

Eight must be refused for their reason, and their servers must see no file
operation: `wrongpw` (`LOGON_FAILURE`), `noshare` (`BAD_NETWORK_NAME`),
`nosign` (`--no-sign` against a signing server), `guest`, `encrypt`,
`truncated` (a cut NTLM challenge), `tamper` (a bad signature on `READ`), and
`smb3` (a server that speaks only 3.1.1).

`LAZYOS_SMB_USER` and `LAZYOS_SMB_PASSWORD` (letters and digits) choose the
account; otherwise a random password is made for the run. The serial log,
the capture and the session files are scanned for it, raw and in UTF-16LE.
