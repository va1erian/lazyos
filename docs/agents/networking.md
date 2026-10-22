# Networking, TLS, SMB and Wi-Fi

Companion to [`AGENTS.md`](../../AGENTS.md); Https clients, networking in a boot, network drives, smb, network time, wi-fi and the network tooling.

## HTTPS clients (`LAZYOS_TLS=1`: `curl`, `wget`, `fetch`)

`nettls/` (a standalone workspace, like `rhai-host/`) builds one static-musl
program that runs as `fetch`, `curl` or `wget` by its name: rustls with
certificates verified against `/etc/ssl/certs/ca-certificates.crt` (the
Mozilla roots, written by the image build), `ureq` for HTTP/1.1, and an
in-tree pure-Rust crypto provider (`nettls/crypto`, MIT). Every linked crate
must have a GPLv2-compatible licence (a NetSurf port will link this stack):
`python tools/nettls/licenses.py` enforces it, so never add `ring`, `aws-lc`
or an Apache-2.0-only crate. `-k`/`--no-check-certificate` do not exist. The
plan and its decisions are [`docs/tls-plan.md`](../tls-plan.md).

```bash
python tools/run_demo.py --tls            # networking + curl/wget/fetch (then: curl https://...)
python tools/nettls/build.py --require    # target/nettls/fetch.elf
cargo test --manifest-path nettls/Cargo.toml --workspace
python tools/nettls/test_host.py          # the host binary against Python ssl servers
python tools/nettls/licenses.py           # GPLv2-compatible dependency tree
python tools/net/tls_run.py               # build, boot, run every check against the harness servers, judge
python tools/net/test_tls_pcap.py         # the wire judge fails when it should
python tools/net/tls_run.py --live        # real sites with the Mozilla roots only (manual, needs internet)
```

`tls_run.py` (also `tools/net/run.py --tls`) builds a test image with a
throwaway CA appended to the bundle (`LAZYOS_TLS_TEST_CA`) and `tls.test`
mapped to the host (`LAZYOS_TLS_TEST_HOSTS`); never set those in a normal
image. Behind an egress proxy that re-signs TLS, `--live --extra-ca PEM`
trusts that proxy's CA too.

## Networking in an interactive boot

