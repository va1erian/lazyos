# Procedural Golf Course Generator & Mid-90s Software Renderer

A design for generating a playable 18-hole course from a seed, and for drawing it in the style of mid-90s PC golf games (Links 386 / Links LS, Microsoft Golf, PGA Tour 96 era): a 256-colour, dithered, painter's-algorithm view that builds up on screen after each shot, on top of the `xui-canvas` primitives.

## Implementation status

Implemented in `xui-app/crates/golf` (crate `xui-golf`) and shipped as the
LazyGolf app (`os.lazy.golf`): every Part I stage (`src/gen/`) and Part II
milestones 1-7 (`src/render/`, `src/minimap.rs`), with a free-flying camera
instead of the shot sequence for now (milestones 8-9: ball physics, swing
meter and scorecard are still to come). Where the code departs from this
design:

- **Depth buffer for everything.** Terrain is depth-tested too, so fly mode
  draws chunks nearest first (hidden pixels are never shaded) and finishes a
  frame in one step; *authentic* mode keeps the far-to-near progressive build.
- **Generation runs on a worker thread** (`src/loading.rs`). The search is
  tens of seconds on a weak CPU (an i3-7100U took 40-60 s), and each step
  used to run on the UI thread, which froze the window and the compositor's
  frame pacing for seconds at a time. The thread publishes its stage and
  progress, the timer tick only polls for the course, and the loading card
  shows the elapsed seconds. A superseded search (a new seed, or the window
  closing) stops at its next step.
- **Shading per pixel from the 1 m cells.** The mesh gives only the shape;
  material, light (bilinear within 60 m) and patterns come from one packed
  word per cell, so low-detail distant chunks keep their detail.
- **Resolution.** The framebuffer runs at the window's resolution divided by
  an integer scale that adapts to the frame cost (under QEMU/WHPX a 1024x640
  window renders at 1x at 30-60 fps).
- **A seed names a search** (`src/gen/search.rs`, `src/gen/quality.rs`):
  16 sites of the seed's archetype are scored from quarter-resolution
  terrain previews (relief, undulation, playable ground), the best 3 are
  built and routed twice each, and the routing that scores best is
  finished. The score rewards relief (judged against what is normal for
  the archetype), vistas from the tees, holes kept apart, variety of
  directions, shapes and lengths, water in play and a full-length course,
  and penalises blind shots, side-sloped landing zones and loosened
  routing constraints. The routing cost itself also steers each hole away
  from blind shots and side slopes, toward rolling ground with a view, and
  toward 95 m between holes so trees can stand between them.
- **Landmarks** (`src/gen/landmarks.rs`): after the land is raised, tall
  knolls (long dune ridges on a links), rocky crags (a `Rock` material) and
  stands of old-growth wood are set into it, and their cores are *blocked*:
  no tee, green or line of play (with 18 m either side) may cross one. A
  drawn hole that runs into a landmark is bent around it
  (`src/gen/routing/detour.rs`: a dogleg corner beside the obstacle, the
  length kept), straight par 4s and 5s cost the routing a little, and the
  quality score rewards a course whose long holes mostly bend.
- **Routing** splits the property into a front-nine and a back-nine half
  around the clubhouse and guides each nine along an out-and-back loop of
  waypoints; the closing holes pick their green near the clubhouse first.
  Holes may bend twice (S-shaped par 5s), and fairways meander off the
  centerline.

---

## Part I — Course generation

### Overview

Generate the land first, route the holes over it, then sculpt and paint around the routing. Par is computed last, from how the generated hole actually plays, not assigned up front.

```
seed ─► 1. Site terrain ─► 2. Hydrology ─► 3. Routing (18 holes)
     ─► 4. Hole layout ─► 5. Terrain sculpting ─► 6. Material painting
     ─► 7. Objects & trees ─► 8. Par & rating ─► 9. Render bake (Part II)
```

Each stage gets its own sub-RNG derived from the course seed, so changing one stage (say, tree placement) doesn't reshuffle another (the routing).

### 1. Site terrain

