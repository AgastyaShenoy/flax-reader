//! The PDF reader: pages as they are, in one scrolling column, rendered in tiles on background
//! threads by the engine from the pdfreader project (`crates/pdf-engine`, `crates/pdf-render`).
//! Notes and comments in the PDF show as tooltips; a locked PDF asks for its password first.

use super::gfx::*;
use super::reader::ReaderAction;
use super::ui::*;
use crate::ao3::models::{ReadingProgress, WorkMeta};
use crate::pdf::{Annotation, Engine, PageSize};
use crate::reader::theme::{ReaderThemeType, Rgb};
use crate::storage::config::AppConfig;
use pdf_render::{compose, wanted_tiles, Frame, Layout, Look, RenderPool, Style, TileCache, Viewport};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const TOOLBAR_H: f32 = 48.0;
/// Space around and between pages, in DIPs.
const GAP: f32 = 12.0;
/// Zoom steps, as a share of the page's printed size.
const ZOOMS: [f32; 13] = [0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0, 3.0, 4.0];

const VK_SPACE: u16 = 0x20;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_ESCAPE: u16 = 0x1B;
const VK_F11: u16 = 0x7A;
const VK_ADD: u16 = 0x6B;
const VK_SUBTRACT: u16 = 0x6D;
const VK_OEM_PLUS: u16 = 0xBB;
const VK_OEM_MINUS: u16 = 0xBD;
const VK_0: u16 = 0x30;
const VK_F: u16 = 0x46;
const VK_G: u16 = 0x47;

/// A line of the PDF's own contents (its outline), flattened.
struct Toc {
    title: String,
    page: usize,
    depth: usize,
}

/// A page the search found, with the words around the first hit.
struct Found {
    page: usize,
    snippet: String,
}

/// A search through the pages' text, run on a thread of its own (big PDFs take a moment).
#[derive(Default)]
struct Search {
    query: String,
    found: Vec<Found>,
    done: bool,
}

fn flatten(items: &[pdf_engine::OutlineItem], depth: usize, out: &mut Vec<Toc>) {
    for it in items {
        if let Some(pdf_engine::Target::Page { page, .. }) = &it.target {
            out.push(Toc { title: it.title.trim().to_string(), page: *page, depth });
        }
        flatten(&it.children, depth + 1, out);
    }
}

/// Up to 200 pages with `query` on them (any case), with the words around the first hit.
fn search_pages(engine: &dyn Engine, query: &str, out: &Mutex<Search>) {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    for page in 0..engine.page_count() {
        let text: String = engine.page_text(page).chars.iter().map(|c| c.ch).collect();
        let lower: Vec<char> = text.to_lowercase().chars().collect();
        if let Some(at) = (0..lower.len().saturating_sub(q.len() - 1)).find(|&i| lower[i..].starts_with(&q)) {
            let chars: Vec<char> = text.chars().collect();
            let from = at.saturating_sub(40);
            let to = (at + q.len() + 60).min(chars.len());
            let mut words: Vec<String> = chars[from..to].iter().collect::<String>().split_whitespace().map(str::to_string).collect();
            // Whole words only: drop the ones cut at either end.
            if from > 0 && words.len() > 3 {
                words.remove(0);
            }
            if to < chars.len() && words.len() > 3 {
                words.pop();
            }
            let snippet = format!("{}{}{}", if from > 0 { "…" } else { "" }, words.join(" "), if to < chars.len() { "…" } else { "" });
            let mut s = out.lock().unwrap_or_else(|e| e.into_inner());
            if s.query != query {
                return; // A newer search took over.
            }
            s.found.push(Found { page, snippet });
            if s.found.len() >= 200 {
                break;
            }
        }
    }
    out.lock().unwrap_or_else(|e| e.into_inner()).done = true;
}