`python tools/run_demo.py --net` (the launcher: *Networking* on the Simple
tab, the *Networking* group on the Advanced tab) builds the stack
(`LAZYOS_NETD=1`, with `LAZYOS_NETD_ARGS=demo=0` so `netd` runs without the
harness's evidence clients) and attaches a virtio-net card on QEMU's user
network, forwarding host `127.0.0.1:8080` to the guest (`--net-forward`,
`--net-restrict`, `--net-pcap`, and `--nics N` for N cards, each on its own
user network and named `eth0`, `eth1`, ... by `devd`; the same flags on
`qemu_session.py` and `qemu_shot.py`, all from `tools/net/qemu_net.py`). A
`--net` desktop ships two core packages: **Network**
(`xui-app/src/bin/network.rs`: status, DHCP or a manual address written to
`confd`'s `sys/net/<if>/*` for the card shown, *Next card* to step through
several) and **Net Tools**
(`nettools.rs`: ping, lookups, an HTTP fetch and a web server on 8080), sharing
`xui-app/src/net/`. How to reach the guest from the host:
[`docs/networking-host-access.md`](../networking-host-access.md).

```bash
python tools/run_demo.py --desktop --net        # then open Net Tools, and http://localhost:8080 on the host
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --net --out shots/net_apps     --script tools/screenshot/examples/net_apps.json      # ping, lookup, a fetch through the host forward
python tools/screenshot/qemu_session.py --image target/lazyos.img --net --out shots/net_config     --script tools/screenshot/examples/net_config.json    # Manual, back to DHCP, Renew
python tools/screenshot/qemu_session.py --image target/lazyos.img --net --nics 2 --out shots/net_cards     --script tools/screenshot/examples/net_cards.json     # two cards: the Network app on eth0, "Next card", eth1
python tools/net/test_qemu_net.py                         # the QEMU argument helper
```

## Network Drives (FTP and SMB mounts from the desktop)

`ftpfuse` serves an FTP server, and `smbfuse` an SMB share, at
`/mnt/<name>` through the FUSE mechanism (syscall 35, `CAP_FS_PROVIDER` and
a provider uid: root or `_mountd`, `mounttable::FS_PROVIDER_UIDS`). Apps
hold no capabilities, so the desktop asks `mountd` (`os.lazy.mount.v1`,
`idl/mount.midl`; uid 910 with only `CAP_FS_PROVIDER`), which starts one
daemon per mount (`kind` `ftp` or `smb`) with the caller's ownership
(`owner=`) and reports `connecting`/`mounted`/`failed`. The rules are
`libs/mounttable` (host-tested); the front end is the core package **Network
Drives** (`os.lazy.netdrives`, `xui-app/src/bin/netdrives.rs`), shipped in
every `--net` desktop. See [`docs/smb-plan.md`](../smb-plan.md) §3.4.

```bash
python tools/run_demo.py --desktop --net  # then Internet -> Network Drives, server 10.0.2.2
cargo test -p mounttable
python tools/fuse/ui_run.py               # build, mount from the app against a host FTP server, judge
python tools/fuse/ui_run.py --smb         # the same against the host SMB harness server
python tools/fuse/test_ui_judge.py        # the judge fails when it should
```

## SMB shares as directories (`smbfuse`, docs/smb-plan.md F3)

`smbfuse -U USER //SERVER/SHARE [name=NAME]` mounts an SMB 2.1 share at
`/mnt/<name>` (default: the share's name in lower case) in every
`LAZYOS_NETD=1` image: it prompts for the password without echo, starts
itself again in the background with the password in that copy's
environment (never `argv`), and returns once the mount answers (`-f`
serves in the foreground; `mountd` passes `LAZYOS_SMB_PASSWORD`). The FUSE
mapping is `libs/smbfs` (host-tested against `smbwire`'s in-memory server,
which the `testserver` feature exposes). FUSE metadata is believed for 1 s
in the VFS (`Filesystem::cache_deadline`, `fuse::ATTR_TICKS`), so a change
another client makes shows within that. Running `smbfuse` directly needs
a provider identity (root, as in a console image's shell, or `_mountd`): a
session user's shell has no `CAP_FS_PROVIDER` and mounts through `mountd`
(Network Drives, or `sys::mount` from `rhai`).

```bash
cargo test -p smbfs -p mounttable -p fused
LAZYOS_TEST_FILTER=fuse python tools/test/run.py --accel none   # incl. the cache lifetime (coherence.rs)
python tools/smb/fuse_run.py              # build, mount two shares, use them with BusyBox, judge servers + pcap
python tools/smb/test_fuse_judge.py
```

## SMB client (`LAZYOS_SMB=1`: `smb`)

`smb` (`user/src/bin/smb.rs`) is an SMB 2.1 client over the socket service:
NTLMv2 inside SPNEGO, HMAC-SHA256 signing, from `libs/smbwire` (pure
`no_std`, host-tested and fuzzed; [`docs/smb-plan.md`](../smb-plan.md) F2,
§4.6). The password comes from `LAZYOS_SMB_PASSWORD` or a prompt that does
not echo, never an argument. From a shell, quote the command list:
`smb -U chaton //10.0.2.2/share 'ls ; get hello.txt - ; put -g 1000 up.bin'`
(add `-p 1445` for the harness server). A native program run from the shell
must be on the kernel's list in `kernel/src/process/linux/native.rs`, or it is
loaded as a Linux program with no native heap.

```bash
python tools/run_demo.py --smb             # networking + smb
cargo test -p smbwire                      # MS-NLMP vectors, a fake server, refusals, seeded fuzz
python tools/smb/test_smbserver.py         # the harness server against the host client smbcat
python tools/smb/test_judge.py             # the judges fail when they should
python tools/smb/samba_interop.py          # the library against real Samba (Docker)
python tools/smb/run.py                    # build, boot, 12 checks against host servers, judge files + pcap
python tools/smb/licenses.py               # GPLv2-compatible crates only
```

`run.py` types passwords with the session step `{"type_secret": "VAR"}`
(the value of a host environment variable), so no password is in the script;
it scans the serial log, the capture and the session record for it.

## Network tooling

Networking (`docs/networking-plan.md`) is verified like audio: serial markers
only say when the guest is done, the verdict is what crossed the wire. Testing
and fuzzing tooling ships with every stage; the full description is
`tools/net/README.md`.

```bash
cargo test -p framering -p virtio-net -p nicdrv -p netstack -p ftpwire -p netpolicy -p virtio -p messenger-generated   # host unit + seeded fuzz
FUZZ_CASES=20000 cargo test -p framering -p virtio-net -p nicdrv -p netstack fuzz::   # a longer seeded soak
FUZZ_SEED=0x<seed> cargo test -p framering clean_scripts                # replay a printed failing seed
python fuzz/gen_corpus.py --check                                       # the checked-in fuzz seeds are current
python tools/net/test_analyze_pcap.py                                   # the capture judge fails when it should
python tools/net/test_sockets_pcap.py                                   # the TCP/UDP/DNS judge fails when it should
python tools/net/run.py                                                 # build (LAZYOS_NET=1), boot QEMU, judge the pcap
python tools/net/run.py --services | --poll | --no-device | --machine q35 --virtio-disk   # variants
python tools/net/run.py --nic e1000                                     # an Intel 8254x instead of virtio-net (issue #497)
python tools/net/run.py --netd                                          # stages N2+N3: netd, DHCP, ping, nslookup, nc and the socket probe/soak; judged from the pcap and the host echo servers (combines with the variants)
python tools/net/run.py --nics 2                                        # WP1: two cards on two user networks, one pcap each: DHCP on both, the wired-metric winner carries traffic and DNS, QMP set_link moves it to eth1 and back (DHCP starts over), a removed card is detached (tools/net/multi_run.py)
python tools/net/test_multi_judge.py                                    # the multi-NIC judge fails when it should
mkdir -p fuzz/corpus/netstack; cargo fuzz run netstack --fuzz-dir fuzz fuzz/corpus/netstack fuzz/seeds/netstack -- -max_total_time=60   # Linux
mkdir -p fuzz/corpus/framering                                          # once; libFuzzer's working corpus (git-ignored)
cargo fuzz run framering --fuzz-dir fuzz fuzz/corpus/framering fuzz/seeds/framering -- -max_total_time=60  # Linux; CI runs it
```

Fuzz entry points (`fuzz::run(&[u8])`) are shared by the in-tree seeded tests
and the `fuzz/` cargo-fuzz targets, so a libFuzzer crash replays under plain
`cargo test`; save fixed crashes in `fuzz/regressions/<target>/`. The `fuzz/`
crate is outside the OS workspace on purpose: local development never needs
libFuzzer, which is Linux-only in CI.

## Network time (`timed`, SNTP)

When the network has an address `timed` asks NTP servers for the time and
steps the wall clock; at start it lifts a clock earlier than the build's floor
(`sntp::CLOCK_FLOOR_UNIX`, a checked-in date: raise it when you cut a release).
It is on by default and switched with `sys/time/ntp/enabled` and
`sys/time/ntp/servers` (Settings -> Time & Date, through `elevd`; a session
cannot write `sys/**`). The first sync of a boot may step by any amount, later
ones by at most 1000 s (a larger offset only when answers keep agreeing for ten
minutes), never to before the floor, and offsets under 250 ms are left alone
(`libs/sntp/src/policy.rs`). Plain SNTP is unauthenticated: it is a
correctness aid (certificate validity, SMB/NTLM times), not a security
control. Plan and as-built notes: [`docs/ntp-plan.md`](../ntp-plan.md).

The packet, step policy and schedule are host-tested; the QEMU harness boots a
services console image against a misbehaving host server (`tools/ntp`) with a
good or years-off RTC and judges the guest's `TIMED:NTP:*` lines, the clock
`timectl now` prints and what the server received (nonce, request spacing).
`LAZYOS_TIMED_ARGS=ntp.server=10.0.2.2:PORT` is the harness switch (it also
forces the client on); `timectl ntp [status|sync]` is the command line.

```bash
cargo test -p sntp                       # packet, maths, policy, schedule, seeded fuzz
python fuzz/gen_corpus.py --check        # the sntp seeds are current
python tools/ntp/test_judge.py           # the harness judge fails when it should
python tools/ntp/run.py --accel none     # build, boot once per scenario, judge (--only NAME, --list)
LAZYOS_TEST_FILTER=timed python tools/test/run.py --accel none   # the clock syscall (centiseconds)
cargo test --manifest-path xui-app/Cargo.toml -p xui-settings    # the Time & Date page
```

## Wi-Fi (`LAZYOS_WIFI=1`, RTL8822CE)

`LAZYOS_WIFI=1` (needs `LAZYOS_NETD=1`; `run_demo.py --wifi`, the launcher's
*Wi-Fi* checkboxes) ships `wifid`, the RTL8822CE driver `devd` starts as
`wlan0` for `10ec:c822` (uid 911 `_wifi`, the kernel's `wireless` device
class only), and its firmware at `fhs::FIRMWARE`
(`/system/share/firmware/rtw88/`), downloaded and hash-checked by
`tools/firmware/fetch.py` from `tools/firmware/manifest.txt`, never
committed or `include_bytes!`'d; drivers read it through `libs/fwload`.
QEMU has no such card: the loop is a stick and `dbgd`
([`docs/wifi-rtw88-plan.md`](../wifi-rtw88-plan.md) section 5).

```bash
python tools/run_demo.py --desktop --wifi --dbgd-control --usb-image --build-only   # the Wi-Fi stick
python tools/dbg/dbgctl.py --host <ip> log --source programs --follow              # WIFID:*
python tools/dbg/dbgctl.py --host <ip> reload wifid                                # after a rebuild
cargo test -p fwload -p devmatch -p netpolicy
python tools/firmware/fetch.py --check && python tools/firmware/test_fetch.py
python tools/wifi/licenses.py           # Wi-Fi crates' licences, THIRD_PARTY.md rows
```

## Wi-Fi soft-MAC and simulator (`LAZYOS_WIFI_SIM=1`)

Both radio programs (`wifid` on the RTL8822CE, `wifisim` in CI) are a
`wifimac::Radio` under one soft-MAC (`libs/wifimac`: `hw.v1` semantics,
scanning, keys, 802.3/802.11 and CCMP packet numbers) served by
`user::wifi::run` as `os.lazy.net.wifi.hw/wlan0` and `os.lazy.net.nic/wlan0`
(kind wireless); see [`docs/architecture/wifi.md`](../architecture/wifi.md).
`wifisim` (uid 913) carries 802.11 in Ethernet (EtherType 0x88B5,
`libs/simair`) over a second virtio-net card the harness gives the MAC
`02:57:53:49:4d:00`: `netdrv` serves it as `air0`, which `netd` never attaches.
`tools/wifi/fake_ap.py` is the other end (open, WPA2, hidden, two-BSS
networks, an independent authenticator, DHCP, faults); `wifiprobe` drives
`hw.v1` when `wlanmd` is absent.

```bash
cargo test -p ieee80211 -p wifimac -p simair
python tools/wifi/sim_run.py              # build, boot, judge scan, joins, DHCP, ping, faults (wifiprobe)
python tools/wifi/sim_run.py --station wlanmd   # the same through wlanmd + wifictl as `user` (needs BusyBox)
python tools/wifi/test_judge.py; python tools/wifi/test_judge_wlanmd.py; python tools/wifi/test_fake_ap.py
python tools/run_demo.py --wifi-sim       # interactive; --wifi-probe open:LazyOpen joins without wlanmd
python tools/run_demo.py --desktop --wifi --wifi-sim   # Network app Wi-Fi tab, tray bars (needs the firmware: tools/firmware/fetch.py --switch wifi)
python tools/wifi/ui_run.py               # the Wi-Fi tab driven by widget name against fake_ap.py, judged
```
