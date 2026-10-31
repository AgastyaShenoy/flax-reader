//! PDF through hayro's interpreter and our copy of its renderer (`raster`).

use crate::nav::{self, Nav};
use crate::raster::{self, DisplayList, GlyphCache, ImageCache, RenderCache, RenderSettings, SharedGlyphs, Target};
use crate::{Bytes, DocInfo, Engine, OpenError, PageRenderer, PageSize, PixelRect};
use hayro_interpret::InterpreterSettings;
use hayro_interpret::hayro_syntax::object::{Array, DateTime, Dict, ObjectIdentifier};
use hayro_interpret::hayro_syntax::{DecryptionError, LoadPdfError, Pdf, PdfVersion};
use hayro_interpret::util::TransformExt;
use kurbo::Affine;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use vello_cpu::color::palette::css::WHITE;
use vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};

/// Recorded pages kept, most recently used ones.
const LISTS: usize = 12;

/// A page's display list, recorded by the first render thread that needs it. Threads that want
/// the same page meanwhile wait for it instead of recording it again. `None` inside means the page
/// renders directly.
type Slot = Arc<OnceLock<Option<Arc<DisplayList>>>>;

pub(crate) struct PdfEngine {
    pdf: Pdf,
    sizes: Vec<PageSize>,
    /// Page objects to page numbers, for links and the outline.
    page_ids: HashMap<ObjectIdentifier, usize>,
    display_lists: bool,
    images: Arc<ImageCache>,
    glyphs: Arc<SharedGlyphs>,
    /// Shared by all render threads, most recently used last.
    lists: Mutex<Vec<(usize, Slot)>>,
    /// Pages that can't be recorded, so threads don't each try.
    direct: Mutex<HashSet<usize>>,
    /// Pages recorded and pages that couldn't be. A failed recording is a wasted interpretation, so
    /// a document whose pages mostly fail (a scan) stops trying.
    recorded: AtomicUsize,
    failed: AtomicUsize,
}

impl PdfEngine {
    pub(crate) fn open(bytes: Arc<Bytes>, display_lists: bool, password: &str) -> Result<Self, OpenError> {
        let pdf = Pdf::new_with_password(bytes, password).map_err(|e| match e {
            LoadPdfError::Decryption(DecryptionError::PasswordProtected) => OpenError::NeedsPassword,
            LoadPdfError::Decryption(_) => OpenError::UnsupportedEncryption,
            LoadPdfError::Invalid => OpenError::Damaged,
        })?;
        let sizes = pdf
            .pages()
            .iter()
            .map(|p| {
                let (width, height) = p.render_dimensions();
                PageSize { width, height }
            })
            .collect();
        let page_ids = nav::page_ids(&pdf);
        Ok(Self {
            pdf,
            sizes,
            page_ids,
            display_lists,
            images: Arc::default(),
            glyphs: Arc::default(),
            lists: Mutex::default(),
            direct: Mutex::default(),
            recorded: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
        })
    }
}

impl Engine for PdfEngine {
    fn page_count(&self) -> usize {
        self.sizes.len()
    }

    fn page_size(&self, page: usize) -> PageSize {
        self.sizes[page]
    }

    fn renderer(&self) -> Box<dyn PageRenderer + '_> {
        Box::new(PdfRenderer {
            engine: self,
            settings: InterpreterSettings::default(),
            cache: RenderCache::new(),
            glyphs: GlyphCache::new(self.glyphs.clone()),
            // single-threaded: parallelism comes from several render threads
            ctx: RenderContext::new_with(1, 1, vello_cpu::RenderSettings { num_threads: 0, ..Default::default() }),
            pixmap: Pixmap::new(1, 1),
        })
    }

    fn trim(&self) {
        self.images.clear();
        self.glyphs.clear();
        self.lists.lock().unwrap().clear();
    }

    fn page_text(&self, page: usize) -> crate::PageText {
        crate::text::extract(&self.pdf.pages()[page], &InterpreterSettings::default())
    }

    fn page_links(&self, page: usize) -> Vec<crate::Link> {
        Nav::new(&self.pdf, &self.page_ids).links(page)
    }

    fn page_annotations(&self, page: usize) -> Vec<crate::Annotation> {
        Nav::new(&self.pdf, &self.page_ids).annotations(page)
    }

    fn outline(&self) -> Vec<crate::OutlineItem> {
        Nav::new(&self.pdf, &self.page_ids).outline()
    }

    fn has_form(&self) -> bool {
        let catalog = self.pdf.xref().get::<Dict>(self.pdf.xref().root_id());
        let fields = catalog.and_then(|c| c.get::<Dict>("AcroForm")).and_then(|f| f.get::<Array>("Fields"));
        fields.is_some_and(|f| f.raw_iter().next().is_some())
    }

    fn page_transform(&self, page: usize) -> Option<[f64; 6]> {
        Some(self.pdf.pages().get(page)?.initial_transform(true).to_kurbo().as_coeffs())
    }

    fn info(&self) -> DocInfo {
        let m = self.pdf.metadata();
        let text = |b: &Option<Vec<u8>>| b.as_deref().map(|b| nav::text_string(b).trim().to_owned()).filter(|s| !s.is_empty());
        let date = |d: &Option<DateTime>| d.map(|d| format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year, d.month, d.day, d.hour, d.minute));
        let version = match self.pdf.version() {
            PdfVersion::Pdf10 => "1.0",
            PdfVersion::Pdf11 => "1.1",
            PdfVersion::Pdf12 => "1.2",
            PdfVersion::Pdf13 => "1.3",
            PdfVersion::Pdf14 => "1.4",
            PdfVersion::Pdf15 => "1.5",
            PdfVersion::Pdf16 => "1.6",
            PdfVersion::Pdf17 => "1.7",
            PdfVersion::Pdf20 => "2.0",
        };
        DocInfo {
            format: format!("PDF {version}"),
            title: text(&m.title),
            author: text(&m.author),
            subject: text(&m.subject),
            keywords: text(&m.keywords),
            creator: text(&m.creator),
            producer: text(&m.producer),
            created: date(&m.creation_date),
            modified: date(&m.modification_date),
        }
    }
}

