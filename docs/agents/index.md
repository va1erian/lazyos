# Agent guides

Subsystem-specific guidance for AI agents, split out of [`AGENTS.md`](../../AGENTS.md). `AGENTS.md` has the core values, performance rules, verification workflow and CI conventions; open the file below that matches the area you are changing.

- [Build, run and the disk image](build-and-run.md): Running the demo, software suites, login and elevation, the OS volume layout, and the documentation site.
- [Tracing in full](tracing.md): The `trace` API, channels, masks and the ad hoc print lint.
- [Kernel, filesystem and conformance](kernel.md): Resource limits, the kernel test harness, ext2 and its journal, the Linux ABI bench and real Linux programs.
- [System services, packages and security](services.md): Developer tools and the Playground, Rhai, packages and the label trace, app failures, notifications, tray, accounts attacks, keyd secrets, shutdown.
- [Desktop apps](apps.md): Terminal, Docs, Writer, Archiver, PDF, Video, Paint, About, Picture Viewer, the MOD player, Mail and LazySky.
- [Games](games.md): LazyGolf, the NES Player, Doom, emusic, Quake and SRB2Kart.
- [WebKit, webd and the browser](web.md): JavaScriptCore, the WebKit dependency bundle and port, LazyWeb and webd.
- [Networking, TLS, SMB and Wi-Fi](networking.md): HTTPS clients, networking in a boot, network drives, SMB, network time, Wi-Fi and the network tooling.
- [Display, drivers and hardware](hardware.md): HiDPI, the Intel GPU service, devd, interrupt routing, USB, sound, FAT sticks and installing beside Linux.
- [LazyVM (Java 8 bytecode VM)](lazyvm.md): The LazyOS side of LazyVM: build switch, test hook, display grant, `midlc` Java backend, packaging, and the clean-room rules (no JDK or other class-library source may be read or copied) that bind all LazyVM work.