- Use a heightfield at 1 m per cell. A course needs about 50–80 ha, so 1024×1024 is plenty.
- Start with fBm noise (5–6 octaves) and add **domain warping** so ridges aren't grid-aligned.
- Add a low-frequency **landform archetype** layer:
  - **Links:** low amplitude, ridged-noise dunes, sandy soil.
  - **Parkland:** rolling hills.
  - **Mountain:** high amplitude, terraces.
- Finish with a few iterations of hydraulic erosion so the valleys look water-carved.

### 2. Hydrology

- Fill sinks, then compute D8 flow directions and flow accumulation.
- High accumulation becomes **streams**. Large enclosed sinks become **ponds and lakes**.
- Keep a `moisture` map (blurred accumulation plus distance to water). It drives rough density and tree species.

### 3. Routing

Pick a par mix: 4 par-3s, 10 par-4s and 4 par-5s (par 72), varied by ±1. Each hole is a corridor made of a tee, a green, and an optional dogleg control point.

**Length bands** (back tees, measured along the centerline):

| Par | Length |
|---|---|
| 3 | 110–230 m |
| 4 | 230–440 m |
| 5 | 430–600 m |

**Hard constraints:**
- No corridor overlaps another, with a safety buffer of about 35 m from each centerline (widest at the landing zones).
- The green of hole *n* is within about 80 m of the tee of hole *n+1*.
- Holes 1, 10 and 18 start or end near the clubhouse, so each nine loops.
- Tees and greens sit on low-slope cells. Steeper sites are allowed, but at a high earthwork cost.

**Soft costs:**
- Earthwork volume.
- Runs of holes facing the same direction (wind and sun variety matter).
- Runs of holes with the same par.
- A reward for interest: water or ravines at carry distance, elevated tees.

**Solver:** beam search hole by hole to get a valid routing, then **simulated annealing** to refine it. Moves are: nudge a green, swap the pars of two holes, move a dogleg point.

### 4. Hole layout

Each hole becomes a centerline spline from tee to green. Along it:

- **Landing zones** at the drive distances of two player models: scratch (about 240 m) and bogey (about 190 m).
- **Fairway width** of 25–45 m, varying along the hole. It pinches near hazards and widens at the bogey landing zone.
- **Green:** a noise-perturbed ellipse or superformula blob of 400–700 m². Larger for long approaches, smaller for wedge shots.
- **Bunkers**, placed strategically:
  - fairway bunkers at the scratch landing distance, on the outside of doglegs;
  - greenside bunkers on the side the approach angle tempts you to miss.
- **Tee sets:** 3–5 of them (back, middle, forward, ladies), stepped along the centerline.

### 5. Terrain sculpting

Blend the heightfield toward shaped targets using signed distance fields (SDFs):

```
h' = lerp(h_natural, h_target, smoothstep(blend_radius, 0, sdf))
```

- **Tees:** flat pads with about 1% drainage slope, often raised 0.5–1.5 m.
- **Fairways:** low-pass the natural terrain inside the corridor. Keep the roll, remove the spikes.
- **Greens:** a base plane tilted 1–3% back to front, plus 1–2 low tiers or a false front. Keep slope below 3% wherever a pin could go.
- **Bunkers:** depressions 0.3–1.5 m deep, with a steep face toward the hole and a raised lip.
- **Ponds:** flattened to a water level, with a bank around the shoreline.

### 6. Material painting

Assign one material per cell, in priority order from the SDFs:

| Material | Rule |
|---|---|
| `Water` | below water level in a pond or stream |
| `Sand` | inside a bunker SDF |
| `Green` | inside the green SDF |
| `Fringe` | 0–1.5 m outside the green |
| `TeeBox` | inside a tee SDF |
| `Fairway` | inside the fairway SDF |
| `FirstCut` | 0–2 m outside the fairway |
| `Rough` | elsewhere in the corridor |
| `DeepRough` / `Fescue` | outside the corridor with low tree density (links) |
| `WasteArea` | dry, sandy, steep cells (links) |
| `Woodland` | outside the corridor with high moisture or tree density |
| `CartPath` | spline rasterized at 2.5 m width |
| `OutOfBounds` | outside the property polygon |

**Also keep the SDF values**, not only the enum. The renderer uses them to draw smooth dithered edges instead of 1 m staircases (see Part II, step 6).

### 7. Objects and trees

**Trees:** use Poisson disk sampling with a density field:

