//! The open document as the window shows it: the layout at the current
//! zoom, the scroll position, the tile cache, and which tiles to ask the
//! render threads for next. No widgets: the page view paints from it and the
//! app drives it, and tests drive it directly.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use lazypdf::{Document, PageSize};
use xui_core::Image;

use crate::cache::{Key, TileCache};
use crate::host::LogFn;
use crate::layout::{self, Layout, PxRect, Zoom};
use crate::worker::{Job, Pool};

/// The longest side of a page's preview, in pixels.
const PREVIEW_SIDE: f32 = 360.0;
/// The cache never goes below this, whatever the window size.
const MIN_BUDGET: usize = 32 << 20;
/// Screens' worth of tiles the cache holds.
const SCREENS: usize = 6;

/// What the window shows.
pub struct Viewer {
    doc: Option<Arc<Document>>,
    pool: Option<Pool>,
    pub pages: Vec<PageSize>,
    pub zoom: Zoom,
    pub layout: Layout,
    /// The viewport's top-left corner in document pixels.
    pub offset: (i32, i32),
    /// The viewport's size in device pixels.
    pub view: (i32, i32),
    pub dpi: u32,
    pub cache: TileCache,
    /// Shown instead of pages: why the last open failed.
    pub error: Option<String>,
    log: Rc<LogFn>,
    threads: usize,
    /// Pages whose visible tiles at this scale have all arrived (reported once).
    drawn: HashSet<usize>,
    /// When each page was first asked for at this scale.
    asked: HashMap<usize, Instant>,
    /// Render-thread time spent on each page's tiles at this scale.
    spent: HashMap<usize, f32>,
}

impl Viewer {
    pub fn new(log: Rc<LogFn>, threads: usize) -> Viewer {
        Viewer {
            doc: None,
            pool: None,
            pages: Vec::new(),
            zoom: Zoom::FitWidth,
            layout: Layout::default(),
            offset: (0, 0),
            view: (0, 0),
            dpi: 96,
            cache: TileCache::new(MIN_BUDGET),
            error: None,
            log,
            threads,
            drawn: HashSet::new(),
            asked: HashMap::new(),
            spent: HashMap::new(),
        }
    }

    /// Shows `doc` from its first page, at fit width.
    pub fn open(&mut self, doc: Document) {
        let doc = Arc::new(doc);
        self.pages = (0..doc.page_count())
            .filter_map(|i| doc.page_size(i))
            .collect();
        self.pool = Some(Pool::new(Arc::clone(&doc), self.threads));
        self.doc = Some(doc);
        self.error = None;
        self.zoom = Zoom::FitWidth;
        self.offset = (0, 0);
        self.reset_tiles();
        self.relayout(None);
    }

    /// Shows `message` instead of a document.
    pub fn fail(&mut self, message: String) {
        self.doc = None;
        self.pool = None;
        self.pages.clear();
        self.layout = Layout::default();
        self.offset = (0, 0);
        self.reset_tiles();
        self.error = Some(message);
    }

