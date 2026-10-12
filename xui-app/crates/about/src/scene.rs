//! The 3D scene: a spinning model with a chrome-and-iridescence shader over
//! the synthwave backdrop. `render` draws one whole frame into a
//! [`Framebuffer`]; the window only feeds it time and the pointer.

use std::f32::consts::TAU;

use crate::backdrop::{Backdrop, Eye};
use crate::math::{Mat3, Vec3};
use crate::mesh::{Mesh, Shape};
use crate::raster::{Framebuffer, ScreenVertex};

/// How the model is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Solid,
    SolidWire,
    Wire,
}

impl Style {
    pub fn next(self) -> Style {
        match self {
            Style::Solid => Style::SolidWire,
            Style::SolidWire => Style::Wire,
            Style::Wire => Style::Solid,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Style::Solid => "solid",
            Style::SolidWire => "solid + wire",
            Style::Wire => "wireframe",
        }
    }
}

/// The part of the framebuffer the model and its set are centred in (the
/// rest is covered by the text panel).
#[derive(Clone, Copy, Debug)]
pub struct Area {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

const NEAR: f32 = 0.2;
const SPIN: f32 = 0.55;
const REST_PITCH: f32 = 0.32;
pub const MIN_DIST: f32 = 3.6;
pub const MAX_DIST: f32 = 9.5;
const LIGHT: Vec3 = Vec3::new(-0.45, 0.62, -0.64);
const FILL: Vec3 = Vec3::new(0.85, -0.15, -0.5);
const SUN: Vec3 = Vec3::new(0.0, 0.1, 0.99);

pub struct Scene {
    shape: Shape,
    mesh: Mesh,
    edges: Vec<[u32; 2]>,
    pub style: Style,
    yaw: f32,
    pitch: f32,
    yaw_velocity: f32,
    pub dist: f32,
    grabbed: bool,
    time: f32,
    last_dt: f32,
    backdrop: Backdrop,
    palette: Vec<[f32; 3]>,
    projected: Vec<ScreenVertex>,
}

impl Scene {
    pub fn new() -> Scene {
        let shape = Shape::Knot;
        let mesh = shape.build();
        Scene {
            shape,
            edges: mesh.edges(),
            mesh,
            style: Style::Solid,
            yaw: 0.6,
            pitch: REST_PITCH,
            yaw_velocity: SPIN,
            dist: 5.6,
            grabbed: false,
            time: 0.0,
            last_dt: 0.016,
            backdrop: Backdrop::new(),
            palette: palette(),
            projected: Vec::new(),
        }
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    pub fn triangles(&self) -> usize {
        self.mesh.triangles.len()
    }

    pub fn set_shape(&mut self, shape: Shape) {
        if shape != self.shape {
            self.shape = shape;
            self.mesh = shape.build();
            self.edges = self.mesh.edges();
        }
    }

    pub fn time(&self) -> f32 {
        self.time
    }

    /// Moves time on by `dt` seconds: the spin eases back to its own pace
    /// after a fling, and the tilt settles.
    pub fn advance(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        self.time += dt;
        self.last_dt = dt.max(0.004);
        if self.grabbed {
            return;
        }
        self.yaw += self.yaw_velocity * dt;
        let ease = 1.0 - (-1.6 * dt).exp();
        self.yaw_velocity += (SPIN - self.yaw_velocity) * ease;
        self.pitch += (REST_PITCH - self.pitch) * (1.0 - (-0.9 * dt).exp());
        self.yaw = self.yaw.rem_euclid(TAU);
    }

    pub fn grab(&mut self) {
        self.grabbed = true;
    }

    pub fn release(&mut self) {
        self.grabbed = false;
    }

    /// A drag of `dx`, `dy` pixels turns the model with the pointer.
    pub fn drag(&mut self, dx: f32, dy: f32) {
        let turn = dx * 0.011;
        self.yaw += turn;
        self.yaw_velocity = (turn / self.last_dt).clamp(-9.0, 9.0);
        self.pitch = (self.pitch + dy * 0.011).clamp(-1.35, 1.35);
    }

    pub fn zoom(&mut self, notches: f32) {
        self.dist = (self.dist - notches * 0.45).clamp(MIN_DIST, MAX_DIST);
    }

    fn rotation(&self) -> Mat3 {
        let wobble = 0.12 * (self.time * 0.7).sin();
        let turn = Mat3::yaw(self.yaw).then_after(&Mat3::roll(wobble));
        Mat3::pitch(self.pitch).then_after(&turn)
    }

    /// The camera for an `area` of the framebuffer.
    pub fn eye(&self, area: Area) -> Eye {
        let height = area.y1 - area.y0;
        Eye {
            cx: (area.x0 + area.x1) * 0.5,
            horizon: area.y0 + height * 0.6,
            focal: height * 0.95,
            dist: self.dist,
        }
    }

    /// Draws the whole frame.
    pub fn render(&mut self, fb: &mut Framebuffer, area: Area) {
        let eye = self.eye(area);
        self.backdrop.paint(fb, &eye, self.time);
        if self.style != Style::Wire {
            self.backdrop.shadow(fb, &eye, 1.25);
        }
        fb.clear_depth();
        self.project(&eye);
        if self.style != Style::Wire {
            self.fill(fb);
        }
        if self.style != Style::Solid {
            self.wire(fb);
        }
        vignette(fb);
    }

    /// The mesh into pixels: rotated, moved out to `dist`, projected.
    fn project(&mut self, eye: &Eye) {
        let turn = self.rotation();
        let centre = Vec3::new(0.0, 0.0, self.dist);
        self.projected.clear();
        for (p, n) in self.mesh.positions.iter().zip(&self.mesh.normals) {
            let view = turn.apply(*p) + centre;
            let inv_z = 1.0 / view.z.max(NEAR);
            self.projected.push(ScreenVertex {
                x: eye.cx + view.x * inv_z * eye.focal,
                y: eye.horizon - view.y * inv_z * eye.focal,
                inv_z,
                position: view,
                normal: turn.apply(*n),
            });
        }
    }

    fn fill(&self, fb: &mut Framebuffer) {
        let time = self.time;
        let palette = &self.palette;
        for t in &self.mesh.triangles {
            let v = [
                self.projected[t[0] as usize],
                self.projected[t[1] as usize],
                self.projected[t[2] as usize],
            ];
            if v.iter().any(|p| p.position.z < NEAR * 2.0) {
                continue;
            }
            fb.triangle(&v, |p, n| shade(palette, time, p, n));
        }
    }

    fn wire(&self, fb: &mut Framebuffer) {
        let solid = self.style == Style::SolidWire;
        for [a, b] in &self.edges {
            let (a, b) = (&self.projected[*a as usize], &self.projected[*b as usize]);
            if a.position.z < NEAR * 2.0 || b.position.z < NEAR * 2.0 {
                continue;
            }
            // Nearer edges burn brighter, which reads as depth in wire-only.
            let near = (1.0 - (a.position.z - self.dist + 1.8) / 3.6).clamp(0.15, 1.0);
            let k = if solid { 0.55 } else { 0.5 + 0.5 * near };
            fb.line(a, b, solid, |_| {
                [(40.0 * k) as u8, (230.0 * k) as u8, (255.0 * k) as u8]
            });
        }
    }
}

impl Default for Scene {
    fn default() -> Self {
        Scene::new()
    }
}

/// A cosine palette that walks cyan, violet, magenta and gold: the base
/// colour the chrome is tinted with.
fn palette() -> Vec<[f32; 3]> {
    (0..256)
        .map(|i| {
            let t = i as f32 / 255.0;
            let ch = |a: f32, b: f32, d: f32| (a + b * (TAU * (t + d)).cos()).clamp(0.0, 1.0);
            [
                ch(0.55, 0.45, 0.0),
                ch(0.45, 0.45, 0.33),
                ch(0.6, 0.4, 0.67),
            ]
        })
        .collect()
}

/// What a ray bouncing off the model sees: the same sky, sun and floor glow
/// the backdrop paints, so the chrome appears to reflect the scene.
fn environment(r: Vec3) -> [f32; 3] {
    let h = r.y;
    let mut c = if h >= 0.0 {
        let t = h.sqrt();
        let (low, high) = ([255.0, 80.0, 160.0], [18.0, 10.0, 70.0]);
        [
            low[0] + (high[0] - low[0]) * t,
            low[1] + (high[1] - low[1]) * t,
            low[2] + (high[2] - low[2]) * t,
        ]
    } else {
        let t = (-h).sqrt();
        let (low, deep) = ([160.0, 24.0, 120.0], [14.0, 4.0, 40.0]);
        [
            low[0] + (deep[0] - low[0]) * t,
            low[1] + (deep[1] - low[1]) * t,
            low[2] + (deep[2] - low[2]) * t,
        ]
    };
    // A neon band right at the horizon.
    let band = 1.0 / (1.0 + h * h * 900.0);
    c[0] += 60.0 * band;
    c[1] += 190.0 * band;
    c[2] += 255.0 * band;
    // The sun, big and bright.
    let s = r.dot(SUN).max(0.0);
    let s2 = s * s;
    let s4 = s2 * s2;
    let s8 = s4 * s4;
    let s16 = s8 * s8;
    let sun = s16 * s16 * s8;
    c[0] += 330.0 * sun;
    c[1] += 250.0 * sun;
    c[2] += 150.0 * sun;
    c
}

/// The colour of a surface point `p` (view space) with unit normal `n`.
fn shade(palette: &[[f32; 3]], time: f32, p: Vec3, n: Vec3) -> [u8; 3] {
    // The depth test leaves only surfaces that face the camera; a normal
    // interpolated past the silhouette is clamped, never flipped (a flip
    // paints a ring of lit speckles).
    let v = (-p).normalized();
    let ndv = n.dot(v).clamp(0.0, 1.0);
    let fres = (1.0 - ndv) * (1.0 - ndv) * (1.0 - ndv);

    let hue = 0.35 * ndv + 0.3 * n.y + 0.12 * n.x + time * 0.05;
    let base = palette[((hue.rem_euclid(1.0)) * 255.0) as usize];

    let key = n.dot(LIGHT).max(0.0);
    let fill = n.dot(FILL).max(0.0);
    let light = 0.16 + 0.78 * key;
    let reflection = environment((-v).reflect(n));
    let mirror = 0.28 + 0.72 * fres;

    let h = (LIGHT + v).normalized();
    let s = n.dot(h).max(0.0);
    let s2 = s * s;
    let s4 = s2 * s2;
    let s8 = s4 * s4;
    let spec = s8 * s8 * s8 * 0.9;

    let mut out = [0.0_f32; 3];
    let fill_tint = [255.0, 40.0, 200.0];
    let rim_tint = [0.0, 200.0, 255.0];
    for k in 0..3 {
        out[k] = base[k] * 255.0 * light * 0.62
            + reflection[k] * mirror
            + fill_tint[k] * fill * 0.18
            + rim_tint[k] * fres * fres * 0.7
            + 255.0 * spec;
    }
    // A soft shoulder instead of a hard clip, so the sun glint blooms.
    for c in &mut out {
        *c = if *c > 190.0 {
            190.0 + (*c - 190.0) * 0.35
        } else {
            *c
        };
    }
    [
        out[0].clamp(0.0, 255.0) as u8,
        out[1].clamp(0.0, 255.0) as u8,
        out[2].clamp(0.0, 255.0) as u8,
    ]
}

/// Darkens the corners.
fn vignette(fb: &mut Framebuffer) {
    let (w, h) = (fb.width(), fb.height());
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    for y in 0..h {
        let dy = (y as f32 - cy) / cy;
        for x in 0..w {
            let dx = (x as f32 - cx) / cx;
            let k = 1.0 - 0.28 * (dx * dx + dy * dy) * 0.5;
            let [r, g, b] = fb.get(x, y);
            fb.set(
                x,
                y,
                [
                    (r as f32 * k) as u8,
                    (g as f32 * k) as u8,
                    (b as f32 * k) as u8,
                ],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(scene: &mut Scene) -> Framebuffer {
        let mut fb = Framebuffer::new(160, 100, Vec::new());
        let area = Area {
            x0: 0.0,
            y0: 0.0,
            x1: 160.0,
            y1: 100.0,
        };
        scene.render(&mut fb, area);
        fb
    }

    #[test]
    fn every_shape_and_style_renders_something_lit() {
        let mut scene = Scene::new();
        for shape in Shape::ALL {
            scene.set_shape(shape);
            let mut style = scene.style;
            for _ in 0..3 {
                scene.style = style;
                let fb = frame(&mut scene);
                let lit = fb
                    .rgba
                    .chunks(4)
                    .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 60)
                    .count();
                assert!(lit > 800, "{shape:?} {style:?}: {lit} lit pixels");
                style = style.next();
            }
        }
    }

    #[test]
    fn wire_over_solid_draws_the_visible_edges() {
        let mut scene = Scene::new();
        scene.set_shape(Shape::Torus);
        let area = Area {
            x0: 0.0,
            y0: 0.0,
            x1: 320.0,
            y1: 200.0,
        };
        let mut fb = Framebuffer::new(320, 200, Vec::new());
        scene.render(&mut fb, area);
        let solid = fb.rgba.clone();
        scene.style = Style::SolidWire;
        scene.render(&mut fb, area);
        let changed = solid
            .chunks(4)
            .zip(fb.rgba.chunks(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 3000, "only {changed} pixels gained a wire");
    }

    #[test]
    fn the_model_changes_as_it_spins() {
        let mut scene = Scene::new();
        let a = frame(&mut scene).rgba;
        scene.advance(0.4);
        let b = frame(&mut scene).rgba;
        assert_ne!(a, b);
    }

    #[test]
    fn dragging_turns_and_releasing_hands_back_to_the_auto_spin() {
        let mut scene = Scene::new();
        let before = scene.yaw;
        scene.grab();
        scene.drag(50.0, 0.0);
        assert!(scene.yaw > before);
        scene.release();
        for _ in 0..600 {
            scene.advance(0.016);
        }
        assert!((scene.yaw_velocity - SPIN).abs() < 0.05);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut scene = Scene::new();
        scene.zoom(100.0);
        assert_eq!(scene.dist, MIN_DIST);
        scene.zoom(-100.0);
        assert_eq!(scene.dist, MAX_DIST);
    }
}