```
density = f(distance_to_corridor) * moisture * archetype_factor
```

- Density is zero inside the corridor and ramps up beyond about 15 m.
- Allow a few **specimen trees** at dogleg corners, guarding one side of the angle.
- Choose species from elevation and moisture: willow near water, pine on dry ridges, umbrella pine and olive for a Provence course.
- Reject candidates in landing zones and on the approach line within 60 m of the green.

**Fixed objects:**
- Flagstick on a pin position with local slope below 3%.
- Tee markers on each tee set.
- Yardage markers at 200, 150 and 100 m from the green center.
- OB stakes along the property line.
- Benches and ball washers at tees.
- Bridges where the cart path crosses water.
- Clubhouse and practice green on a flat site near holes 1, 10 and 18.

**Cart path:** A* from green *n* to tee *n+1*. The cost penalizes slope, the play corridor, and water.

### 8. Par by simulated play

Par comes from a **player model**, so terrain counts: a 400 m par 4 that drops 40 m plays like 360 m.

**Effective length** is the centerline length adjusted by:
- elevation, about ±1 m of length per metre of drop or rise;
- forced carries over water or ravines;
- a dogleg penalty if the corner can't be cut.

**Value iteration** for a scratch player, on a grid of about 3 m:

```
E(x) = 1 + min over aim a of  Σ P(x' | x, a, club) · [E(x') + penalty(x')]
```

- Shot outcomes are Gaussian dispersion around the aim point, scaled by club and lie. Rough cuts distance; sand adds dispersion.
- `penalty` is +1 for water or OB, with the ball dropped per the rules.
- On the green, use a fitted putting curve: about 1.0 strokes at 1 m, 1.5 at 3 m, 2.0 at 12 m.

**Hole par:**

```
reach = expected strokes for scratch to reach the green from the tee
par   = clamp(round(reach) + 2, 3, 5)
```

Cross-check against the length bands. If they disagree, either nudge the tee or green and re-solve, or keep the hole as a "long par 4" or "short par 5".

**Rating:** run the same solver for a bogey player model. The scratch and bogey expected scores give a **course rating and slope**, and the per-hole bogey-minus-par difference gives each hole's **handicap index**.

### Output types

```rust
pub struct Course {
    pub seed: u64,
    pub cell_size_m: f32,
    pub width: u32,
    pub height: u32,
    pub elevation: Vec<f32>,        // metres
    pub material: Vec<Material>,
    pub edge_sdf: Vec<f32>,         // signed distance to nearest material boundary
    pub objects: Vec<Object>,       // trees, markers, flags, benches...
    pub holes: [Hole; 18],
    pub par: u8,
    pub rating: f32,
    pub slope: u16,
}

pub struct Hole {
    pub number: u8,
    pub par: u8,
    pub tees: Vec<TeeSet>,
    pub centerline: Vec<Vec2>,
    pub green: Polygon,
    pub pin: Vec2,
    pub effective_length_m: f32,
    pub scratch_expected: f32,
    pub handicap_index: u8,
}

#[repr(u8)]
pub enum Material {
    Green, Fringe, TeeBox, Fairway, FirstCut, Rough, DeepRough,
    Sand, WasteArea, Water, Woodland, CartPath, OutOfBounds,
}
```

---

## Part II — A mid-90s software renderer on xui-canvas

### The look you're after

What made those games look the way they did:

- **Low resolution, indexed colour.** 320×200 or 640×480 with a 256-colour palette, built from a few colour ramps per material.
- **Painter's algorithm, back to front, drawn visibly.** After each shot the view was rebuilt over a couple of seconds: horizon and far hills first, then nearer terrain overdrawing it, then trees popping in. That progressive reveal *is* the "slow realtime" feel, so the renderer should embrace it rather than hide it.
- **Flat or Gouraud-shaded terrain** with ordered dithering and simple world-space patterns: mowing stripes, speckled sand, noisy rough.
- **Billboarded bitmap trees**, scaled with nearest-neighbour sampling, colour-keyed (no alpha blending).
- **A backdrop** (sky gradient, distant hills or a panorama) drawn first.
- **Palette tricks:** distance haze through colour lookup tables, and palette-cycled water.

### What xui-canvas gives you, and what to avoid