    pub fn document(&self) -> Option<&Document> {
        self.doc.as_deref()
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// The viewport changed size (or DPI): lay out again around the same spot.
    pub fn set_view(&mut self, width: i32, height: i32, dpi: u32) {
        if (width, height, dpi) == (self.view.0, self.view.1, self.dpi) {
            return;
        }
        let anchor = self.anchor();
        self.view = (width.max(0), height.max(0));
        self.dpi = dpi.max(1);
        let screen = self.view.0.max(1) as usize * self.view.1.max(1) as usize * 4;
        self.cache = TileCache::new((screen * SCREENS).max(MIN_BUDGET));
        self.reset_tiles();
        self.relayout(anchor);
    }

    pub fn set_zoom(&mut self, zoom: Zoom) {
        let anchor = self.anchor();
        self.zoom = zoom;
        self.relayout(anchor);
    }

    /// The zoom as a factor of printed size (1.0 = 100%).
    pub fn factor(&self) -> f32 {
        self.layout.scale / (self.dpi as f32 / 72.0)
    }

    /// The page most of the upper viewport shows (0-based).
    pub fn current_page(&self) -> usize {
        self.layout.page_at(self.offset.1 + self.view.1 / 3)
    }

    /// Scrolls by (`dx`, `dy`) pixels, clamped; whether anything moved.
    pub fn scroll_by(&mut self, dx: i32, dy: i32) -> bool {
        self.scroll_to(
            self.offset.0.saturating_add(dx),
            self.offset.1.saturating_add(dy),
        )
    }

    pub fn scroll_to(&mut self, x: i32, y: i32) -> bool {
        let max_x = (self.layout.width - self.view.0).max(0);
        let max_y = (self.layout.height - self.view.1).max(0);
        let next = (x.clamp(0, max_x), y.clamp(0, max_y));
        let moved = next != self.offset;
        self.offset = next;
        moved
    }

    /// Scrolls so page `index` starts at the top of the viewport.
    pub fn go_to_page(&mut self, index: usize) -> bool {
        let Some(rect) = self
            .layout
            .rects
            .get(index.min(self.pages.len().saturating_sub(1)))
        else {
            return false;
        };
        self.scroll_to(self.offset.0, rect.y - layout::gap(self.dpi))
    }

    /// The viewport in document pixels.
    pub fn view_rect(&self) -> PxRect {
        PxRect {
            x: self.offset.0,
            y: self.offset.1,
            w: self.view.0,
            h: self.view.1,
        }
    }

    /// The cached image for `key`.
    pub fn image(&self, key: &Key) -> Option<&Image> {
        self.cache.get(key)
    }

    /// Asks the render threads for what the viewport shows, nearest first:
    /// previews of the visible pages, their tiles, then a half screen above
    /// and below.
    pub fn schedule(&mut self) {
        let Some(pool) = &self.pool else {
            return;
        };
        let view = self.view_rect();
        let ahead = PxRect {
            y: view.y - view.h / 2,
            h: view.h * 2,
            ..view
        };
        let visible = self.layout.visible(view.y, view.h);
        let mut jobs = Vec::new();
        for page in visible.clone() {
            let key = Key::Preview { page: page as u32 };
            if !self.cache.contains(&key) {
                jobs.push(preview_job(page, self.pages[page]));
            }
        }
        let scale = self.layout.scale;
        for (area, pages) in [
            (view, visible),
            (ahead, self.layout.visible(ahead.y, ahead.h)),
        ] {
            // A page's wait is timed from when it is on screen, not prefetched.
            let on_screen = area == view;
            for page in pages {
                for (col, row) in self.layout.tiles(page, area) {
                    let key = Key::tile(page, scale, col, row);
                    if self.cache.contains(&key) || jobs.iter().any(|j: &Job| j.key == key) {
                        continue;
                    }
                    if let Some(r) = self.layout.tile_rect(page, col, row) {
                        if on_screen {
                            self.asked.entry(page).or_insert_with(Instant::now);
                        }
                        jobs.push(Job {
                            key,
                            page,
                            scale,
                            x: r.x as u32,
                            y: r.y as u32,
                            w: r.w as u32,
                            h: r.h as u32,
                        });
                    }
                }
            }
        }
        pool.set_jobs(jobs);
    }

    /// Moves finished tiles into the cache; whether any arrived.
    pub fn drain(&mut self) -> bool {
        let Some(pool) = &self.pool else {
            return false;
        };
        let mut arrived = Vec::new();
        while let Some(done) = pool.try_recv() {
            arrived.push(done);
        }
        if arrived.is_empty() {
            return false;
        }
        let view = self.view_rect();
        let scale = self.layout.scale;
        let visible = self.layout.visible(view.y, view.h);
        for done in arrived {
            if let Key::Tile { scale: s, .. } = done.key {
                if s == scale.to_bits() {
                    *self.spent.entry(done.key.page()).or_default() += done.millis;
                }
            }
            let Some(tile) = done.tile else {
                (self.log)(&format!("PDF:PAGE:FAIL:{}", done.key.page() + 1));
                continue;
            };
            let Ok(image) = Image::from_rgba(tile.width, tile.height, tile.rgba) else {
                continue;
            };
            let keep = |key: &Key| match *key {
                Key::Preview { page } => visible.contains(&(page as usize)),
                Key::Tile { page, scale: s, .. } => {
                    s == scale.to_bits() && visible.contains(&(page as usize))
                }
            };
            self.cache.insert(done.key, image, &keep);
        }
        self.report_drawn();
        true
    }

    /// Whether the render threads still have work.
    pub fn busy(&self) -> bool {
        self.pool.as_ref().is_some_and(Pool::busy)
    }

    /// `PDF:PAGE:DRAWN:<page>:<ms>:<render ms>` once per page and scale,
    /// when every tile the viewport shows of it has arrived: the time since
    /// it was first asked for, and the render threads' time on its tiles.
    fn report_drawn(&mut self) {
        let view = self.view_rect();
        for page in self.layout.visible(view.y, view.h) {
            if self.drawn.contains(&page) {
                continue;
            }
            let scale = self.layout.scale;
            let complete = self
                .layout
                .tiles(page, view)
                .iter()
                .all(|&(col, row)| self.cache.contains(&Key::tile(page, scale, col, row)));
            if complete {
                self.drawn.insert(page);
                // A page prefetched whole before it scrolled in waited 0 ms.
                let ms = self.asked.get(&page).map_or(0, |t| t.elapsed().as_millis());
                let render = self.spent.get(&page).copied().unwrap_or(0.0);
                (self.log)(&format!("PDF:PAGE:DRAWN:{}:{ms}:{render:.0}", page + 1));
            }
        }
    }

    /// Where the viewport is, as a page and a fraction down it, so a new
    /// layout can put it back.
    fn anchor(&self) -> Option<(usize, f32, i32)> {
        if self.layout.rects.is_empty() {
            return None;
        }
        let page = self.layout.page_at(self.offset.1);
        let rect = self.layout.rects[page];
        let fraction = (self.offset.1 - rect.y) as f32 / rect.h.max(1) as f32;
        let centre_x = self.offset.0 + self.view.0 / 2;
        Some((page, fraction, centre_x - self.layout.width / 2))
    }

    fn relayout(&mut self, anchor: Option<(usize, f32, i32)>) {
        let current = anchor.map_or(0, |a| a.0);
        let scale = layout::scale_for(self.zoom, &self.pages, current, self.view, self.dpi);
        if scale != self.layout.scale {
            self.drawn.clear();
            self.asked.clear();
            self.spent.clear();
        }
        self.layout = Layout::new(&self.pages, scale, self.view.0, self.dpi);
        match anchor {
            Some((page, fraction, dx)) if page < self.layout.rects.len() => {
                let rect = self.layout.rects[page];
                let y = rect.y + (fraction * rect.h as f32).round() as i32;
                let x = self.layout.width / 2 + dx - self.view.0 / 2;
                self.scroll_to(x, y);
            }
            _ => {
                let x = (self.layout.width - self.view.0) / 2;
                self.scroll_to(x, 0);
            }
        }
    }

    fn reset_tiles(&mut self) {
        self.cache.clear();
        self.drawn.clear();
        self.asked.clear();
        self.spent.clear();
    }
}

/// The job for page `page`'s preview: the whole page, its longest side
/// [`PREVIEW_SIDE`] pixels.
fn preview_job(page: usize, size: PageSize) -> Job {
    let scale = (PREVIEW_SIDE / size.width.max(size.height).max(1.0)).min(1.0);
    let (w, h) = layout::page_pixels(size, scale);
    Job {
        key: Key::Preview { page: page as u32 },
        page,
        scale,
        x: 0,
        y: 0,
        w: w as u32,
        h: h as u32,
    }
}
