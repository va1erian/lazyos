# `os.lazy.display.cursor.v1`

Interface id: `0x17e8a175efe87bc6`

The mouse pointer's shape over an app's own windows (`docs/cursor-plan.md`).

`xuid` serves this interface on the display service (`os.lazy.display.v1`,
the endpoint every display client already holds) and is also the ACL scope
it travels on: the parcel's interface id is this interface's, so the
kernel's label rules decide whether an app may change the pointer before
the compositor sees the call. A package asks for it by listing
`os.lazy.display.cursor.v1` in its manifest's `interfaces`; it is not
implied by `os.lazy.display.v1`. Unlabelled tasks (the Terminal, system
services) pass by the usual uid rules.

The compositor itself picks the arrow over the title bar, a resize shape
over a resizable window's border and while a resize runs, and the shape the
surface asked for over its content. Failures are returned as a structured
error field (id 15) like the display interface's.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| SetCursor | 1 | sync | `(surface: U64, shape: CursorShape) -> ()` |

## enum `CursorShape`

- Default, Busy, Text, Hand, ResizeH, ResizeV, ResizeNwse, ResizeNesw