From `crates/xui-canvas/src/canvas.rs`, `SkiaCanvas` wraps a `tiny-skia` pixmap and implements xui's `Canvas` trait. The primitives that matter here:

| Primitive | Use it for |
|---|---|
| `draw_image(&Image, Rect)` | **Presenting the 3D view.** This is the main path. |
| `fill_rect`, `fill_rect_rgba`, `stroke_rect`, `draw_line` | HUD panels, bevels, the swing meter |
| `fill_rect_linear`, `fill_rect_radial` | HUD gradients (title bars, meter fill) |
| `fill_polygon(&[Point], Color)`, `fill_ellipse` | HUD icons: wind arrow, minimap markers |
| `draw_text`, `measure_text` | Hole and club info, yardages |
| `push_clip` / `pop_clip`, `save` / `restore`, `set_scale_translate` | Layout |

Supporting types from `xui-core`:
- `Point` and `Rect` use `i32` coordinates. `Rect` is half-open (`right` and `bottom` are exclusive).
- `Color::rgb(r, g, b)` and `Color::hex(0xRRGGBB)` build opaque 24-bit colours.
- `Image::from_rgba(w, h, Vec<u8>)` wraps tightly packed RGBA8 pixels, row-major and top-down.

Three implementation details in `canvas.rs` shape the renderer design.

**1. Don't rasterize the terrain with `fill_polygon`.**
- It goes through tiny-skia's anti-aliased path filler, which is the wrong look: you want hard aliased edges and dithering.
- It calls `ensure_mask()`, which, with a clip active, builds a full-surface coverage mask. Thousands of terrain triangles per frame would be very slow.

So: **rasterize into your own framebuffer**, and hand xui one image per frame.

**2. `draw_image` has a fast path. Stay on it.**

When there is no coverage mask *and* the destination size equals the image size, `draw_image` does a plain row copy (`blit::unscaled`). Otherwise it goes through a `Pattern` shader with `FilterQuality::Bilinear`. That path is slower, and **bilinear filtering will smear your pixel art**. Therefore:

- Do the integer **nearest-neighbour upscale yourself**, so the image you pass is already at destination size.
- Make sure the destination `Rect` maps 1:1 to device pixels *after* the canvas transform. `draw_image` applies the current translation and scale, so if you've called `set_scale_translate` with a non-1 scale (or the DPI implies one), size the image to the mapped size.
- **Draw the 3D view before any HUD polygon or ellipse fill.** Those fills call `ensure_mask()`, and once a mask exists the fast path's `mask.is_none()` check fails. Also keep the view out of rounded clips (`push_clip_rounded`), which build a mask too.

**3. Images are immutable and cached by identity.**
- `Image::id()` is unique per construction, and clones share it.
- The canvas keeps an LRU cache keyed by that id, sized by bytes, holding the premultiplied upload.
- Each `Image::from_rgba` is therefore a new cache entry plus one upload on its first draw. That's fine at a few frames per second during a progressive redraw, but:
  - keep the upscaled image modest in size (see "Resolution" below);
  - once a view is finished, **keep the same `Image` (clone it) for every repaint** so later repaints are cached row copies;
  - build static images (minimap, tree sprite atlas previews) once.

### Architecture

```
┌──────────────────────── your code ────────────────────────┐
│ Course ─► RenderBake ─► RenderJob::step(budget)            │
│                          │ writes palette indices          │
│                          ▼                                 │
│                    Framebuffer (u8 index + u16 depth)      │
│                          │ palette + cycling               │
│                          ▼                                 │
│                 resolve(): index→RGBA, ×k nearest upscale  │
│                          │                                 │
│                          ▼                                 │
│                 Image::from_rgba(W*k, H*k, rgba)           │
└──────────────────────────┬─────────────────────────────────┘
                           ▼
        canvas.draw_image(&image, view_rect)   // fast path
        canvas HUD primitives on top           // after the blit
```

Keep the framebuffer **indexed** (`Vec<u8>` plus a 256-entry palette), and convert to RGBA only in `resolve()`. That gives you the authentic palette constraints, cheap lookup-table fog, and palette cycling for free.

### 1. Resolution and aspect

