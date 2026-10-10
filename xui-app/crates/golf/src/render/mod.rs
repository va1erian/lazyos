//! The mid-90s software renderer (docs/golf-course-generator.md, Part II):
//! an indexed framebuffer filled back to front or front to back by a
//! resumable job, presented through one RGBA image per frame.

pub mod bake;
pub mod camera;
pub mod job;
mod objects;
pub mod palette;
pub mod raster;
pub mod sky;
pub mod sprites;
pub mod terrain;

use crate::course::Course;

pub use bake::Bake;
pub use camera::Camera;
pub use job::{resolve_into, Mode, RenderJob};
pub use palette::Palette;
pub use raster::Frame;

/// A course ready to draw: the course, its bake, the sky and the palette.
pub struct Scene {
    pub course: Course,
    pub bake: Bake,
    pub sky: sky::Sky,
    pub palette: Palette,
}

impl Scene {
    pub fn new(course: Course) -> Scene {
        let bake = Bake::new(&course);
        let sky = sky::Sky::new(course.seed);
        Scene {
            course,
            bake,
            sky,
            palette: Palette::new(),
        }
    }

    /// Renders one whole frame (`width` x `height` indices) from `camera`.
    pub fn render(&self, camera: &Camera, frame: &mut Frame, time: f32) {
        let mut job = RenderJob::new(self, camera, frame.width, frame.height, Mode::Fly, time);
        job.step(frame, self, None);
    }
}