pub struct PdfScreen {
    meta: WorkMeta,
    engine: Arc<dyn Engine>,
    /// The PDF's own contents, if it has any.
    toc: Vec<Toc>,
    /// The contents and search panel is open.
    panel: bool,
    query: String,
    search: Arc<Mutex<Search>>,
    /// "Go to page" being typed.
    goto: Option<String>,
    pool: RenderPool,
    cache: TileCache,
    sizes: Vec<PageSize>,
    /// None fits the pages' width to the window; otherwise the zoom (1.0 = printed size).
    zoom: Option<f32>,
    layout: Option<Layout>,
    /// What the layout was made for: (pixels per point, window width in pixels).
    layout_for: (u32, u32),
    /// Scroll position, in layout pixels (sideways only when pages are wider than the window).
    x: i64,
    y: i64,
    /// Which way the last scroll went: the next screen that way is rendered first.
    ahead: i64,
    /// Where to go once laid out: page + fraction of it.
    restore: Option<f32>,
    anchor: f32,
    /// The visible pages, composed for drawing.
    buf: Vec<u32>,
    /// The scroll bar's thumb is being dragged: where it was grabbed.
    grab: Option<f32>,
    /// Annotations with something to say, by page, for the pages looked at so far.
    notes: HashMap<usize, Vec<Annotation>>,
    pub progress_dirty: bool,
    pub fullscreen: bool,
}

/// 0x00RRGGBB from a colour.
fn rgb(c: Color) -> u32 {
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (to(c.r) << 16) | (to(c.g) << 8) | to(c.b)
}