- **320×200** is the most authentic. It was displayed on 4:3 monitors with non-square pixels, so upscale **×5 horizontally and ×6 vertically** to get 1600×1200, an exact 4:3.
- **640×480** has square pixels; upscale ×2.
- Pick the largest integer factor that fits the window and letterbox the rest with `fill_rect`.

An upscaled 1600×1200 RGBA image is about 7.7 MB. During the progressive build, publish at most every UI frame (or throttle to around 15 Hz). Otherwise the image cache spends its life evicting yesterday's sky.

### 2. Render bake (once per course)

Precompute everything that doesn't depend on the camera:

- **Vertex normals** from the heightfield (central differences).
- **Sun shading** per vertex: `shade = ambient + diffuse * max(0, N·L)`, quantized to 0–15.
- **Shadow map:** for each cell, march toward the sun over the heightfield. Also stamp each tree's shadow (a dark ellipse offset along the sun direction) into the map. Store it as a per-cell shade reduction.
- **Chunks:** split the terrain into 32×32-cell chunks, each with a bounding box and a list of the trees and objects whose base lies inside it.
- **LOD meshes** per chunk at steps of 1, 2, 4 and 8 cells. Add downward **skirts** on chunk edges to hide cracks between neighbouring LOD levels.
- **The palette** (see step 5).
- **Tree sprites** (see step 7).

### 3. Camera

- The camera sits behind the ball at about 1.7 m eye height, looking toward the target line, with a horizontal FOV of about 60°.
- Use a standard look-at view matrix plus a perspective projection. Keep a **far plane of about 600 m**; beyond that, the backdrop takes over.
- Pick chunk LOD by distance: step 1 within 60 m, 2 within 150 m, 4 within 300 m, and 8 beyond.

### 4. Painter's order on a heightfield

You don't need a z-buffer. Heightfields have a convenient property: drawn back to front by chunk, then back to front by cell within each chunk, the order is almost always correct.

**Chunk order:**
- Frustum-cull chunks with their bounding boxes.
- Sort the survivors by distance from the camera to the chunk center, farthest first.

**Within a chunk:**
- Iterate rows and columns *toward* the camera. If the camera is at larger x than the chunk, iterate x ascending; otherwise descending. Same for z.
- If the camera is inside a chunk, split that chunk into four quadrants around the camera cell and draw each quadrant the same way, far corner first.

**Interleaving sprites:**
- After finishing each cell row inside a chunk, draw that row's trees and objects, sorted by depth.
- This is what makes trees correctly hide terrain behind them while nearer hills hide the trees.

Occasional ordering errors happen, for example a tall ridge in one chunk against a tree in the next. Call it period-accurate, or keep a `u16` depth buffer and depth-test sprites only, which fixes the visible cases cheaply.

### 5. Palette, shading, dithering, haze

**Palette layout (256 entries):**

| Range | Contents |
|---|---|
| 0–15 | UI greys and black |
| 16–31 | sky ramp, horizon to zenith |
| 32–47 | fairway greens, dark to light |
| 48–63 | green (putting surface), slightly bluer and lighter |
| 64–79 | rough |
| 80–95 | deep rough / fescue, yellowish |
| 96–111 | sand |
| 112–127 | water (reserved for palette cycling) |
| 128–143 | tree foliage ramp A |
| 144–159 | tree foliage ramp B |
| 160–175 | bark, path and stone |
| 176–191 | backdrop hills |
| 192–255 | flag, ball, golfer sprite, spare |

**Ordered dithering:** the ramps give 16 shades, but your lighting is continuous. Dither between adjacent ramp entries with a 4×4 Bayer matrix:

```rust
const BAYER4: [[u8; 4]; 4] = [
    [ 0,  8,  2, 10],
    [12,  4, 14,  6],
    [ 3, 11,  1,  9],
    [15,  7, 13,  5],
];

#[inline]
fn shade_index(ramp_base: u8, light: u16 /* 0..=15*16 */, x: usize, y: usize) -> u8 {
    let level = (light >> 4) as u8;          // 0..=15
    let frac  = (light & 15) as u8;          // sub-step
    let bump  = (frac > BAYER4[y & 3][x & 3]) as u8;
    ramp_base + (level + bump).min(15)
}
```

