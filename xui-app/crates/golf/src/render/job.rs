//! The resumable render job (docs/golf-course-generator.md, Part II §9):
//! sky first, then the visible chunks, then the objects, inside a time or
//! chunk budget per step. In *fly* mode chunks go nearest first (the depth
//! test rejects hidden pixels before they are shaded) and a frame finishes
//! in one step; in *authentic* mode they go farthest first, a couple per
//! step, so the course paints itself in the way it did on a 486.

use std::time::{Duration, Instant};

use crate::math::Vec3;

use super::bake::CHUNK;
use super::camera::{Camera, View};
use super::objects;
use super::raster::Frame;
use super::terrain::{draw_chunk, lod, Scratch, Shading};
use super::Scene;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Whole frames, nearest chunks first.
    Fly,
    /// Farthest first, `chunks` per step.
    Authentic { chunks: usize },
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Sky,
    Chunks,
    Objects,
    Done,
}

/// One view being rendered.
pub struct RenderJob {
    view: View,
    mode: Mode,
    phase: Phase,
    /// Visible chunks with their mesh steps, in drawing order.
    order: Vec<(usize, usize)>,
    next: usize,
    /// Objects to draw after the terrain (fly mode), farthest first.
    pending: Vec<(f32, u32)>,
    scratch: Scratch,
    time: f32,
}

impl RenderJob {
    pub fn new(
        scene: &Scene,
        camera: &Camera,
        width: usize,
        height: usize,
        mode: Mode,
        time: f32,
    ) -> RenderJob {
        let view = camera.view(width, height);
        let mut visible: Vec<(f32, usize, usize)> = Vec::new();
        let reach = view.far + CHUNK as f32;
        for (i, chunk) in scene.bake.chunks.iter().enumerate() {
            let (x0, z0) = (chunk.x0 as f32, chunk.z0 as f32);
            let center = Vec3::new(
                x0 + CHUNK as f32 * 0.5,
                (chunk.min_h + chunk.max_h) * 0.5,
                z0 + CHUNK as f32 * 0.5,
            );
            let d = (center - view.eye).length();
            if d > reach {
                continue;
            }
            let lo = Vec3::new(x0, chunk.min_h, z0);
            let hi = Vec3::new(
                x0 + CHUNK as f32 + 1.0,
                chunk.max_h + 1.0,
                z0 + CHUNK as f32 + 1.0,
            );
            if !view.sees_box(lo, hi) {
                continue;
            }
            // The level of detail follows the distance to the nearest point
            // of the chunk, so the chunk underfoot is always full detail.
            let nearest = Vec3::new(
                view.eye.x.clamp(lo.x, hi.x),
                view.eye.y.clamp(lo.y, hi.y),
                view.eye.z.clamp(lo.z, hi.z),
            );
            visible.push((d, i, lod((nearest - view.eye).length())));
        }
        match mode {
            Mode::Fly => visible.sort_by(|a, b| a.0.total_cmp(&b.0)),
            Mode::Authentic { .. } => visible.sort_by(|a, b| b.0.total_cmp(&a.0)),
        }
        RenderJob {
            view,
            mode,
            phase: Phase::Sky,
            order: visible.into_iter().map(|(_, i, s)| (i, s)).collect(),
            next: 0,
            pending: Vec::new(),
            scratch: Scratch::default(),
            time,
        }
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// How many visible chunks the view has.
    pub fn chunks(&self) -> usize {
        self.order.len()
    }

    /// Renders until `budget` runs out (`None`: to the end) or the job is
    /// done. Returns whether it is done.
    pub fn step(&mut self, frame: &mut Frame, scene: &Scene, budget: Option<Duration>) -> bool {
        let deadline = budget.map(|b| Instant::now() + b);
        let shading = Shading::new(&scene.bake, &scene.palette.fog, &self.view);
        let mut chunks_this_step = 0;
        loop {
            match self.phase {
                Phase::Sky => {
                    scene.sky.draw(frame, &self.view, &scene.palette.fog);
                    self.phase = Phase::Chunks;
                }
                Phase::Chunks => {
                    if let Mode::Authentic { chunks } = self.mode {
                        if chunks_this_step >= chunks {
                            return false;
                        }
                    }
                    let Some(&(index, step)) = self.order.get(self.next) else {
                        self.phase = Phase::Objects;
                        continue;
                    };
                    self.next += 1;
                    chunks_this_step += 1;
                    draw_chunk(frame, &self.view, &shading, index, step, &mut self.scratch);
                    let mut here = self.objects_in(scene, index);
                    match self.mode {
                        // Authentic: the chunk's trees pop in right after it.
                        Mode::Authentic { .. } => {
                            here.sort_by(|a, b| b.0.total_cmp(&a.0));
                            for (_, o) in here {
                                objects::draw(
                                    frame,
                                    &self.view,
                                    &shading,
                                    &scene.bake.objects[o as usize],
                                    self.time,
                                );
                            }
                        }
                        Mode::Fly => self.pending.extend(here),
                    }
                }
                Phase::Objects => {
                    // Far to near, so nearer sprites cover farther ones.
                    self.pending.sort_by(|a, b| b.0.total_cmp(&a.0));
                    for &(_, o) in &self.pending {
                        objects::draw(
                            frame,
                            &self.view,
                            &shading,
                            &scene.bake.objects[o as usize],
                            self.time,
                        );
                    }
                    self.pending.clear();
                    self.phase = Phase::Done;
                }
                Phase::Done => return true,
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return self.phase == Phase::Done;
            }
        }
    }

    /// The objects of chunk `index` in front of the camera, with depths.
    fn objects_in(&self, scene: &Scene, index: usize) -> Vec<(f32, u32)> {
        scene.bake.chunks[index]
            .objects
            .iter()
            .filter_map(|&o| {
                objects::depth(&self.view, &scene.bake.objects[o as usize]).map(|z| (z, o))
            })
            .filter(|&(z, _)| z < self.view.far)
            .collect()
    }
}

/// Index buffer to opaque RGBA, nearest-neighbour upscaled by `scale` and
/// cropped to `width` x `height` (the window), ready for `Image::from_rgba`.
pub fn resolve(
    frame: &Frame,
    words: &[u32; 256],
    scale: usize,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 4];
    let mut row = vec![0u8; width * 4];
    for y in 0..height {
        if y % scale == 0 || y == 0 {
            let src_y = (y / scale).min(frame.height - 1);
            let src = &frame.index[src_y * frame.width..(src_y + 1) * frame.width];
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let i = src[(x / scale).min(frame.width - 1)];
                px.copy_from_slice(&words[usize::from(i)].to_le_bytes());
            }
        }
        out[y * width * 4..(y + 1) * width * 4].copy_from_slice(&row);
    }
    out
}
