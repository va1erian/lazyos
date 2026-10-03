# `os.lazy.display.v1`

Interface id: `0x5ef41f254d43c2b4`

The userspace compositor protocol (`xuid`; issues #113, #143, #145, #167,
#287).

One interface carries both directions. **Calls** (app or shell to
compositor) are ordinary request/reply methods: `CreateSurface`,
`AttachBuffer`, `Commit`, `DestroySurface`, `DragStart`, `DragCancel`,
`ListSurfaces`, `GetWorkArea`, `Subscribe`, `GetTheme`, `SetTitle`,
`HintOpenOrigin` and `SetSizeHints`. **Events** (compositor to app, or to
the shell subscriber) are `oneway` methods sent on the event endpoint the
client transferred: `PointerMove`, `PointerDown`, `PointerUp`,
`PointerWheel`, `KeyDown`, `KeyUp`, `WindowClose`, `Configure`, the
drag-and-drop set
`DragEnter`/`DragOver`/`DragLeave`/`Drop`/`DragEnded`, and the shell set
`SurfaceChanged`/`FocusChanged`/`StartMenu`. Method ids are pinned to the
values the hand-written protocol used (1-24), so the numbering stays
append-only from here on.

Endpoint and buffer transfers ride in the parcel's `handles` and `buffers`
vectors, where the kernel moves them; the TLV body has no `Handle`/`Buffer`
fields because a raw handle number in the body would be meaningless to the
receiver. `CreateSurface` and `Subscribe` transfer one event endpoint
(`handles[0]`), `AttachBuffer` shares one pixel buffer (`buffers[0]`, at
least `width * height * 4` bytes of RGBA8 for the surface's current
content size, rows tightly packed at that width).

Pointer coordinates in every event are relative to the surface content
origin; a move outside the surface (a press-and-drag) reports negative or
oversized values. Pointer events carry a button id (`1` left, `2` right,
`3` middle). Failures of calls are returned as a structured error field
(id 15, errno-style code plus friendly text) instead of the declared reply
fields, which never use that id.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| CreateSurface | 1 | sync | `(width: U32, height: U32, title: String, role: U32) -> (surface: U64) transfers (events: Channel<os.lazy.display.v1>)` |
| AttachBuffer | 2 | sync | `(surface: U64) -> () transfers (pixels: Buffer)` |
| Commit | 3 | sync | `(surface: U64, x: U32, y: U32, w: U32, h: U32) -> ()` |
| DestroySurface | 4 | sync | `(surface: U64) -> ()` |
| PointerMove | 5 | oneway | `(x: I32, y: I32) -> ()` |
| PointerDown | 6 | oneway | `(x: I32, y: I32, button: U32) -> ()` |
| PointerUp | 7 | oneway | `(x: I32, y: I32, button: U32) -> ()` |
| KeyDown | 8 | oneway | `(key: U32) -> ()` |
| KeyUp | 9 | oneway | `(key: U32) -> ()` |
| WindowClose | 10 | oneway | `() -> ()` |
| DragStart | 11 | sync | `(surface: U64, token: U64, mime: String) -> ()` |
| DragCancel | 12 | sync | `(surface: U64) -> ()` |
| DragEnter | 13 | oneway | `(x: I32, y: I32, mime: String) -> ()` |
| DragOver | 14 | oneway | `(x: I32, y: I32) -> ()` |
| DragLeave | 15 | oneway | `() -> ()` |
| Drop | 16 | oneway | `(x: I32, y: I32, token: U64, mime: String) -> ()` |
| DragEnded | 17 | oneway | `(dropped: Bool) -> ()` |
| ListSurfaces | 18 | sync | `() -> (surfaces: Array<SurfaceRow>)` |
| GetWorkArea | 19 | sync | `() -> (x: I32, y: I32, w: I32, h: I32)` |
| Subscribe | 20 | sync | `(subscriber_role: String) -> () transfers (events: Channel<os.lazy.display.v1>)` |
| GetTheme | 21 | sync | `() -> (title_bg_active: U32, title_bg_inactive: U32, border: U32, taskbar: U32, text: U32, mode: String, accent: U32)` |
| SurfaceChanged | 22 | oneway | `(surface: U64, kind: U32, x: I32, y: I32, w: I32, h: I32, minimized: Bool, focused: Bool, title: Option<String>, role: U32, maximized: Bool) -> ()` |
| FocusChanged | 23 | oneway | `(surface: Option<U64>) -> ()` |
| StartMenu | 24 | oneway | `() -> ()` |
| AttachBufferSlot | 25 | sync | `(surface: U64, slot: U32) -> () transfers (pixels: Buffer)` |
| Present | 26 | oneway | `(surface: U64, slot: U32, seq: U64, damage: Array<Rect>) -> ()` |
| BufferRelease | 27 | oneway | `(surface: U64, slot: U32) -> ()` |
| FrameDone | 28 | oneway | `(surface: U64, seq: U64) -> ()` |
| SetTitle | 29 | sync | `(surface: U64, title: String) -> ()` |
| HintOpenOrigin | 30 | sync | `(surface: U64, x: I32, y: I32, w: U32, h: U32) -> ()` |
| PointerWheel | 31 | oneway | `(x: I32, y: I32, delta: I32) -> ()` |
| SetSizeHints | 32 | sync | `(surface: U64, min_w: U32, min_h: U32, max_w: U32, max_h: U32) -> ()` |
| Configure | 33 | oneway | `(surface: U64, width: U32, height: U32, state: U32) -> ()` |
| RequestSize | 34 | sync | `(surface: U64, width: U32, height: U32) -> ()` |
| Ping | 35 | oneway | `() -> ()` |
| PlaceSurface | 36 | sync | `(surface: U64, x: I32, y: I32) -> ()` |
| ActivateSurface | 37 | sync | `(surface: U64) -> ()` |
| MinimizeSurface | 38 | sync | `(surface: U64) -> ()` |
| SetWorkArea | 39 | sync | `(x: I32, y: I32, w: I32, h: I32) -> ()` |
| SetIconGeometry | 40 | sync | `(surface: U64, x: I32, y: I32, w: I32, h: I32) -> ()` |
| HintLaunchOrigin | 41 | sync | `(x: I32, y: I32, w: U32, h: U32) -> ()` |
| Dismiss | 42 | oneway | `() -> ()` |
| GetOutput | 43 | sync | `() -> (width: U32, height: U32, scale: U32)` |

## Transfers

Objects a request carries outside its body, in the parcel's
`handles` and `buffers` vectors.

| Method | Name | Slot |
|---|---|---|
| CreateSurface | `events` | `handles[0]`, a channel the receiver sends `os.lazy.display.v1` on |
| AttachBuffer | `pixels` | `buffers[0]`, a shared buffer |
| Subscribe | `events` | `handles[0]`, a channel the receiver sends `os.lazy.display.v1` on |
| AttachBufferSlot | `pixels` | `buffers[0]`, a shared buffer |

## struct `Rect`

- `x: U32`
- `y: U32`
- `w: U32`
- `h: U32`

## struct `SurfaceRow`

- `id: U64`
- `title: String`
- `x: I32`
- `y: I32`
- `w: I32`
- `h: I32`
- `minimized: Bool`
- `focused: Bool`
- `role: U32`
- `maximized: Bool`

## enum `Role`

- Window, Desktop, Panel

## enum `Change`

- Unspecified, Created, Destroyed, Moved, Minimized, Restored, Title, Resized, Maximized, Unmaximized

## enum `WindowState`

- Normal, Maximized
