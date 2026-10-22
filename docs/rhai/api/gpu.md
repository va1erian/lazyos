# `sys::gpu`

The Rhai module for the Messenger interface `os.lazy.gpu.v1`. Wire-level reference: [os.lazy.gpu.v1](../../idl/os.lazy.gpu.v1.md).

The Intel GPU service (`gpud`, docs/kabylake-gpu-plan.md, stage G1:
"Adopt + observe").

**Read-only.** In this stage `gpud` claims the HD Graphics 620 of the
Kaby Lake box (PCI `8086:5916`, Gen9) or the UHD Graphics of the Alder
Lake box (`8086:46b3`, display 13), maps its register BAR and only *reads*
and decodes what the firmware left on the display: the pipe, transcoder
and DDI that are running, the link mode, the timing and pixel clock, the
core display clock, the first plane's surface and the GGTT pages behind
it. It writes no display register, programs no mode and takes no
interrupt, so nothing in this interface can change the hardware. It exists
to learn what the firmware leaves (plan section 2.1) and to give later
stages (HDMI audio, flips, decode) one place that already knows.

The state is a snapshot taken once when `gpud` starts; `GetBeam` is the
one live read (the frame counter and scan line); for any other live
register use `dbgd`'s `mmio.read`. Failures of a call are returned as a
structured error field (errno-style code, friendly text), not as a typed
reply. The interface is open to every caller: it reveals the display
mode and where its framebuffer is, nothing a program could not see by
asking the compositor. Method ordinals are append-only.

## Functions

### `get_state()`

`GetState() -> (state: State)`

A running pipe was found and decoded; the pipe fields below
mean nothing otherwise.
PCI device id and revision of the GPU.
Core display clock in kHz.
Bit `n`: pipe `n` (A = 0) is enabled.
The pipe (and transcoder) adopted, 0 = A.
The DDI port it drives (0 = A, 1 = B, ...; Gen9 has A to E,
display 13 A, B and the Type-C ports TC1 to TC4 as 3 to 6; a
register that names another value is passed on as read),
4294967295 when the registers name none.
A `LinkMode` ordinal.
Bits per colour component, 0 when not a defined value.
Pixel clock in kHz and refresh rate in millihertz, 0 when the
PLL is not in a mode `gpud` can compute them from.
The link is in HDMI mode, so HDMI audio (stage G2) needs no
mode set.
The state `gpud` adopted when it started.

Returns `state`: `State` (map (`State`)).

```rhai
let result = sys::gpu::get_state();
```

### `get_beam()`

`GetBeam() -> (beam: Beam)`

The adopted pipe's beam, read now. `ENODEV` when no pipe was
adopted.

Returns `beam`: `Beam` (map (`Beam`)).

```rhai
let result = sys::gpu::get_beam();
```

## Types

### `Axis`

One pipe's timing, in pixels (horizontal) or lines (vertical).

| Name | IDL type | Script value |
|---|---|---|
| `active` | `U32` | int |
| `total` | `U32` | int |
| `blank_start` | `U32` | int |
| `blank_end` | `U32` | int |
| `sync_start` | `U32` | int |
| `sync_end` | `U32` | int |

```rhai
let value = sys::gpu::new_axis();
```

### `Plane`

The first universal plane of the adopted pipe.

| Name | IDL type | Script value |
|---|---|---|
| `enabled` | `Bool` | bool |
| `format` | `U32` | int |
| `tiling` | `U32` | int |
| `stride_bytes` | `U32` | int |
| `width` | `U32` | int |
| `height` | `U32` | int |
| `surface` | `U32` | int |
| `surface_live` | `U32` | int |

```rhai
let value = sys::gpu::new_plane();
```

### `FbMap`

`PLANE_CTL` format code (4 is 32-bit XRGB).
A `Tiling` ordinal.
Row pitch in bytes.
The surface being programmed and the one being scanned out, as
GGTT offsets.
The framebuffer's pages as the GGTT maps them.

| Name | IDL type | Script value |
|---|---|---|
| `pages` | `U32` | int |
| `present` | `U32` | int |
| `phys_base` | `U64` | int |
| `contiguous` | `Bool` | bool |

```rhai
let value = sys::gpu::new_fb_map();
```

### `Beam`

Where the adopted pipe's beam is now, read live (read-only: the
frame counter and scan line count whether or not anybody asked for
interrupts).

| Name | IDL type | Script value |
|---|---|---|
| `frame` | `U32` | int |
| `line` | `U32` | int |
| `vtotal` | `U32` | int |
| `at_ns` | `U64` | int |
| `measured_millihz` | `U32` | int |
| `underrun` | `U32` | int |

```rhai
let value = sys::gpu::new_beam();
```

### `State`

Frames since the pipe was enabled (wraps at 2^32).
Scan line, 0 at the top of the active area.
Lines per frame, active and blank (the transcoder's total).
Monotonic time of the read, nanoseconds since boot.
The refresh rate `gpud` measured from the counter when it
started, millihertz; 0 when it could not, or when the
measurement disagreed with the timing registers.
An `Underrun` ordinal.
The adopted display state.

| Name | IDL type | Script value |
|---|---|---|
| `adopted` | `Bool` | bool |
| `device` | `U32` | int |
| `revision` | `U32` | int |
| `cdclk_khz` | `U32` | int |
| `power_well_1` | `Bool` | bool |
| `power_well_2` | `Bool` | bool |
| `pipes_enabled` | `U32` | int |
| `pipe` | `U32` | int |
| `ddi_port` | `U32` | int |
| `link` | `U32` | int |
| `bits_per_component` | `U32` | int |
| `hsync_positive` | `Bool` | bool |
| `vsync_positive` | `Bool` | bool |
| `pixel_clock_khz` | `U32` | int |
| `refresh_millihz` | `U32` | int |
| `horizontal` | `Axis` | map (`Axis`) |
| `vertical` | `Axis` | map (`Axis`) |
| `plane` | `Plane` | map (`Plane`) |
| `framebuffer` | `FbMap` | map (`FbMap`) |
| `audio_capable` | `Bool` | bool |

```rhai
let value = sys::gpu::new_state();
```

### `LinkMode`

A string; the constants of this module are:

- `LINK_MODE_NONE` = `"None"`
- `LINK_MODE_HDMI` = `"Hdmi"`
- `LINK_MODE_DVI` = `"Dvi"`
- `LINK_MODE_DP_SST` = `"DpSst"`
- `LINK_MODE_DP_MST` = `"DpMst"`
- `LINK_MODE_OTHER` = `"Other"`

`LINK_MODE` is the array of all of them.

### `Tiling`

A string; the constants of this module are:

- `TILING_LINEAR` = `"Linear"`
- `TILING_X` = `"X"`
- `TILING_Y` = `"Y"`
- `TILING_YF` = `"Yf"`
- `TILING_OTHER` = `"Other"`

`TILING` is the array of all of them.

### `Underrun`

A string; the constants of this module are:

- `UNDERRUN_NONE` = `"None"`
- `UNDERRUN_LATCHED` = `"Latched"`
- `UNDERRUN_UNOBSERVED` = `"Unobserved"`

`UNDERRUN` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.gpu.v1 on the wire](../../idl/os.lazy.gpu.v1.md)
