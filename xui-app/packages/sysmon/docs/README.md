# System Monitor

System Monitor shows how busy your computer is: how hard the processor is
working, where the memory goes, which programs are busiest, and whether the
system's services are healthy. It updates every second.

Open it from the LazyOS menu (**System Monitor**). Press **F1**, or click
**Help** in the top-right corner, to open this guide.

## Overview

The first tab answers "is my computer busy, and why?"

### Processor

The large number is how much of the last second the processor spent working.
A quiet desktop sits near **0%**. A program that is computing something
(building, decoding a video, running a game) pushes it up. If it stays near
**100%** for a long time, something is keeping the processor busy all the
time; the list of busiest programs below says what.

The graph shows the last minute, newest on the right. Each horizontal line is
a quarter: 25%, 50% and 75%.

Under the number you see how many programs there are, how many of them are
running (working right now rather than waiting), and how long the computer has
been on since it last started.

### Memory

The large number is the memory programs and the system are using, and the
share of all memory that is. The coloured bar splits all of the memory into
four parts:

| Colour | Part | What it is |
| --- | --- | --- |
| Blue | **Programs** | Memory your apps and the background services are using: their code, their data and the pictures of their windows. Closing an app gives its memory back. |
| Purple | **System** | Memory LazyOS keeps for itself: the kernel, which runs every program and talks to the hardware, and its bookkeeping. |
| Amber | **Disk cache** | Copies of files read or written recently, kept so opening them again is fast. This memory is not lost: it is handed back as soon as a program needs it. |
| Empty | **Free** | Memory nothing is using right now. |

"Available to programs" is free memory plus the disk cache, because the cache
gives way whenever a program asks for more. A full-looking bar with a large
amber part is normal and healthy. Worry only when **Programs** and **System**
together approach the whole bar: then the computer is short of memory, and
closing an app that is not needed helps.

### Busiest programs

Every running program, busiest first, with its share of the processor in the
last second. **Running** means it is working or ready to work; **Waiting**
means it is idle until something happens (a key press, a timer, data from the
network or the disk). Most programs wait almost all of the time.

## Services

Services are the background programs LazyOS starts for you: the display, the
settings store, sound, networking, the package manager and more. The
supervisor (`init`) starts them, restarts them if they stop, and a health
monitor (`healthd`) checks that each one still answers.

The heading counts how many are **ok**, **degraded** (running but not well)
and **down** (stopped). Each row shows a service's state, its process number,
how many times it had to be restarted, its health, the services it depends on
and any detail it reported. A service that keeps restarting is worth a look in
the system log.

If the tab cannot read the list, a line above the table says why: the
supervisor or the health monitor is missing, or refused the request.

## Advanced

The last tab is for developers who work on LazyOS itself. It shows the
kernel's own counters:

- **Frames**: physical memory, counted in 4 KiB pages ("frames"): how many
  are in use, free and reserved for the allocator's bookkeeping, how many the
  block (disk) cache holds and how many the object slabs hold, and double or
  invalid frees, which should always be zero.
- **Slab**: the kernel's small-object allocator: bytes in use, the peak, and
  requests too big for a slab that went to the heap instead.
- **Kernel heap**: the kernel's general memory pool: bytes used out of what it
  has mapped, and the running count of page allocations and frees.
- **Tasks**: every scheduler slot in use, with its process id, scheduler
  state (`run`, `block`), scheduling class (`norm`, `intr`, `bg`, `rt`), the
  processor time charged to it in 100 Hz ticks, its name and its parent.

The Overview's memory parts come from the same numbers: **Disk cache** is the
block cache's frames, **System** is the kernel heap's pages, the slab frames
and the reserved frames, and **Programs** is every other page in use.

## Keys

| Key | Does |
| --- | --- |
| `o` | Show Overview |
| `s` | Show Services |
| `a` | Show Advanced |
| `r` | Update now |
| `c` | Switch to the compact view (three meters) and back |
| `F1` | Open this guide |
| `q` | Quit |

The tables scroll with the arrow keys, Page Up/Down and the mouse wheel.
