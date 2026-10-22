# LazyOS documentation

Start here. The guides below are written to be read in the Docs app: follow a
link to open that page, use **Back** (`Alt+Left`) and **Forward** (`Alt+Right`)
to return, and **Home** to come back to this index.

## Build your own apps

LazyOS apps are small: a LazyRAD form and a Rhai script, packaged as an
installable app. You rarely need to write Rust.

| Guide | What it covers |
|---|---|
| [LazyRAD](lazyrad/index.md) | The visual form designer and its player: forms, controls, scripts, packaging |
| [LazyRAD cookbook](lazyrad/cookbook.md) | Worked recipes: forms, drawing, files, timers, Messenger, sound |
| [Controls reference](lazyrad/controls.md) | Every control with its properties, events and methods |
| [Scripting reference](lazyrad/scripting.md) | How a form script runs; the [standard library](lazyrad/stdlib.md) and the [LazyOS-only modules](lazyrad/lazyos.md) |
| [Packaging](lazyrad/packaging.md) | `.lzp` packages, Make LazyOS App, permissions, where data lives |

## Script the system

| Guide | What it covers |
|---|---|
| [Rhai on LazyOS](rhai/index.md) | Where Rhai is used and a map of the pages below |
| [The Rhai language](rhai/language.md) | A compact tour of the language as this build runs it |
| [The `rhai` command](rhai/command.md) | The REPL, scripts, options, the `os` module |
| [Rhai cookbook](rhai/cookbook.md) | Recipes for configuration, notifications, files, topics and services |
| [msg: Messenger from Rhai](rhai/msg.md) | Calling services, subscribing to topics, serving methods |
| [Messenger API reference](rhai/api/index.md) | One page per `sys::` module, generated from the interface files |

## Messenger and the platform

| Guide | What it covers |
|---|---|
| [Messenger](messenger.md) | The RPC and publish/subscribe fabric every service speaks |
| [MIDL](midl.md) | The interface language and how clients and servers are generated |
| [Topics catalog](topics-catalog.md) | The topics in use |
| [Packages](packages.md) | The `.lzp` format, manifests, permissions, core packages |
| [Security model](security-model.md) | Accounts, labels, capabilities and the trusted prompt |
| [Platform plan](platform-plan.md) | The architecture and the staged roadmap |

## Using LazyOS

| Guide | What it covers |
|---|---|
| [README](README.md) | What LazyOS is and how to build and boot it |
| [LazyOS on a USB stick](usb-stick.md) | Booting real hardware |
| [Installing beside Linux](installing-beside-linux.md) | Adding LazyOS to a GRUB menu |
| [Build switches](build-switches.md) | The `LAZYOS_*` options that choose what an image contains |
| [Terminal](terminal-plan.md) | The desktop terminal |
| [Notifications](notify-plan.md) | Toasts from services and apps |

## Applications

[LazyWriter](xui-writer.md), [Archiver](xui-archiver.md), [Paint](paint-plan.md),
[PDF Viewer](pdf-reader-plan.md), [Video Player](video-player-plan.md),
[NES Player](nes-player.md), [Mail](mail.md), [LazyWeb+](lazywebplus.md), [LazySky](lazysky.md) and this viewer,
[Docs](xui-docs.md).

## How the system works

| Area | Pages |
|---|---|
| Kernel | [Boot](architecture/boot.md), [tasks](architecture/tasks.md), [memory](architecture/virtual-memory.md), [syscall gates](architecture/arch.md), [limits](architecture/limits.md) |
| Messenger | [Core](architecture/ipc-core.md), [fabric](architecture/ipc-fabric.md), [security](architecture/ipc-security.md) |
| Storage | [Filesystem](architecture/filesystem.md), [block cache](architecture/block-cache.md), [journal](architecture/journal.md), [USB storage](architecture/usb-storage.md) |
| Devices | [Devices](architecture/devices.md), [drivers](architecture/drivers.md), [USB](architecture/usb.md), [audio](architecture/audio.md), [networking](architecture/networking.md), [Wi-Fi](architecture/wifi.md) |
| Userland | [Userland](architecture/userland.md), [processes](architecture/processes.md), [display](architecture/display.md), [sandboxing](architecture/process-sandbox.md) |
| Overview | [Architecture reference](architecture.md) |

The wire-level reference for each Messenger interface (method ids, field types,
topics) is generated from the interface files and sits next to these pages; the
[Messenger API reference](rhai/api/index.md) links to the page of every
interface.

Guidance for AI agents working on the code (per-subsystem build, run and
verification recipes) is in the [agent guides](agents/index.md).