impl PdfScreen {
    /// `wake` is called from the render threads when pages are ready to show.
    pub fn new(meta: WorkMeta, engine: Arc<dyn Engine>, progress: Option<&ReadingProgress>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let sizes = (0..engine.page_count()).map(|p| engine.page_size(p)).collect();
        let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4));
        let mut toc = Vec::new();
        flatten(&engine.outline(), 0, &mut toc);
        Self {
            meta,
            engine: engine.clone(),
            toc,
            panel: false,
            query: String::new(),
            search: Arc::new(Mutex::new(Search::default())),
            goto: None,
            pool: RenderPool::new(engine, threads, wake),
            cache: TileCache::default(),
            sizes,
            zoom: None,
            layout: None,
            layout_for: (0, 0),
            x: 0,
            y: 0,
            ahead: 1,
            restore: progress.map(|p| p.position),
            anchor: 0.0,
            buf: Vec::new(),
            grab: None,
            notes: HashMap::new(),
            progress_dirty: false,
            fullscreen: false,
        }
    }

    pub fn work_id(&self) -> &str {
        &self.meta.id
    }

    fn page_count(&self) -> usize {
        self.sizes.len()
    }

    pub fn progress(&self) -> ReadingProgress {
        let n = self.page_count().max(1) as f32;
        let completed = self.anchor >= n - 0.05;
        ReadingProgress {
            current_chapter: 1,
            scroll_offset: self.anchor.floor(),
            position: self.anchor,
            percentage: if completed { 100.0 } else { (self.anchor / n * 100.0).clamp(0.0, 100.0) },
            last_read: chrono::Utc::now(),
            completed,
        }
    }

    /// Page and fraction at the top of the window.
    fn anchor_at(layout: &Layout, y: i64) -> f32 {
        if layout.pages.is_empty() {
            return 0.0;
        }
        let p = layout.page_at(y);
        let pos = layout.pages[p];
        p as f32 + ((y - pos.y) as f32 / pos.height.max(1) as f32).clamp(0.0, 0.999)
    }

    fn y_for(layout: &Layout, anchor: f32) -> i64 {
        let Some(last) = layout.pages.len().checked_sub(1) else { return 0 };
        let p = (anchor.max(0.0) as usize).min(last);
        let pos = layout.pages[p];
        pos.y + ((anchor - p as f32).clamp(0.0, 1.0) * pos.height as f32) as i64 - layout.gap as i64
    }

    fn set_zoom(&mut self, zoom: Option<f32>) {
        if let Some(l) = &self.layout {
            self.restore = Some(Self::anchor_at(l, self.y + l.gap as i64));
        }
        self.zoom = zoom.map(|z| z.clamp(ZOOMS[0], ZOOMS[ZOOMS.len() - 1]));
    }

    /// The zoom now, as a share of the printed size (also when fitting the width).
    fn zoom_now(&self, dpi: f32) -> f32 {
        self.zoom.unwrap_or_else(|| self.layout.as_ref().map_or(1.0, |l| l.scale * 72.0 / dpi))
    }

    fn step_zoom(&mut self, dpi: f32, up: bool) {
        let now = self.zoom_now(dpi);
        let next = if up { ZOOMS.iter().copied().find(|z| *z > now + 0.01) } else { ZOOMS.iter().rev().copied().find(|z| *z < now - 0.01) };
        if let Some(z) = next {
            self.set_zoom(Some(z));
        }
    }

    pub fn frame(&mut self, ui: &mut Ui, r: Rect, config: &mut AppConfig) -> Vec<ReaderAction> {
        let mut out = Vec::new();
        let c = ui.c;
        let chrome = !self.fullscreen;
        let top = if chrome { TOOLBAR_H } else { 0.0 };
        let area = Rect::new(r.x, r.y + top, r.w, (r.h - top).max(50.0));
        let px = ui.g.dpi / 96.0;
        let (vw, vh) = ((area.w * px).round().max(1.0) as u32, (area.h * px).round().max(1.0) as u32);

        // Lay the pages out for this zoom and window width.
        let gap = (GAP * px).round() as u32;
        let widest = self.sizes.iter().map(|s| s.width).fold(1.0f32, f32::max);
        let scale = match self.zoom {
            Some(z) => z * ui.g.dpi / 72.0,
            None => ((vw.saturating_sub(2 * gap)) as f32 / widest).clamp(0.05, 20.0),
        };
        if self.layout.is_none() || self.layout_for != (scale.to_bits(), vw) {
            if let (Some(l), None) = (&self.layout, self.restore) {
                self.restore = Some(Self::anchor_at(l, self.y + l.gap as i64));
            }
            self.layout = Some(Layout::new(&self.sizes, scale, gap, vw));
            self.layout_for = (scale.to_bits(), vw);
        }
        let layout = self.layout.as_ref().unwrap();
        if let Some(a) = self.restore.take() {
            self.y = Self::y_for(layout, a);
        }

        self.keys(ui, &mut out, vh as i64, px);
        let panel_r = self.panel_rect(area);
        if ui.hit(area) && ui.input.wheel != 0.0 && !(self.panel && panel_r.contains(ui.input.mouse)) {
            let dy = (-ui.input.wheel * 120.0 * px) as i64;
            self.y += dy;
            self.ahead = dy.signum();
            ui.request_repaint();
        }

        let layout = self.layout.as_ref().unwrap();
        let max_y = (layout.height - vh as i64).max(0);
        let max_x = (layout.width - vw as i64).max(0);
        self.y = self.y.clamp(0, max_y);
        self.x = if max_x == 0 { 0 } else { self.x.clamp(0, max_x) };
        let view = Viewport { x: self.x, y: self.y, width: vw, height: vh };

        // Pages: tiles that are ready, then ask for the ones still missing.
        if ui.g.drawing {
            for done in self.pool.take_finished() {
                self.cache.insert(done.key, done.width, done.height, done.pixels);
            }
            self.cache.set_budget(6 * vw as usize * vh as usize * 4);
            self.cache.begin_frame();
            let pal = config.palette();
            // Black pages on a black background (OLED Black, High Contrast), grey on other dark themes.
            let look = if pal.is_dark && pal.bg == Rgb(0, 0, 0) {
                Look::Black
            } else if pal.is_dark {
                Look::Dark
            } else if config.theme == ReaderThemeType::WarmSepia {
                Look::Sepia
            } else {
                Look::Normal
            };
            self.buf.resize(vw as usize * vh as usize, 0);
            let style = Style { background: rgb(c.bg), border: Some(rgb(c.border)), look };
            let mut frame = Frame { pixels: &mut self.buf, width: vw, height: vh, stride: vw as usize };
            compose(&mut frame, layout, &view, &self.cache, &style);
            self.pool.set_jobs(wanted_tiles(layout, &view, self.ahead, look, &self.cache));
            self.cache.evict();
            ui.g.draw_pixels(&self.buf, vw, vh, area);
        }
        self.scrollbar(ui, area, vh as i64);
        self.note_tips(ui, area, &view, px);

        // Where the reader is, for the library and the toolbar.
        let layout = self.layout.as_ref().unwrap();
        let anchor = if self.y >= max_y && max_y > 0 { self.page_count() as f32 } else { Self::anchor_at(layout, self.y + layout.gap as i64) };
        if (anchor - self.anchor).abs() > 0.001 {
            self.anchor = anchor;
            self.progress_dirty = true;
        }
        let current = layout.current_page(&view);

        if chrome {
            let mark = ui.node_mark();
            self.toolbar(ui, Rect::new(r.x, r.y, r.w, TOOLBAR_H), current, &mut out);
            ui.tab_first(mark);
        }
        if self.panel {
            self.side_panel(ui, panel_r);
        }
        out
    }

    /// A note's text, or the comment on a highlight or drawing, as a tooltip over it.
    fn note_tips(&mut self, ui: &mut Ui, area: Rect, view: &Viewport, px: f32) {
        if self.panel && self.panel_rect(area).contains(ui.input.mouse) {
            return;
        }
        let Some(layout) = &self.layout else { return };
        if layout.pages.is_empty() {
            return;
        }
        let s = layout.scale;
        for page in layout.page_at(view.y)..layout.pages.len() {
            let pos = layout.pages[page];
            if pos.y > view.y + view.height as i64 {
                break;
            }
            let notes = self.notes.entry(page).or_insert_with(|| crate::pdf::page_notes(self.engine.as_ref(), page));
            for (i, n) in notes.iter().enumerate() {
                // points on the page to DIPs in the window, kept inside the pages' area
                let x0 = (area.x + ((pos.x - view.x) as f32 + n.rect[0] * s) / px).max(area.x);
                let y0 = (area.y + ((pos.y - view.y) as f32 + n.rect[1] * s) / px).max(area.y);
                let x1 = (area.x + ((pos.x - view.x) as f32 + n.rect[2] * s) / px).min(area.right());
                let y1 = (area.y + ((pos.y - view.y) as f32 + n.rect[3] * s) / px).min(area.bottom());
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let r = Rect::new(x0, y0, x1 - x0, y1 - y0);
                // Text to a screen reader, a tooltip to the mouse.
                let nid = id2(id2(id("pdf-note"), page), i);
                ui.interact_as(nid, r, Role::Text, &n.contents);
                ui.tip(nid, r, &n.contents);
            }
        }
    }

    fn panel_rect(&self, area: Rect) -> Rect {
        let w = 380.0f32.min(area.w - 24.0);
        Rect::new(area.right() - w - 12.0, area.y + 8.0, w, (area.h - 16.0).max(100.0))
    }

    /// Scrolls to the top of page `page` (0-based).
    fn go_to_page(&mut self, page: usize) {
        if let Some(l) = &self.layout {
            let p = page.min(l.pages.len().saturating_sub(1));
            self.y = l.pages[p].y - l.gap as i64;
            self.ahead = 1;
        }
    }

    /// The PDF's contents, or what a search found: click a line to go there.
    fn side_panel(&mut self, ui: &mut Ui, r: Rect) {
        let c = ui.c;
        ui.block(r);
        ui.g.fill_round(r, 8.0, c.surface);
        ui.g.stroke_round(r, 8.0, 1.0, c.border);
        let input = Rect::new(r.x + 14.0, r.y + 14.0, r.w - 28.0, 34.0);
        let resp = ui.text_input(id("pdf-search"), input, &mut self.query, "Search the pages", false);
        if resp.submitted && self.query.trim().chars().count() >= 2 {
            let q = self.query.trim().to_string();
            *self.search.lock().unwrap_or_else(|e| e.into_inner()) = Search { query: q.clone(), ..Default::default() };
            let (engine, out) = (self.engine.clone(), self.search.clone());
            std::thread::spawn(move || search_pages(engine.as_ref(), &q, &out));
        }
        if self.query.trim().is_empty() {
            self.search.lock().unwrap_or_else(|e| e.into_inner()).query.clear();
        }
        let list = Rect::new(r.x + 4.0, r.y + 58.0, r.w - 8.0, r.h - 62.0);
        let mut go: Option<usize> = None;
        let s = self.search.lock().unwrap_or_else(|e| e.into_inner());
        if !s.query.is_empty() {
            if !s.done {
                ui.wake_after(150);
            }
            let head = if s.done { format!("{} pages with “{}”", s.found.len(), s.query) } else { format!("Searching… {} pages so far", s.found.len()) };
            ui.text(&head, &TextStyle::ui(12.5).semibold(), list.x + 12.0, list.y, c.muted);
            let body = Rect::new(list.x, list.y + 24.0, list.w, list.h - 24.0);
            let found = &s.found;
            ui.scroll_area(id("pdf-found"), body, |ui, view, y0| {
                let row_h = 62.0;
                for (i, f) in found.iter().enumerate() {
                    let rr = Rect::new(view.x + 6.0, y0 + i as f32 * row_h, view.w - 22.0, row_h - 4.0);
                    if rr.bottom() < view.y || rr.y > view.bottom() {
                        continue;
                    }
                    let resp = ui.interact_as(id2(id("pdf-hit"), i), rr, Role::Button, &f.snippet);
                    if resp.hovered {
                        ui.g.fill_round(rr, 6.0, c.hover());
                    }
                    ui.text(&format!("Page {}", f.page + 1), &TextStyle::ui(12.5).semibold(), rr.x + 10.0, rr.y + 6.0, c.accent_text);
                    let t = ui.measure(&f.snippet, &TextStyle::ui(12.5), TextOpts::wrap(rr.w - 20.0).max_lines(2));
                    ui.g.draw_text(t, rr.x + 10.0, rr.y + 26.0, c.text);
                    if resp.clicked {
                        go = Some(f.page);
                    }
                }
                found.len() as f32 * row_h + 8.0
            });
        } else if self.toc.is_empty() {
            let t = ui.measure("This PDF has no contents of its own. Search its pages above, or click the page number to go to a page.", &TextStyle::ui(13.0), TextOpts::wrap(list.w - 40.0).align(Align::Center));
            ui.g.draw_text(t, list.x + 20.0, list.y + 20.0, c.muted);
        } else {
            let toc = &self.toc;
            ui.scroll_area(id("pdf-toc"), list, |ui, view, y0| {
                let row_h = 38.0;
                for (i, t) in toc.iter().enumerate() {
                    let rr = Rect::new(view.x + 6.0, y0 + i as f32 * row_h, view.w - 22.0, row_h - 2.0);
                    if rr.bottom() < view.y || rr.y > view.bottom() {
                        continue;
                    }
                    let resp = ui.interact_as(id2(id("pdf-toc-row"), i), rr, Role::Button, &t.title);
                    if resp.hovered {
                        ui.g.fill_round(rr, 6.0, c.hover());
                    }
                    let indent = t.depth.min(4) as f32 * 16.0;
                    let n = format!("{}", t.page + 1);
                    let nt = ui.text_in(&n, &TextStyle::ui(12.0), Rect::new(rr.right() - 60.0, rr.y, 52.0, rr.h), Align::Right, c.muted);
                    let style = if t.depth == 0 { TextStyle::ui(13.5).semibold() } else { TextStyle::ui(13.5) };
                    ui.text_in(&t.title, &style, Rect::new(rr.x + 10.0 + indent, rr.y, rr.w - nt.w - 30.0 - indent, rr.h), Align::Left, c.text);
                    if resp.clicked {
                        go = Some(t.page);
                    }
                }
                toc.len() as f32 * row_h + 8.0
            });
        }
        drop(s);
        if let Some(p) = go {
            self.go_to_page(p);
            ui.request_repaint();
        }
    }

    fn keys(&mut self, ui: &mut Ui, out: &mut Vec<ReaderAction>, view_h: i64, px: f32) {
        let line = (48.0 * px) as i64;
        let screen = (view_h - line).max(line);
        let page_y = |s: &Self, p: usize| s.layout.as_ref().map_or(0, |l| l.pages[p.min(l.pages.len() - 1)].y - l.gap as i64);
        let current = self.layout.as_ref().map_or(0, |l| l.page_at(self.y + l.gap as i64 + 1));
        let mut dy = 0;
        if ui.key_combo(VK_DOWN, false, false) {
            dy = line;
        } else if ui.key_combo(VK_UP, false, false) {
            dy = -line;
        } else if ui.key_combo(VK_NEXT, false, false) || ui.key_combo(VK_SPACE, false, false) {
            dy = screen;
        } else if ui.key_combo(VK_PRIOR, false, false) || ui.key_combo(VK_SPACE, false, true) {
            dy = -screen;
        }
        if dy != 0 {
            self.y += dy;
            self.ahead = dy.signum();
        }
        if ui.key_combo(VK_RIGHT, false, false) {
            self.y = page_y(self, current + 1);
            self.ahead = 1;
        } else if ui.key_combo(VK_LEFT, false, false) {
            self.y = page_y(self, current.saturating_sub(1));
            self.ahead = -1;
        }
        if ui.key_combo(VK_HOME, false, false) {
            self.y = 0;
        }
        if ui.key_combo(VK_END, false, false) {
            self.y = i64::MAX / 4;
        }
        let dpi = ui.g.dpi;
        if ui.key_combo(VK_OEM_PLUS, true, false) || ui.key_combo(VK_ADD, true, false) {
            self.step_zoom(dpi, true);
        }
        if ui.key_combo(VK_OEM_MINUS, true, false) || ui.key_combo(VK_SUBTRACT, true, false) {
            self.step_zoom(dpi, false);
        }
        if ui.key_combo(VK_0, true, false) {
            self.set_zoom(None);
        }
        // Ctrl+F: contents and search; Ctrl+G: go to a page.
        if ui.key_combo(VK_F, true, false) {
            self.panel = true;
            ui.focus_input(id("pdf-search"));
        }
        if ui.key_combo(VK_G, true, false) {
            self.goto = Some(String::new());
        }
        if self.panel && ui.key_combo(VK_ESCAPE, false, false) {
            self.panel = false;
        }
        if ui.key_combo(VK_F11, false, false) || (self.fullscreen && ui.key_combo(VK_ESCAPE, false, false)) {
            out.push(ReaderAction::ToggleFullscreen);
        }
    }

    /// A thin scroll bar at the right edge; drag the thumb, or press the bar to jump there.
    fn scrollbar(&mut self, ui: &mut Ui, area: Rect, view_h: i64) {
        let Some(layout) = &self.layout else { return };
        let total = layout.height.max(1) as f32;
        let max = (layout.height - view_h).max(0) as f32;
        if max <= 0.0 {
            return;
        }
        let bar = Rect::new(area.right() - 12.0, area.y, 12.0, area.h);
        let thumb_h = (area.h * view_h as f32 / total).max(32.0);
        let thumb_y = area.y + (area.h - thumb_h) * (self.y as f32 / max);
        let resp = ui.interact(id("pdf-bar"), bar);
        if resp.held {
            let m = ui.input.mouse.1;
            let grab = *self.grab.get_or_insert(if (thumb_y..thumb_y + thumb_h).contains(&m) { m - thumb_y } else { thumb_h / 2.0 });
            let t = ((m - grab - area.y) / (area.h - thumb_h).max(1.0)).clamp(0.0, 1.0);
            self.y = (t * max) as i64;
            ui.request_repaint();
        } else {
            self.grab = None;
        }
        let alpha = if resp.held || resp.hovered { 0.5 } else { 0.28 };
        let c = ui.c.text.alpha(alpha);
        ui.g.fill_round(Rect::new(bar.x + 3.0, thumb_y + 2.0, 5.0, thumb_h - 4.0), 2.5, c);
    }

    fn toolbar(&mut self, ui: &mut Ui, r: Rect, current: usize, out: &mut Vec<ReaderAction>) {
        let c = ui.c;
        ui.g.fill_rect(r, c.surface);
        ui.separator(r.x, r.bottom() - 1.0, r.w);
        let b = |x: f32| Rect::new(x, r.y + 8.0, 34.0, 32.0);
        if ui.button_ex(id("pdf-back"), Rect::new(r.x + 8.0, r.y + 8.0, 96.0, 32.0), "Library", Some("\u{E72B}"), ButtonKind::Ghost, true) {
            out.push(ReaderAction::Close);
        }
        let toc_r = Rect::new(r.x + 108.0, r.y + 8.0, 34.0, 32.0);
        if ui.icon_button(id("pdf-contents"), toc_r, "\u{E8FD}", "Contents and search (Ctrl+F)", self.panel) {
            self.panel = !self.panel;
        }

        let mut x = r.right() - 8.0 - 34.0;
        let (fs_icon, fs_tip) = if self.fullscreen { ("\u{E73F}", "Leave full screen (F11)") } else { ("\u{E740}", "Full screen (F11)") };
        if ui.icon_button(id("pdf-full"), b(x), fs_icon, fs_tip, false) {
            out.push(ReaderAction::ToggleFullscreen);
        }
        x -= 38.0;
        let dpi = ui.g.dpi;
        if ui.icon_button(id("pdf-zoom-in"), b(x), "\u{E8A3}", "Zoom in (Ctrl+=)", false) {
            self.step_zoom(dpi, true);
        }
        let zr = Rect::new(x - 64.0, r.y + 8.0, 60.0, 32.0);
        let label = format!("{:.0}%", self.zoom_now(dpi) * 100.0);
        let fit = self.zoom.is_none();
        if ui.button_ex(id("pdf-fit"), zr, &label, None, if fit { ButtonKind::Tonal } else { ButtonKind::Ghost }, true) {
            self.set_zoom(if fit { Some(1.0) } else { None });
        }
        ui.tip(id("pdf-fit"), zr, if fit { "Fitting the width: click for the printed size (Ctrl+0 to fit again)" } else { "Fit the width (Ctrl+0)" });
        x -= 68.0 + 38.0;
        if ui.icon_button(id("pdf-zoom-out"), b(x), "\u{E71F}", "Zoom out (Ctrl+−)", false) {
            self.step_zoom(dpi, false);
        }

        // Title, and where the reader is: click the page number to go to a page.
        let page = format!("Page {} of {}", current + 1, self.page_count());
        let left = r.x + 150.0;
        let width = (x - 16.0 - left).max(0.0);
        let title = TextStyle::ui(14.0).semibold();
        let tt = ui.measure(&self.meta.title, &title, TextOpts::ellipsize((width - 150.0).max(40.0)));
        let pt = ui.measure(&page, &TextStyle::ui(13.0), TextOpts::default());
        let total = tt.w + 14.0 + pt.w;
        let tx = left + ((width - total) / 2.0).max(0.0);
        ui.g.draw_text(tt, tx, r.y + (r.h - tt.h) / 2.0, c.text);
        let pr = Rect::new(tx + tt.w + 8.0, r.y + 8.0, pt.w + 12.0, 32.0);
        if let Some(mut typed) = self.goto.take() {
            let n = self.page_count();
            let input = Rect::new(pr.x, pr.y, 120.0, 32.0);
            let resp = ui.text_input(id("pdf-goto"), input, &mut typed, &format!("Page (1–{n})"), false);
            ui.focus_input(id("pdf-goto"));
            if resp.submitted {
                if let Ok(p) = typed.trim().parse::<usize>() {
                    self.go_to_page(p.clamp(1, n) - 1);
                }
            } else if !resp.escaped {
                self.goto = Some(typed);
            }
        } else {
            let resp = ui.interact_as(id("pdf-page"), pr, Role::Button, "Go to a page (Ctrl+G)");
            if resp.hovered {
                ui.g.fill_round(pr, 6.0, c.hover());
            }
            ui.g.draw_text(pt, pr.x + 6.0, r.y + (r.h - pt.h) / 2.0, c.muted);
            ui.tip(id("pdf-page"), pr, "Go to a page (Ctrl+G)");
            if resp.clicked {
                self.goto = Some(String::new());
            }
        }
    }
}

