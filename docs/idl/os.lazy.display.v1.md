# `os.lazy.display.v1`

Interface id: `0x5ef41f254d43c2b4`

The userspace compositor protocol (`xuid`; issues #113, #143, #145, #167,
#287).

One interface carries both directions. **Calls** (app or shell to
compositor) are ordinary request/reply methods: `CreateSurface`,
`AttachBuffer`, `Commit`, `DestroySurface`, `DragStart`, `DragCancel`,
`ListSurfaces`, `GetWorkArea`, `Subscribe`, `GetTheme`, `SetTitle`,
`HintOpenOrigin` and `SetSizeHints`. **Events** (compositor to app, or to
the shell subscriber) are `event` methods sent on the event endpoint the
client transferred: `PointerMove`, `PointerDown`, `PointerUp`,
`PointerWheel`, `KeyDown`, `KeyUp`, `WindowClose`, `Configure`, the
drag-and-drop set
`DragEnter`/`DragOver`/`DragLeave`/`Drop`/`DragEnded`, and the shell set
`SurfaceChanged`/`FocusChanged`/`StartMenu`. Method ids are pinned to the
values the hand-written protocol used (1-24), so the numbering stays
append-only from here on.

Endpoints and buffers are object fields of the request (the parcel's
object list, which the kernel resolves and installs in the receiver's
table): `CreateSurface` and `Subscribe` move one event endpoint
(`events`), `AttachBuffer` shares one pixel buffer (`pixels`, at least
`width * height * 4` bytes of RGBA8 for the surface's current content
size, rows tightly packed at that width).

Pointer coordinates in every event are relative to the surface content
origin; a move outside the surface (a press-and-drag) reports negative or
oversized values. Pointer events carry a button id (`1` left, `2` right,
`3` middle). Failures of calls are returned as a structured error field
(id 15, errno-style code plus friendly text) instead of the declared reply
fields, which never use that id.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| CreateSurface | 1 | sync | `(width: U32, height: U32, title: String, role: U32, popup: Option<U64>, events: Channel<os.lazy.display.v1>) -> (surface: U64)` |
| AttachBuffer | 2 | sync | `(surface: U64, pixels: Buffer) -> ()` |
| Commit | 3 | sync | `(surface: U64, x: U32, y: U32, w: U32, h: U32) -> ()` |
| DestroySurface | 4 | sync | `(surface: U64) -> ()` |
| PointerMove | 5 | event | `(x: I32, y: I32) -> ()` |
| PointerDown | 6 | event | `(x: I32, y: I32, button: U32) -> ()` |
| PointerUp | 7 | event | `(x: I32, y: I32, button: U32) -> ()` |
| KeyDown | 8 | event | `(key: U32) -> ()` |
| KeyUp | 9 | event | `(key: U32) -> ()` |
| WindowClose | 10 | event | `() -> ()` |
| DragStart | 11 | sync | `(surface: U64, token: U64, mime: String) -> ()` |
| DragCancel | 12 | sync | `(surface: U64) -> ()` |
| DragEnter | 13 | event | `(x: I32, y: I32, mime: String) -> ()` |
| DragOver | 14 | event | `(x: I32, y: I32) -> ()` |
| DragLeave | 15 | event | `() -> ()` |
| Drop | 16 | event | `(x: I32, y: I32, token: U64, mime: String, modifiers: Option<U32>, source: Option<U64>) -> ()` |
| DragEnded | 17 | event | `(dropped: Bool) -> ()` |
| ListSurfaces | 18 | sync | `() -> (surfaces: Array<SurfaceRow>)` |
| GetWorkArea | 19 | sync | `() -> (x: I32, y: I32, w: I32, h: I32)` |
| Subscribe | 20 | sync | `(subscriber_role: String, events: Channel<os.lazy.display.v1>) -> ()` |
| GetTheme | 21 | sync | `() -> (title_bg_active: U32, title_bg_inactive: U32, border: U32, taskbar: U32, text: U32, mode: String, accent: U32)` |
| SurfaceChanged | 22 | event | `(surface: U64, kind: U32, x: I32, y: I32, w: I32, h: I32, minimized: Bool, focused: Bool, title: Option<String>, role: U32, maximized: Bool) -> ()` |
| FocusChanged | 23 | event | `(surface: Option<U64>) -> ()` |
| StartMenu | 24 | event | `() -> ()` |
| AttachBufferSlot | 25 | sync | `(surface: U64, slot: U32, pixels: Buffer) -> ()` |
| Present | 26 | oneway | `(surface: U64, slot: U32, seq: U64, damage: Array<Rect>) -> ()` |
| BufferRelease | 27 | event | `(surface: U64, slot: U32) -> ()` |
| FrameDone | 28 | event | `(surface: U64, seq: U64) -> ()` |
| SetTitle | 29 | sync | `(surface: U64, title: String) -> ()` |
| HintOpenOrigin | 30 | sync | `(surface: U64, x: I32, y: I32, w: U32, h: U32) -> ()` |
| PointerWheel | 31 | event | `(x: I32, y: I32, delta: I32) -> ()` |
| SetSizeHints | 32 | sync | `(surface: U64, min_w: U32, min_h: U32, max_w: U32, max_h: U32) -> ()` |
| Configure | 33 | event | `(surface: U64, width: U32, height: U32, state: U32) -> ()` |
| RequestSize | 34 | sync | `(surface: U64, width: U32, height: U32) -> ()` |
| Ping | 35 | event | `() -> ()` |
| PlaceSurface | 36 | sync | `(surface: U64, x: I32, y: I32) -> ()` |
| ActivateSurface | 37 | sync | `(surface: U64) -> ()` |
| MinimizeSurface | 38 | sync | `(surface: U64) -> ()` |
| SetWorkArea | 39 | sync | `(x: I32, y: I32, w: I32, h: I32) -> ()` |
| SetIconGeometry | 40 | sync | `(surface: U64, x: I32, y: I32, w: I32, h: I32) -> ()` |
| HintLaunchOrigin | 41 | sync | `(x: I32, y: I32, w: U32, h: U32) -> ()` |
| Dismiss | 42 | event | `() -> ()` |
| GetOutput | 43 | sync | `() -> (width: U32, height: U32, scale: U32, refresh_millihz: U32)` |
| AllowPopup | 44 | sync | `(task: U64, token: U64, x: I32, y: I32, w: U32, h: U32) -> ()` |
| GrabPanelKeys | 45 | sync | `(grab: Bool) -> ()` |
| PanelKey | 46 | event | `(key: U32) -> ()` |
| TrayKeys | 47 | event | `() -> ()` |
| SetCloseMode | 48 | sync | `(surface: U64, ask: Bool) -> ()` |
| ActivateApp | 49 | sync | `(label: U32, session: U64) -> ()` |

## Service

- Served under the registry name `os.lazy.display.v1` (together with `os.lazy.display.cursor.v1`); registered once.

## Scopes, deadlines and retries

| Method | Scope interface | Scope id | Deadline | Retry |
|---|---|---|---|---|
| CreateSurface | this interface | - | 1000 ms | never |
| AttachBuffer | this interface | - | 5000 ms | never |
| Commit | this interface | - | 5000 ms | never |
| DestroySurface | this interface | - | 1000 ms | never |
| DragStart | this interface | - | 1000 ms | never |
| DragCancel | this interface | - | 1000 ms | never |
| ListSurfaces | this interface | - | 2000 ms | safe |
| GetWorkArea | this interface | - | 2000 ms | safe |
| Subscribe | this interface | - | 2000 ms | never |
| GetTheme | this interface | - | 5000 ms | safe |
| AttachBufferSlot | this interface | - | 1000 ms | never |
| SetTitle | this interface | - | 1000 ms | never |
| HintOpenOrigin | this interface | - | 500 ms | never |
| SetSizeHints | this interface | - | 1000 ms | never |
| RequestSize | this interface | - | 1000 ms | never |
| PlaceSurface | this interface | - | 2000 ms | never |
| ActivateSurface | this interface | - | 2000 ms | never |
| MinimizeSurface | this interface | - | 2000 ms | never |
| SetWorkArea | this interface | - | 2000 ms | safe |
| SetIconGeometry | this interface | - | 2000 ms | never |
| HintLaunchOrigin | this interface | - | 2000 ms | never |
| GetOutput | this interface | - | 500 ms | safe |
| AllowPopup | this interface | - | 5000 ms | never |
| GrabPanelKeys | this interface | - | 2000 ms | never |
| SetCloseMode | this interface | - | 1000 ms | never |
| ActivateApp | this interface | - | 2000 ms | never |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| CreateSurface | `events` | `Channel<os.lazy.display.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.display.v1` on |
| AttachBuffer | `pixels` | `Buffer` | `objects[0]`, a shared buffer |
| Subscribe | `events` | `Channel<os.lazy.display.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.display.v1` on |
| AttachBufferSlot | `pixels` | `Buffer` | `objects[0]`, a shared buffer |

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

- Window, Desktop, Panel, Popup

## enum `Change`

- Unspecified, Created, Destroyed, Moved, Minimized, Restored, Title, Resized, Maximized, Unmaximized

## enum `WindowState`

- Normal, Maximized