struct PdfRenderer<'a> {
    engine: &'a PdfEngine,
    settings: InterpreterSettings,
    /// hayro's font and glyph caches, kept as long as this renderer.
    cache: RenderCache<'a>,
    glyphs: GlyphCache,
    ctx: RenderContext,
    /// Scratch target, at least as big as the largest tile so far.
    pixmap: Pixmap,
}

impl PdfRenderer<'_> {
    /// The page's display list, or `None` if the page renders directly.
    fn display_list(&mut self, index: usize) -> Option<Arc<DisplayList>> {
        let engine = self.engine;
        let slot = {
            let mut lists = engine.lists.lock().unwrap();
            let slot = match lists.iter().position(|(p, _)| *p == index) {
                Some(i) => lists.remove(i).1,
                None => Slot::default(),
            };
            lists.push((index, slot.clone()));
            if lists.len() > LISTS {
                lists.remove(0);
            }
            slot
        };
        slot.get_or_init(|| {
            let failed = engine.failed.load(Ordering::Relaxed);
            let mostly_failing = failed >= 3 && failed > 2 * engine.recorded.load(Ordering::Relaxed);
            if mostly_failing || engine.direct.lock().unwrap().contains(&index) {
                return None;
            }
            let page = &engine.pdf.pages()[index];
            let list = raster::record(page, &self.cache, &self.settings, &engine.images).map(Arc::new);
            if list.is_none() {
                engine.direct.lock().unwrap().insert(index);
                engine.failed.fetch_add(1, Ordering::Relaxed);
            } else {
                engine.recorded.fetch_add(1, Ordering::Relaxed);
            }
            list
        })
        .clone()
    }
}

impl PageRenderer for PdfRenderer<'_> {
    fn render(&mut self, index: usize, scale: f32, area: PixelRect) -> Vec<u32> {
        let (w, h) = (area.width as u16, area.height as u16);
        if self.pixmap.width() < w || self.pixmap.height() < h {
            self.pixmap = Pixmap::new(w.max(self.pixmap.width()), h.max(self.pixmap.height()));
        }
        self.ctx.reset_and_resize(w, h);
        let tile = Affine::translate((-f64::from(area.x), -f64::from(area.y))) * Affine::scale(f64::from(scale));
        let list = if self.engine.display_lists { self.display_list(index) } else { None };
        let engine = self.engine;
        let defer = list.as_ref().is_some_and(|l| l.defers_at(f64::from(scale)));
        match &list {
            Some(list) => {
                let settings = RenderSettings::default();
                raster::replay(list, &self.cache, &settings, &engine.images, &mut self.ctx, tile, defer);
            }
            None => {
                let page = &engine.pdf.pages()[index];
                let transform = tile * page.initial_transform(true).to_kurbo();
                raster::render_into(
                    page,
                    &self.cache,
                    &self.settings,
                    &RenderSettings::default(),
                    &engine.images,
                    &mut self.ctx,
                    transform,
                );
            }
        }
        self.ctx.flush();
        // a pixmap bigger than the scene gets cleared to white beyond it, and that part is skipped below
        self.ctx.render_with(
            &mut self.pixmap,
            &mut Resources::default(),
            RasterizerSettings { target_init: TargetInit::Clear(WHITE), ..Default::default() },
        );
        if let Some(list) = list.filter(|_| defer) {
            let stride = usize::from(self.pixmap.width());
            let data = self.pixmap.data_as_u8_slice_mut();
            let mut target = Target { data, stride, width: usize::from(w), height: usize::from(h) };
            raster::stamp_text(&list, &mut target, tile, &mut self.glyphs);
        }

        let stride = usize::from(self.pixmap.width()) * 4;
        let mut out = Vec::with_capacity(usize::from(w) * usize::from(h));
        for row in self.pixmap.data_as_u8_slice().chunks_exact(stride).take(usize::from(h)) {
            // opaque, since the page is drawn on white
            out.extend(row[..usize::from(w) * 4].chunks_exact(4).map(|c| u32::from_be_bytes([0, c[0], c[1], c[2]])));
        }
        out
    }
}