/// A PDF locked with a password, asking for it in place of its pages.
pub struct LockedPdf {
    pub meta: WorkMeta,
    /// Where the reader was, for when it opens.
    pub progress: Option<ReadingProgress>,
    /// The last password tried was wrong.
    pub wrong: bool,
    typed: String,
    focused: bool,
}

impl LockedPdf {
    pub fn new(meta: WorkMeta, progress: Option<ReadingProgress>) -> Self {
        Self { meta, progress, wrong: false, typed: String::new(), focused: false }
    }

    /// The toolbar, and a card asking for the password: Enter or Open tries it.
    pub fn frame(&mut self, ui: &mut Ui, r: Rect) -> Vec<ReaderAction> {
        let mut out = Vec::new();
        let c = ui.c;
        let bar = Rect::new(r.x, r.y, r.w, TOOLBAR_H);
        ui.g.fill_rect(bar, c.surface);
        ui.separator(bar.x, bar.bottom() - 1.0, bar.w);
        if ui.button_ex(id("pdf-back"), Rect::new(r.x + 8.0, r.y + 8.0, 96.0, 32.0), "Library", Some("\u{E72B}"), ButtonKind::Ghost, true) {
            out.push(ReaderAction::Close);
        }
        let tt = ui.measure(&self.meta.title, &TextStyle::ui(14.0).semibold(), TextOpts::ellipsize((r.w - 260.0).max(40.0)));
        ui.g.draw_text(tt, r.x + (r.w - tt.w) / 2.0, r.y + (TOOLBAR_H - tt.h) / 2.0, c.text);

        let (w, h) = (400.0f32.min(r.w - 32.0), 214.0);
        let card = Rect::new(r.x + (r.w - w) / 2.0, r.y + TOOLBAR_H + ((r.h - TOOLBAR_H - h) / 2.0).max(16.0), w, h);
        ui.g.fill_round(card, 12.0, c.card);
        ui.g.stroke_round(card, 12.0, 1.0, c.border);
        let x = card.x + 24.0;
        ui.text("\u{E72E}", &TextStyle::icons(18.0), x, card.y + 25.0, c.text);
        ui.text("This PDF is locked", &TextStyle::ui(17.0).semibold(), x + 30.0, card.y + 22.0, c.text);
        let (msg, color) = if self.wrong { ("That password isn't right. Try again.", c.danger_text) } else { ("Type its password to open it.", c.muted) };
        ui.text(msg, &TextStyle::ui(13.5), x, card.y + 62.0, color);
        let input = ui.text_input(id("pdf-password"), Rect::new(x, card.y + 92.0, card.w - 48.0, 36.0), &mut self.typed, "Password", true);
        if !self.focused {
            ui.focus_input(id("pdf-password"));
            self.focused = true;
        }
        let bw = ui.button_width("Open", None).max(96.0);
        let can = !self.typed.is_empty();
        let open = ui.button_ex(id("pdf-unlock"), Rect::new(card.right() - 24.0 - bw, card.y + 150.0, bw, 36.0), "Open", None, ButtonKind::Primary, can);
        if can && (open || input.submitted) {
            out.push(ReaderAction::Unlock(std::mem::take(&mut self.typed)));
        } else if input.escaped {
            out.push(ReaderAction::Close);
        }
        out
    }
}
