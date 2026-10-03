# xui dark theme proposals

Four directions for the desktop's dark look were drawn for issue #542 as
mockups of the Settings window: [design/xui-theme-proposals.html](design/xui-theme-proposals.html)
(open it in a browser). **B, Midnight gradient, was chosen** and is what xui's
dark theme implements. The others are kept here, with their tokens, as
ready-made presets for a future theming system: each one is a set of values
for the same roles, so a theme picker only has to swap the set.

All four share the fixes that started this:

- A widget does not paint a background of its own. It draws on its
  container, so labels, radio groups, check boxes and swatch rows no longer
  sit in darker boxes on a lighter panel.
- A different background marks a real section (a group of related
  settings, a sidebar, a toolbar), drawn once by the container: a *card*.

## Roles

| Role | What it is |
|---|---|
| desktop | wallpaper behind the windows |
| window | the window body |
| title (focused) | title bar of the focused window |
| sidebar | navigation column |
| nav selected | selected sidebar entry, plus its indicator |
| card | a section's background and border |
| card divider | line between rows inside a card |
| text / secondary | body text, section labels |
| control | radio, check box, swatch selection ring |
| button / primary | ordinary and default buttons |
| taskbar / task on | taskbar and the focused window's button |

## A. Flat Fluent

Neutral greys, no gradients; elevation by a shadow under the window only.

| Role | Value |
|---|---|
| window | `#202020`, border `#3a3a3a`, radius 8 |
| title | same as the window (`#202020`) |
| sidebar | transparent; selected `#2d2d2d` + 3 px accent bar |
| page | `#272727` |
| card | `#2d2d2d`, border `#353535`, radius 6; divider `#353535` |
| text / secondary | `#f2f2f2` / `#c7c7c7` |
| control | accent `#4caf7a`; ring `0 0 0 2px page, 0 0 0 4px #e8e8e8` |
| button / primary | `#353535` border `#404040` / accent fill |
| taskbar | `#1c1c1c`; task on `#2d2d2d` + 2 px accent underline |

## B. Midnight gradient (chosen)

Builds on the navy chrome LazyOS already has. Gradients are vertical and
subtle; a 1 px top highlight (white at 5-8 %) gives cards and buttons their
bevel; the accent glows on the selection.

| Role | Value |
|---|---|
| desktop | radial `#1d2a55` → `#121831` → `#0c1022` |
| window | vertical `#232a40` → `#1a1f30`, border `#3b4566`, radius 10, shadow |
| title (focused) | vertical accent: lighter `#3a7d58` → accent `#2c704a` → darker `#25603f`, top highlight white 18 % |
| title (unfocused) | vertical `#3a4468` → `#2c3452` |
| sidebar | black 15-35 %, right border `rgba(120,135,190,.12)` |
| nav selected | horizontal accent 28 % → 6 %; 3 px bar `#5fd08f` with glow |
| card | vertical `#2a3250` → `#242b44`, border `rgba(140,155,210,.16)`, radius 10, top highlight 5 % |
| card divider | `rgba(140,155,210,.10)` |
| text / secondary | `#e4e8f5` / `#9aa3c2` (section labels: uppercase 11 px, tracking .08em) |
| input | `#1a1f30`, border `#434e75` |
| control | accent `#5fd08f` (light accent), glow `rgba(95,208,143,.5)`; check box gradient `#5fd08f` → `#3a9e68` |
| swatch | round, inner highlight; selected ring 2 px card + 2 px accent |
| button | vertical `#323b5b` → `#2a3150`, border `#434e75`, radius 7, top highlight 8 % |
| primary | vertical `#4fbf82` → `#2f8a5a`, highlight 25 %, accent glow |
| taskbar | vertical `rgba(30,36,58,.92)` → `rgba(18,22,38,.96)`, top border `rgba(140,155,210,.18)` |
| task on | accent 30 % → 12 % + 2 px accent underline |

## C. Glass & glow

Translucent windows over coloured glows. Real backdrop blur is too costly
for the software compositor (every frame would re-blur what is behind a
window); the practical version samples a pre-blurred copy of the wallpaper
("Mica"), so windows look like glass over the desktop but not over each
other.

| Role | Value |
|---|---|
| desktop | `#0e1018` + radial accent glow (top right) + radial `#2f3a7a` (bottom left) |
| window | `rgba(22,24,32,.72)` over blurred wallpaper, border white 10 %, radius 14 |
| title | merged with the window (transparent), 34 px |
| nav selected | diagonal accent → `#2f6bb0` at 55 % / 35 %, highlight, accent shadow |
| page heading | text gradient white → `#9fe3bd` |
| card | white 4.5 %, border white 8 %, radius 12 |
| control | accent `#6fe0a0`, selection glow 14 px |
| button / primary | white 7 %, border white 12 % / diagonal accent → `#2f6bb0` |
| taskbar | `rgba(14,16,24,.65)` over blurred wallpaper |

## D. Graphite elevation

No borders: depth comes from shading and shadows; neutral greys with the
accent used sparingly.

| Role | Value |
|---|---|
| desktop | diagonal `#20232b` → `#14161b` |
| window | `#1b1c20`, radius 10, deep shadow + black 1 px outline |
| title | vertical `#2c2e35` → `#24262c`, highlight 7 %, accent status dot |
| sidebar | `#17181b`; selected raised `#2b2d33` → `#25272c` with accent icon |
| page | `#1f2024` |
| card | vertical `#292b30` → `#25262b`, radius 9, two-layer shadow, highlight 5 % |
| control | inset wells `#17181b`; accent `#4caf7a` / `#5fd08f` |
| button / primary | raised `#34363d` → `#2b2d33` / `#56c48a` → `#3a9e68` |
| taskbar | vertical `#26282e` → `#1c1d21`; task on raised + accent underline |

## Feasibility on a framebuffer

Gradients, translucency and glows are alpha blending, which both the app
canvas (tiny-skia) and the compositor can do. Glows inside a window are
painted by the app and cost only when that part repaints. A window shadow
is a nine-slice image computed once and blended around the frame. Only
live backdrop blur (C as drawn) is out of reach; see C for the substitute.
