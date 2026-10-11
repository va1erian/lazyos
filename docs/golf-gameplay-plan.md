# LazyGolf gameplay plan

Status: draft. Scope: turn the free-flying LazyGolf viewer into a golf game
(play 18 generated holes, keep score) in the style of the mid-90s 3D golf
games, with a **camera that is always well placed to watch the shot**.
This continues milestones 8-9 of [`golf-course-generator.md`](golf-course-generator.md)
(section 10 "The shot sequence" is the sketch this plan expands).

## 1. What we are copying

Reference points (from memory of the games; check against footage before
fixing numbers): T&E Soft's PC-98/console golf line (*Waialae*, *Harukanaru
Augusta*, *Pebble Beach no Hatou*; *Hyper* branding in the West), Access's
*Links 386 / Links LS*, EA's *PGA Tour 96*, Microsoft *Golf*.

What defines the genre, and what we keep:

| Convention | In LazyGolf |
|---|---|
| One shot = one *shot sequence*: address view, swing, flight, landing, rest, cut to next address | A state machine (section 3); every state names its camera |
| Hole preview flyover on arriving at a tee | Reuse the bench flyover (`fly_bench`, `polyline_at`) as a skippable preview |
| Caddy picks a club from distance, lie and wind; the player can override | `caddy::recommend` |
| Aim marker on the ground at the expected carry; left/right rotates the whole view | Aim cross drawn into the frame, depth-tested |
| Three-click swing meter (start, power, accuracy) | HUD meter, section 5 |
| Ball is 1-3 px, white, with a ground shadow; camera pans after it and **cuts** to a landing view | Section 4 |
| Putting view: low camera behind the ball, a slope grid on the green | Section 4.5 and 6 |
| Cut-and-repaint: each cut repaints the scene (it *is* the look) | `Mode::Authentic` repaints on cuts |
| Info box: hole, par, distance to pin, club, lie, wind | HUD, section 7 |

Non-goals for v1: course editor, online play, club/ball equipment choices,
a modelled golfer mesh (a sprite is enough), career mode.

## 2. What exists

`xui-app/crates/golf` already has everything the game stands on:

- `Course` / `Hole`: per-cell `material`, `elevation`, `hole_of`, tees, `pin`,
  `green`, `landing_zones`, par and stroke index (`course.rs`).
- `Scene` / `RenderJob` / `Frame`: depth-buffered 8-bit renderer, fly and
  authentic (progressive) modes, `Camera { eye, yaw, pitch, fov, far }`.
- `Bake::height_at`, `Course::material_at`, `ground_at` (water-aware).
- `Flyer` (free camera), `Game`/`Running` (tick, scale adaptation, present),
  `hud.rs` (bevel panel, map), `Minimap`, Report markers.

Missing: a ball, a swing, rules, score, a shot camera. `game.rs` is 460
lines, so gameplay lives in a new `play/` module and `Running` is split
(section 8) rather than growing past the 500-line limit.

## 3. Game flow

```
Preview -> Address -> Swing -> Flight -> Rest -+-> Address (next stroke)
   ^          ^                                +-> Penalty -> Drop -> Address
   |          +-- aim, club, stance, wind       +-> Holed -> HoleSummary -> Preview(next)
 skip                                                              \-> Scorecard (after 18)
```