**Haze through a lookup table** (the same idea as Doom's COLORMAP):
- Precompute `fog_lut[level][index] -> index` for about 16 fog levels, mapping each colour to the palette entry nearest to `lerp(colour, haze_colour, level / 15)`.
- Per pixel: `fb[i] = fog_lut[fog_level(depth)][idx]`, with the fog level dithered via Bayer as well.
- That's one table lookup per pixel and gives the classic desaturated, bluish distance haze.

### 6. Triangle rasterizer and material patterns

Write a plain scanline rasterizer:

- **Edges** in 16.16 fixed point, with a top-left fill rule so shared edges are neither drawn twice nor left as gaps.
- **Per-vertex attributes:** screen x and y, `1/z`, world `u/z` and `v/z`, and `shade/z`.
- **Perspective correction:** recompute the true `u` and `v` every 16 pixels along a span and interpolate linearly in between (Quake-style subdivision).
  - Far triangles are tiny, so this is nearly free.
  - Near triangles are large: at 1.7 m eye height, the cells around your feet fill the bottom of the screen. Without correction, the mowing stripes would swim.

**Per-pixel shading:**

```rust
let (wx, wz) = (u, v);                       // world metres
let jitter = (BAYER4[y & 3][x & 3] as f32 - 7.5) * EDGE_JITTER; // ~0.03 m
let mat = course.material_at(wx + jitter, wz - jitter);
let light = vertex_shade + pattern(mat, wx, wz) - shadow_at(wx, wz);
fb[i] = fog(shade_index(RAMP[mat], light, x, y), depth);
```

The Bayer **jitter on the material lookup** turns blocky 1 m material boundaries into dithered edges, which is exactly how those games looked around greens and bunkers. If you stored `edge_sdf`, you can use it instead for smoother shapes.

**Patterns** are procedural and live in world space:

| Material | Pattern |
|---|---|
| Fairway | Mowing stripes: `((dot(p, hole_axis) / 8.0).floor() as i32 & 1) * 12` light bump |
| Green | Finer stripes (2 m), lighter base |
| Rough | High-frequency value noise, ±2 shades |
| Fescue | Value noise, ±3 shades, with a few darker clumps |
| Sand | Light speckle noise; darker on faces turned away from the sun |
| Water | A reflection gradient (darker near the camera) using the cycling range |
| Cart path | Flat grey with a slight edge darkening |

### 7. Trees and objects as sprites

**Generating sprites offline** (once per species, at bake time):
- Build each tree as a cluster of foliage spheres around a trunk cylinder.
- Ray-march it with Lambert lighting from the same sun as the terrain.
- Quantize the result to the species' foliage ramp, using index `0` as the transparent colour key.
- Produce **6–8 pre-scaled sizes**: 8, 12, 16, 24, 32, 48, 64 and 96 px tall. Hand-scaling small sprites looks better than scaling down at runtime.
- Add 2–3 variants per species, mirrored at random, so forests don't look tiled.

**Drawing a tree:**
1. Project its base to the screen. The projected height is `tree_height_m * focal / z`.
2. Choose the nearest pre-scaled size, then nearest-neighbour scale to the exact height.
3. Draw it column by column, clipped to the view, skipping key pixels and running each pixel through the fog table.
4. Optionally depth-test it against the `u16` depth buffer.

**Other objects:**
- **Flagstick:** a vertical one-pixel line in white with a red sprite flag. Use 2–3 flag frames to flutter it with the wind.
- **Tee markers and yardage posts:** tiny sprites.
- **Ball:** 1–2 px, drawn last, in pure white so it pops against everything.

### 8. Sky and backdrop

Draw this first, every frame of the build:

1. **Sky:** a vertical gradient from the horizon haze colour to zenith blue, dithered with Bayer. Shift the horizon line with camera pitch.
2. **Clouds:** fBm thresholded into 2–3 bands of lighter sky indices. Scroll them horizontally with camera yaw at low parallax.
3. **Distant hills:** a 360° silhouette from 1D noise sampled by yaw angle. Fill below it with the backdrop ramp, lighter at the top, plus haze.

Links used a photographed panorama here. If you want that, decode a PNG with `Image::decode`, quantize it to the backdrop ramp at bake time, and blit it into the framebuffer.

### 9. Progressive "slow realtime" rendering

Make the renderer a **resumable job** that runs inside a time budget each UI frame:

```rust
pub enum Phase {
    Sky,
    Chunks { order: Vec<ChunkId>, next: usize, row: u16 },
    Overlay,          // ball, flag, markers
    Done,
}

pub struct RenderJob {
    camera: Camera,
    phase: Phase,
    /// Authentic-mode throttle: max chunks per step even if time remains.
    max_chunks_per_step: Option<usize>,
}

impl RenderJob {
    /// Renders until `budget` elapses or the job finishes. Returns true when done.
    pub fn step(&mut self, fb: &mut Framebuffer, course: &RenderBake, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            match &mut self.phase {
                Phase::Sky => { draw_sky(fb, &self.camera, course); self.phase = Phase::Chunks { /* sorted */ .. }; }
                Phase::Chunks { order, next, row } => {
                    // draw one row of one chunk, then its sprites; advance cursor
                    if *next == order.len() { self.phase = Phase::Overlay; }
                }
                Phase::Overlay => { draw_overlay(fb, &self.camera, course); self.phase = Phase::Done; }
                Phase::Done => return true,
            }
            if Instant::now() >= deadline { return false; }
        }
    }
}
```

**In the paint handler:**

```rust
fn paint(&mut self, canvas: &mut dyn Canvas) {
    if !self.job_done {
        self.job_done = self.job.step(&mut self.fb, &self.bake, Duration::from_millis(8));
        // Publish a fresh image: new id, one upload. Fine at a few Hz.
        self.view_image = self.fb.resolve(&self.palette, self.scale_x, self.scale_y);
        self.request_repaint();
    } else if self.palette_tick() {
        // Water cycling: rebuild from the same indices with rotated palette entries.
        self.view_image = self.fb.resolve(&self.palette, self.scale_x, self.scale_y);
    }

    // 1) The 3D view FIRST, while no coverage mask exists → unscaled row-copy fast path.
    canvas.draw_image(&self.view_image, self.view_rect);

    // 2) HUD on top (polygons/ellipses may build a mask; that's fine now).
    self.draw_hud(canvas);
}
```

**`resolve()`** turns indices into an upscaled RGBA `Image`:

```rust
impl Framebuffer {
    pub fn resolve(&self, pal: &[[u8; 4]; 256], sx: usize, sy: usize) -> Image {
        let (w, h) = (self.w * sx, self.h * sy);
        let mut out = vec![0u8; w * h * 4];
        for y in 0..self.h {
            let src = &self.idx[y * self.w..(y + 1) * self.w];
            // Expand one source row horizontally...
            let row_start = (y * sy) * w * 4;
            {
                let row = &mut out[row_start..row_start + w * 4];
                for (x, &i) in src.iter().enumerate() {
                    let px = pal[i as usize];
                    for k in 0..sx {
                        row[(x * sx + k) * 4..(x * sx + k) * 4 + 4].copy_from_slice(&px);
                    }
                }
            }
            // ...then duplicate it vertically.
            for r in 1..sy {
                out.copy_within(row_start..row_start + w * 4, row_start + r * w * 4);
            }
        }
        Image::from_rgba(w as u32, h as u32, out).expect("size matches")
    }
}
```

**Authentic mode:** set `max_chunks_per_step = Some(2)` or so, and the scene assembles on screen the way it did on a 486. **Fast mode:** no throttle, an 8–12 ms budget, and on a modern CPU at 320×200 the whole view finishes in a frame or two.

**Palette cycling for water:** rotate entries 112–127 every 120 ms. Because the framebuffer stores indices, you only re-run `resolve()`; nothing is re-rendered. This was often the only thing on screen that moved, and players loved it.

### 10. The shot sequence

1. **Address.** The view is finished. Keep a `clean_fb` copy of the index buffer, and keep the `view_image` clone around so repaints are cached blits.
2. **Swing.** The HUD swing meter animates while the view image stays the same.
3. **Ball flight.** Integrate the ball with gravity, quadratic drag, and Magnus lift from spin.
   - Each UI frame: copy `clean_fb` into `fb`, project the ball and draw it (plus an optional shadow dot on the ground), then `resolve()` and publish.
   - A cheaper alternative is to `draw_ellipse` the ball with the canvas over the cached view image. It's anti-aliased, but at 2 px nobody will notice.
4. **Landing.** Bounce and roll using a per-material restitution and friction table:

   | Material | Behaviour |
   |---|---|
   | Green | soft landing, long roll |
   | Rough | grabs the ball |
   | Sand | plugs it |
   | Water | splash sprite, penalty |

5. **Camera cut.** Place the camera behind the ball facing the pin, start a new `RenderJob`, and watch the course paint itself in again. That cut-and-repaint rhythm *is* the mid-90s golf experience.

Optional: a "reverse angle" camera from the green when the ball is near the hole, rendered by a second job.

### 11. HUD with xui primitives

The HUD is the part where the canvas primitives are a good fit. Draw everything here **after** the 3D blit.

**Bevelled panel** (Win95 style):

```rust
fn bevel_panel(c: &mut dyn Canvas, r: Rect) {
    c.fill_rect(r, Color::hex(0xC0C0C0));
    let (l, t, rt, b) = (r.left, r.top, r.right - 1, r.bottom - 1);
    c.draw_line(Point::new(l, t),  Point::new(rt, t), Color::hex(0xFFFFFF), 1.0);
    c.draw_line(Point::new(l, t),  Point::new(l, b),  Color::hex(0xFFFFFF), 1.0);
    c.draw_line(Point::new(l, b),  Point::new(rt, b), Color::hex(0x404040), 1.0);
    c.draw_line(Point::new(rt, t), Point::new(rt, b), Color::hex(0x404040), 1.0);
}
```

**Swing meter**, in the classic three-click style (start, power, accuracy):
- Use `fill_rect` for the bar background.
- Fill the power level with `fill_rect_linear` (green to yellow to red).
- Mark the accuracy zone with `stroke_rect`.
- Draw the moving cursor as a `fill_rect` two pixels wide.

**Info box:** hole number, par, distance to pin, club and lie, using `draw_text` with `measure_text` for alignment. A bitmap-style font sells the look if xui lets you choose one.

**Wind indicator:** a `fill_ellipse` compass with a `fill_polygon` arrow rotated by the wind angle and the wind speed in text.

**Overhead minimap:**
- Render the hole top-down from the material map once, at about 2 m per pixel, rotated so the tee is at the bottom. Use the same palette and patterns.
- Create the `Image` **once per hole**. It stays cached, and if you draw it at its native size it uses the fast row copy.
- Overlay the ball, pin and aim line with `fill_ellipse` and `draw_line` each frame.

**Scorecard:** `fill_rect` grid cells with `draw_text`, inside a `push_clip` region.

### 12. Performance notes

| Item | Rough cost at 320×200 |
|---|---|
| Sky + backdrop | < 0.2 ms |
| Terrain, ~20–40k visible triangles with LOD | 2–6 ms (painter's overdraw ≈ 2–3×) |
| Trees, a few hundred sprites | 1–3 ms |
| `resolve()` ×5/×6 to 1600×1200 | 2–4 ms |
| `Image::from_rgba` + first `draw_image` upload | a few ms (copy + premultiply) |

At 640×480 everything is roughly 4–5× more. That's still fine for a progressive build, and with a throttle on it's exactly the point.

**Cheap wins:**
- Skip triangles whose screen bounding box is smaller than a pixel and plot a single dot instead.
- Cull back-facing terrain triangles. On a heightfield, a hill hides its far side anyway.
- Run `resolve()` only on rows that changed since the last publish, while keeping the previous RGBA buffer around.

### 13. Suggested milestones

1. Heightfield plus materials rendered as a top-down minimap image. This validates the generator.
2. Sky and backdrop, plus flat-shaded terrain triangles at a single LOD.
3. Palette ramps, Bayer dithering, fog lookup table.
4. Perspective-correct patterns (stripes) and dithered material edges.
5. Chunk LOD, skirts, back-to-front traversal.
6. Tree sprite baking and interleaved sprite drawing.
7. Resumable `RenderJob`, authentic throttle, palette cycling.
8. Ball physics, camera cuts, HUD.
9. Par solver output in the HUD and the scorecard.

At step 2 the terrain will look like a crumpled green tablecloth. That's normal. Everything from 1995 looked like that until the dithering went in.
