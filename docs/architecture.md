# LazyOS architecture reference

Terse per-element reference for the code as committed on `main`. Each page covers
what an element is, its key files, its interfaces, its invariants and its current
status. Design rationale is not repeated here; the plan docs own it:

- [Platform plan](platform-plan.md) - roadmap stages S0-S9 (the `Stage` column below).
- [Messenger specification](messenger.md) - wire format, handles, topics, policy.
- [Security model](security-model.md) - trust rules, sandbox profiles, secrets.
- [Accounts plan](accounts-plan.md) - user accounts, login, `elevd` and the
  trusted prompt (U0-U2 landed, U3-U5 next).
- [Linux ABI plan](linux-abi-plan.md) - compatibility bridge.
- [XUI plan](xui-plan.md) - userspace toolkit target.
- [Shell plan](shell-plan.md) - S5 desktop shell (LazyShell) on XUI.
- [rust-std](rust-std.md), [Dyon feasibility](dyon-feasibility.md),
  [Wi-Fi exploration](wifi-plan.md) and its [prerequisites](wifi-prerequisites-plan.md), [printing exploration](printing-plan.md) -
  supporting notes.

Path references are relative to the repository root. `path:line` references are
used only where the line is a stable anchor.

| Element | Page | Key paths | Issue / stage |
|---|---|---|---|
| Boot & image build | [boot.md](architecture/boot.md) | `build.rs`, `src/main.rs`, `.cargo/config.toml` | #62, S0 |
| Arch, CPU tables & syscall gates | [arch.md](architecture/arch.md) | `kernel/src/arch/{gdt,idt,msr,pic,cpu,linux}.rs` | #69, S0 |
| Physical memory & paging | [physical-memory.md](architecture/physical-memory.md) | `kernel/src/mem/mod.rs` | #54, #55, S0 |
| Virtual memory (VMA/COW/mmap) | [virtual-memory.md](architecture/virtual-memory.md) | `kernel/src/mem/vma.rs`, `kernel/src/process/linux/mem.rs` | #55, S0 |
| Resource limits (derived defaults, `lazyos.cfg` keys) | [limits.md](architecture/limits.md) | `kernel/src/limits.rs`, `build_support/limits_cfg.rs` | real-PC limits |
| Allocators (heap/slab/user) | [allocators.md](architecture/allocators.md) | `kernel/src/mem/{heap,slab}.rs`, `user/src/heap.rs` | #61, S0 |
| Tasks & scheduler | [tasks.md](architecture/tasks.md) | `kernel/src/task/mod.rs`, `kernel/src/task/switch.rs` | #58, S0 |
| Wait queues & signals | [wait-signals.md](architecture/wait-signals.md) | `kernel/src/task/{wait,signal}.rs` | #57, #60, S0 |
| Processes, supervision & Linux ABI | [processes.md](architecture/processes.md) | `kernel/src/task/process.rs`, `kernel/src/process/{mod,gate}.rs`, `kernel/src/process/linux/` | #59, #93, #101, S0/S2 |
| Messenger core (handles/channels/buffers) | [ipc-core.md](architecture/ipc-core.md) | `kernel/src/ipc/{handles,channels,shared}.rs`, `libs/messenger` | #64-#67, S1 |
| Messenger security (creds/ACL/audit/quota) | [ipc-security.md](architecture/ipc-security.md) | `kernel/src/ipc/{credentials,acl,audit}.rs`, `kernel/src/quota.rs` | #68, #103, S1/S3 |
| Messenger fabric (registry/topics/stats/syscalls) | [ipc-fabric.md](architecture/ipc-fabric.md) | `kernel/src/ipc/{registry,topics,stats,syscalls}.rs` | #69, #70, #89, #92, S1/S2 |
| Filesystem (VFS/ramfs/FAT/ext2, ext2 OS volume at `/`) | [filesystem.md](architecture/filesystem.md) | `kernel/src/fs/{mod,vfs,ramfs,ext2,mounts}.rs`, `kernel/src/fs/fat/`, `libs/ext2fs`, `libs/fhs` | #98, #99, S3, [filesystem plan](filesystem-plan.md) F0-F5 |
| Block devices (ATA/virtio) | [block-devices.md](architecture/block-devices.md) | `kernel/src/block/{mod,ata,virtio,stats}.rs` | #100, S3 |
| Block cache (ext2 write-back, crash ordering, periodic flusher) | [block-cache.md](architecture/block-cache.md) | `libs/ext2fs/src/{cache/,commit.rs}`, `kernel/src/fs/{ext2/cache,flusher}.rs` | filesystem plan section 5 |
| Device core (enumeration/resources/drivers) | [devices.md](architecture/devices.md) | `kernel/src/dev/{mod,pci,bus,table,driver,resources}.rs` | #239, D1 |
| Interrupt routing (I/O APIC, MSI/MSI-X) | [interrupts.md](architecture/interrupts.md) | `kernel/src/arch/{irqchip,ioapic,msi_stubs}.rs`, `kernel/src/dev/{msi,msi_hw}.rs` | #616, K1 |
| Audio (virtio-sound driver, `audiod` mixer, `os.lazy.audio.v1`) | [audio.md](architecture/audio.md) | `libs/{virtio,virtio-snd,pcm,audiomix,audioclient}`, `user/src/bin/{sndd,audiod,beep,mixer}*`, `idl/audio*.midl`, `tools/sound/` | D5/D6, [audio-plan](audio-plan.md) |
| Networking (NIC interface, `netdrv`, `netd` on smoltcp, sockets/DNS, `nc`/`nslookup`/`ftp`, Linux `AF_INET`) | [networking.md](architecture/networking.md) | `libs/{framering,virtio-net,nicdrv,netstack,ftpwire,netpolicy}`, `user/src/bin/{netdrv,netd}*`, `kernel/src/ipc/inet/`, `idl/net.midl`, `tools/net/` | N0-N5, S6, [networking plan](networking-plan.md) |
| USB HID (`usbd`, xHCI keyboard/mouse/tablet) | [usb.md](architecture/usb.md) | `libs/{xhci,usbhid,usbpolicy}`, `user/src/bin/usbd*`, `kernel/src/input/sources.rs`, `tools/usb/` | [usb-hid plan](usb-hid-plan.md) U0-U5 |
| Display, input & mux | [display.md](architecture/display.md) | `kernel/src/{display,mux,console,gfx,surface,text,cursor}.rs`, `user/src/bin/{xuid,xdemo,dragdemo,shellprobe}.rs`, `xui-app/` | #113, #114, #143, #145, #167, #168, S4/S5 |
| Userland runtime, shared libs & services | [userland.md](architecture/userland.md) | `user/src/*`, `libs/*`, `user/src/bin/*` | #69, #90-#93, #101-#116, S1-S3 |
| Build, tools & CI | [build-tools.md](architecture/build-tools.md) | `tools/*`, `.github/workflows/*` | #62, #90, #124, S9 |

## Cross-cutting invariants

- Single CPU, no SMP; many registries are indexed by task slot (0 = kernel task).
- Kernel and user programs are `no_std`; shared crates (`libs/messenger`,
  `libs/crypto`) build for both `x86_64-unknown-none` and the host.
- Every userspace-reachable operation returns negative errno-style codes
  (x86_64 Linux numbering) and stores structured errors in parcels.
- Security decisions flow through `ipc::authorize` (`kernel/src/ipc/mod.rs`):
  kernel-stamped credentials, default-deny ACL, audit ring.
- Interaction with the OS is expected through Messenger; the Linux syscall shim
  is a compatibility guest, not the native ABI.