`Free` (today's flyer) stays as a **practice/spectator camera**: `F` toggles
it from any Address state; the ball and state freeze while it is on.

Each state owns: its camera shot (section 4), the inputs it accepts, and
the overlay it draws. The state machine is pure data plus `step(dt, input)`,
so it runs under `cargo test` with no window.

## 4. The shot camera (the main design problem)

The camera is a **director**, not a flyer: `play::cam::Director` takes the
game state (ball, lie, aim point, target, trajectory) and returns a
`Camera` each tick. A cut is a hard switch (period-correct); inside a shot,
motion is damped (critically damped springs on yaw, pitch, FOV and eye).

### 4.1 Pre-simulated flight makes this easy

At the instant of impact the whole shot is **simulated up front** at a fixed
240 Hz step into a `Vec<Sample { t, pos, vel, event }>` (events: apex,
first bounce, water, tree, rest, holed). Playback just indexes it by wall
clock. The director can therefore look **into the future**: it knows where
and when the ball lands before it gets there, so it can cut to the right
landing view *early* and have the ball enter the frame. It also makes
replays, the AI and tests trivial (deterministic, no timers).

### 4.2 Shots

| Shot | When | Eye | Looks at | FOV |
|---|---|---|---|---|
| **Preview** | arriving on a tee | flyover along the centerline, 35 m up, then settle to *Address* | ahead on the line | 60 |
| **Address** | aiming, swinging | behind the ball on the ball-to-aim line, over the shoulder (offset 0.8 m right) | a point on the line so the aim marker sits in the upper third | 50-58 |
| **Track** | ball in the air | the Address eye, held | the ball (spring-damped pan/tilt) | zooms in with distance to hold ~6 px ball |
| **Landing** | ball past apex and landing is more than 60 m from the eye | cut to a point short of and beside the landing spot (below) | landing spot, then the ball through bounce and roll | 40 |
| **Roll** | ball on the ground | stay on Landing; follow the ball with a lead | the ball | 40-30 |
| **Green back** | approach onto a green (landing within 25 m of the pin) | 10-20 m beyond the pin, 2 m up, looking back at the player's side | pin then ball | 45 |
| **Cup** | ball holed, or last 2 m of a putt | 0.4 m above the green, 2.5 m from the cup, off the line | the cup | 35 |
| **Splash** | water / OB | the Landing eye, held on the splash and a 1 s hold | impact point | 40 |
| **Putt** | on the green (or fringe, putter chosen) | 1.8 m behind the ball, 0.9 m up | the cup (or aim point) | 50 |

### 4.3 Address camera placement (the part players see most)

Goals: ball low in the frame (about 78% down), the aim point and flag in the
upper third, horizon around 40%, nothing between eye and ball.

```
dir     = normalize(aim - ball)            // ground plane
dist    = |aim - ball| clamped 15..300     // to the target, not the pin
back    = lerp(4.0, 10.0, t)               // t = smoothstep(dist, 20, 220)
height  = lerp(1.8, 3.6, t)                // ground(ball) + height, never below +1.2
side    = 0.8
eye     = ball - dir*back + right*side + up*height
look    = ball + dir * (0.65*dist)         // not the target itself: keeps the
                                           // landing area, not the horizon, centred
pitch   = solved so `look` projects to 38% from the top
```

Then three corrections, run in order, each cheap (a handful of
`height_at` samples):

1. **Ground clearance**: eye at least 1.2 m over `ground_at` (and over any
   water surface).
2. **Line of sight**: march eye -> ball and eye -> look over the heightfield
   (8-16 samples); if a crest blocks it, raise the eye in 0.5 m steps up to
   +8 m, and as a last resort move it back 2 m. A shot that is still blind
   (dog-leg over trees, uphill approach) is allowed: the aim marker is
   always drawn, plus a flag-top marker, as the old games did.
3. **Trees**: the objects list is per-chunk; test the eye and the eye->ball
   segment against trunks (radius ~0.4 m, vertical extent from the object
   height). If the eye is inside a trunk or canopy, slide it sideways to the
   clear side (try +/-1.5 m, then +/-3 m).

The same solver, with different constants, gives the chip and putt
views, so there is one code path and one set of tests.

### 4.4 Flight: tracking, then the cut

1. **0 - 1.2 s**: Track from the Address eye. Pan with the ball, FOV
   narrows so a long drive does not shrink to a pixel. Ball at screen
   centre-ish, with a dead zone (10% of the frame) so small motion does
   not jitter.
2. **At apex**, if the landing spot is further than 60 m from the eye, cut
   to **Landing**: eye = `landing - dir*28 + side*14`, 3 m up (raised by
   the same clearance rules), pointing at the landing spot. The cut happens
   a beat *before* the ball arrives, so the player sees it come into frame.
   For a ball that will land within 60 m, Track continues instead.
3. **Landing -> Roll**: stay put; pan with the ball. When the ball slows
   below 3 m/s and leaves the frame's middle 60%, glide the eye (not cut)
   along the ball's heading by up to 12 m.
4. **Rest**: hold 0.8 s on the ball with the lie shown in the info box,
   then cut to the next Address.

Shots that do not need drama (a putt rolling 3 m) never cut: Track only.
Every rule has an upper bound on camera travel per second so replays and
tests can assert "no teleport inside a shot".

### 4.5 Putting

Green back (approach) and Cup (holing out) as above. At address, the Putt
camera sits behind the ball on the ball-cup line. **Green grid**: a 1.5 m
lattice of short arrows over the green polygon, length and colour from the
elevation gradient (blue downhill ... red uphill), drawn as an overlay into
the frame (depth-tested, fading with distance). It is computed once per
green from `course.elevation` and cached.

### 4.6 Rendering the ball and overlays

`render::Overlay { ball, shadow, trail, aim_marker, grid }` is passed to
`RenderJob::new` and drawn in the objects phase, depth-tested against the
frame, so the ball goes behind hills and trees. The ball is white
(palette index in the reserved 192-255 block), 1-3 px by distance, with a
dark shadow dot projected onto `ground_at`. A short fading trail of the
last N samples is optional (a setting). During flight the camera moves
every frame, so every frame is a full Fly render; in **authentic** mode a
cut starts a progressive repaint and the ball is drawn from the first
step, which gives the cut-and-repaint rhythm for free.

## 5. Shot mechanics

**Clubs** (`play/clubs.rs`, a table): Driver, 3W, 5W, 3I-9I, PW, SW, Putter.
Per club: loft, full-swing carry for the default player, launch/spin
(backspin rpm), accuracy spread, minimum lie quality. Defaults come from
typical amateur numbers (driver ~200 m carry, 7I ~130 m, PW ~90 m); the
`caddy` suggests the club whose full carry is just over the distance, then
adjusted for wind, elevation difference and lie.

**Swing meter** (`play/meter.rs`, classic three clicks, one key, `Space`):
1. click: swing starts, the cursor sweeps up the power bar;
2. click: sets power (0-100%; past 100 is "overswing", loses accuracy);
3. click: the cursor sweeps back down; distance of the click from the
   *snap line* gives the accuracy error: early = hook, late = slice.

Mouse alternative (Microsoft Golf style, later): drag back and push
forward; speed = power, path = accuracy. Putting uses power only (one
sweep), with the distance the bar represents shown in metres.

**Ball flight** (`play/ball.rs`): semi-implicit Euler at 240 Hz with
gravity, quadratic drag, Magnus lift from backspin and sidespin curve,
wind as a velocity offset on the air (`play/wind.rs`: per-hole speed and
direction derived from the course seed, gusts ramp slowly). Inputs:
launch speed, launch angle (loft + lie), face angle (accuracy), spin.

**Surfaces** (`play/surface.rs`, table keyed by `Material`, applied at
each contact with the heightfield's normal from central differences):

| Material | Restitution | Roll friction | Notes |
|---|---|---|---|
| Green | 0.35 | low | backspin can check / pull back |
| Fringe, Tee, Fairway | 0.40-0.45 | low-mid | |
| FirstCut, Rough | 0.25 | high | grabs; hard to launch from |
| DeepRough, Woodland | 0.15 | very high | heavy power penalty |
| Sand, Waste | 0.05 | plugs | explosion shot needs the wedge |
| Water | - | - | splash, penalty drop |
| CartPath | 0.6 | very low | bounces and runs |
| OutOfBounds | - | - | stroke and distance |
| Rock | 0.7 | low | unpredictable deflection |

Slope accelerates rolling by `g * sin(angle)`: the green break and
fairway kicks come from the same elevation field, so nothing extra is
authored.

**Trees**: billboards have no volume, so a ball is tested against a
vertical cylinder (trunk radius from species, height from `Object.height`);
a hit in the canopy removes most of the speed and adds a random
deflection, a hit on the trunk kills it and drops it at the base.

**Holing out**: within 0.054 m of the pin and slower than ~1.6 m/s the
ball drops; faster, it lips out (deflects). A "gimme" inside 0.5 m is a
setting.

**Lies** (`play/lie.rs`): from the material under the ball (and slope):
power factor, spin factor, extra dispersion, and the clubs allowed. Ball
on a steep slope adds a stance penalty (sidehill = lateral push).

**Rules** (`play/rules.rs`): water = one stroke, drop at the entry point
(relief within two club lengths, no closer); OB = stroke and distance
(replay from previous lie, +1); unplayable in woodland = +1 and a drop;
max 10 strokes per hole (pick up) to avoid infinite loops for beginners.

## 6. Scoring and players

- `Round { holes: [HoleScore; 18], player }` with strokes, putts, penalties,
  fairway-in-regulation, GIR; the scorecard uses the data `gen::card`
  already computes (par, stroke index, yardage).
- 1 player, stroke play first. Hot-seat 2-4 players and match play next:
  the director already takes a `ball` and a `lie`, nothing in it is
  single-player.
- AI player (also the "play for me" and test driver): picks a club with the
  caddy and aims at the centre of the safest landing zone (reuse the
  `landing_zones` the par solver produced), swings through the same meter
  model with noise from a skill number.
- Result text: Albatross, Eagle, Birdie, Par, Bogey, ... shown at the
  hole summary.

## 7. HUD additions (`hud/` split from `hud.rs`)

Win95 style like today. Info box: hole, par, stroke, distance to pin (m or
yd, setting), club, lie, wind arrow and speed, elevation delta. Swing
meter along the bottom (`fill_rect_linear`, snap line, 2 px cursor).
Minimap gains the ball, aim line and the shot's planned carry. Scorecard
overlay (`Tab`). Existing debug panel (fps, flight speed) moves behind `F3`.

Controls (defaults):

| Key | Action |
|---|---|
| Left / Right | rotate aim |
| Up / Down | club up / down (caddy suggestion is the default) |
| Space | swing meter click (start / power / accuracy) |
| Tab | scorecard |
| V | cycle the overview camera / green grid |
| F | free camera (practice) |
| Enter / Esc | skip flyover or replay / menu |
| R | replay last shot from a different camera |

## 8. Code layout (every file under 500 lines)

```
xui-app/crates/golf/src/
  play/
    mod.rs        Play: the state machine and step()
    state.rs      State, Input, events
    clubs.rs  lie.rs  surface.rs  wind.rs  meter.rs  caddy.rs
    ball.rs       integrator, contacts, trees, holing out
    rules.rs      penalties, drops
    score.rs      Round, HoleScore, result names
    ai.rs         the simulated player
    cam/
      mod.rs      Director, Shot, damping
      address.rs  placement + clearance (section 4.3)
      track.rs    tracking and landing cut (4.4)
      green.rs    putt, green back, cup, grid
  render/overlay.rs   Overlay and its drawing in the objects phase
  hud/{mod,info,meter,scorecard,map}.rs   (hud.rs becomes a directory)
  game.rs        Running keeps loading + render pacing; its rig is
                 enum Rig { Fly(Flyer), Play(Play) }
```

Gameplay never touches Messenger or the OS, so no MIDL and no kernel
tests; the crate is host-testable and the QEMU session is a smoke test.
Per AGENTS.md "Every application must be launchable from both front
ends": LazyGolf already is; no new switch is needed unless a settings
option (units) lands in confd later.

## 9. Milestones

Each ends with a screenshot Read by a person, not only a passing test.

| # | Milestone | Done when |
|---|---|---|
| G0 (**done**) | `Overlay` + ball/shadow drawn depth-tested; `play::ball` integrates a shot | host tests: every club carries its table distance +/-5% on flat ground; `golf-ball-flight.png` (ball and trail over a generated tee) |
| G1 (**done**) | Address camera (4.3) with clearance and trees; aim rotation; caddy club; `Play` at Address | invariant tests over 20 seeds x 18 holes: ball and aim in frame, eye above ground+1.2, no eye inside a trunk; snapshot of every tee |
| G2 | Meter and one shot end to end on the tee: swing, flight, Track camera, rest | session script plays hole 1 stroke 1; markers `GOLF:SHOT`, `GOLF:LAND:<material>` |
| G3 | Landing/roll cuts, surfaces table, bounce and roll on slopes, lies | tests: energy never increases, a ball stops, sand plugs, water ends in a penalty; snapshots of the cut |
| G4 | Short game: wedges, chip, fringe, Green back camera, putting with grid and holing out | tests: a putt below the speed limit drops, above lips out; green-back framing invariants; green grid snapshot |
| G5 | Rules, scoring, hole summary, scorecard, 18-hole round | AI plays a full round deterministically for a seed (`GOLF:ROUND:<seed>:<strokes>`) in a host test |
| G6 | Preview flyover, replay, authentic cut-and-repaint polish, wind | session script of a whole hole; fps stays above 20 at 720p in QEMU |
| G7 | Hot-seat, match play, settings (units, mouse swing) | scoped separately |

## 10. Testing

Host-first (`cargo test --manifest-path xui-app/Cargo.toml -p xui-golf`):

- **Determinism**: same seed, club, power, accuracy, wind -> byte-equal
  sample list. The AI round is a regression oracle.
- **Physics properties**: speed after a bounce <= speed before; total
  energy never rises; a ball always comes to rest within N seconds; carry
  grows monotonically with power; wind pushes downwind.
- **Camera invariants** over a sweep of seeds, holes and clubs, per sample
  of a recorded shot: ball inside the frame's middle 80% during Track;
  eye >= ground + 1.2 m; eye never inside an object; per-tick eye motion
  within the shot's bound (no teleport inside a shot, only at cuts); a cut
  lands the ball in frame within 0.5 s.
- **Snapshots** (as `tests/render.rs`, into `xui-app/target/snapshots`):
  address on every tee, mid-flight, landing cut, green back, putt with
  grid. Read the PNGs.
- **Fuzz**: random inputs to the state machine never panic or deadlock a
  hole (stuck states are the classic golf-game bug).

QEMU smoke test: `tools/screenshot/examples/xui_golf_play.json`
(`LAZYOS_XUI_AUTOSTART=golf`), drives Space/arrow keys through a shot and
a putt; markers `GOLF:SHOT:<hole>:<stroke>:<club>:<carry>`,
`GOLF:LAND:<material>`, `GOLF:HOLED:<hole>:<strokes>`, judged with
`pngstats.py` like the existing `xui_golf.json`.

## 10a. Status

**G0** is in (`src/play/{ball,clubs}.rs`, `src/render/overlay.rs`,
`tests/play.rs`). Where it departs from the plan above:

- The `Rig { Fly, Play }` split is deferred to G1: with one variant it
  would be a placeholder. `Running` only gained an `Overlay` and
  `set_ball`, which both render jobs draw.
- The overlay is drawn in the job's final (objects) phase. In authentic
  mode that is after the last chunk, so a cut's ball appears when the
  repaint finishes, not from its first step as 4.6 says; revisit when
  the cut is built (G3).
- Club ball speeds were bisected against the flight model, not taken
  from real launch-monitor data; the table (`carry_m`) is the contract
  and `every_club_carries_its_table_distance` holds it.
- The model carries only: a flight ends at first ground contact. Bounce
  and roll are G3.

**G1** is in (`src/play/{mod,caddy}.rs`, `src/play/cam/`, `src/game/`,
`tests/camera.rs`). The game now opens on the shot camera at the first
tee; Left/Right (or A/D) turn the aim, Up/Down/W/S and the wheel change
club, `F` toggles the free camera, `N`/`P`/`Home` pick the hole. 720 shots
(5 courses x 18 holes x 4 lies x 2 aspect ratios) hold every invariant:
ball and aim in the picture, eye >= 1.2 m over the ground and outside
every tree, the ball in the lower half, and 0.1% blind. Snapshots:
`target/snapshots/golf-address-*.png`. Departures and findings:

- **`Rig` became two fields** (`Running.play` plus `Running.free`), not an
  enum: the free camera is always available to toggle back to, so a sum
  type would only have added a state to keep in sync.
- **The aim marker is at the intended distance, not the club's carry**
  (`Play.distance`): the caddy aims at the pin when it is in reach, and a
  9 m chip with a sand wedge otherwise put the marker 70 m away, on a
  cliff. Stepping to a longer club goes for its full carry; a shorter one
  never lengthens the shot.
- **The eye search is one search over eye position and framing**, ordered
  by cost (lift 1 per m, backing off 4 per stretch, sideways 0.4 per m).
  Choosing a clear spot first and framing second lifted the eye 12 m on
  a ridge to clear a tree and ruined the picture. Heights are measured
  from the higher of the ground there and the ball: on a rise the eye
  sat level with the ball.
- **Horizontal framing is a constraint too**: an eye 3.5 m to the side of
  a ball 4 m ahead turned it out of the picture.
- **Not done yet**: smoothing when the aim turns (the eye can step when a
  tree begins or stops blocking; section 4 wants damped motion inside a
  shot), the pin's flag-top as a second must-be-visible point, and
  the overlay marker colours are the palette's yellow, not tuned.

## 11. Risks and open questions

- **Frame cost while the camera moves**: every flight frame is a full
  render. The scale adaptation already copes (fly benchmark), and Track
  could reuse the Address frame when the camera only pans (reproject the
  ball onto a cached wider frame). Revisit only if G6 shows it.
- **Blind shots**: the clearance solver can fail on dense holes; the
  fallback (aim marker + flag-top marker) must be good enough. Collect the
  failures with a survey test like `routing_survey`.
- **Tree collision approximates billboards**; players will notice
  "through the leaves" cases. Cylinders + deflection is the usual answer.
- **Course generator guarantees**: is a landing zone always reachable
  from the tee on short-grass? `quality` suggests yes; the AI round will
  find the exceptions.
- **Decisions needed** (defaults in brackets): metres or yards [metres,
  yards a setting]; meter style [three-click first, mouse swing later];
  gimme distance [0.5 m]; max strokes per hole [10 then pick up];
  whether the golfer is drawn [a 4-frame sprite at G6, none before].
