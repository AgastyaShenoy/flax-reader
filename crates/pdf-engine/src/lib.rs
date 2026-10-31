//! Document engines behind one interface, so the viewer doesn't care which library draws a page.
//! Only PDF for now, through hayro's interpreter and display lists of our own (`raster`); PDFium and
//! the formats in docs/architecture.md §7 slot in here.

mod nav;
mod pdf;
mod raster;
mod text;

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

/// Files up to this size are read into memory, so other programs (LaTeX tools) can still overwrite
/// them while they're open. Bigger files are memory-mapped. Same rule as Sumatra.
const MAP_ABOVE: u64 = 32 << 20;

/// Size of a page in PDF points (1/72 inch), with the page's rotation applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// An open document, shared by the UI thread and the render threads.
pub trait Engine: Send + Sync {
    fn page_count(&self) -> usize;

    fn page_size(&self, page: usize) -> PageSize;

    /// Per-thread render state, such as font caches that can't be shared between threads.
    /// Each render thread makes one and drops it when it runs out of work.
    fn renderer(&self) -> Box<dyn PageRenderer + '_>;

    /// Drops caches shared between render threads. Called once rendering has been idle a while.
    fn trim(&self) {}

    /// The page's text, for search, selection and copying. Safe to call from any thread.
    fn page_text(&self, _page: usize) -> PageText {
        PageText::default()
    }

    /// The page's links. Cheap: no page content is interpreted.
    fn page_links(&self, _page: usize) -> Vec<Link> {
        Vec::new()
    }

    /// The page's annotations that a reader can remove: notes, highlights, drawings and the like;
    /// not links, form fields or the pop-ups that belong to notes.
    fn page_annotations(&self, _page: usize) -> Vec<Annotation> {
        Vec::new()
    }

    /// The document's outline (bookmarks), if it has one.
    fn outline(&self) -> Vec<OutlineItem> {
        Vec::new()
    }

    /// What the document says about itself.
    fn info(&self) -> DocInfo {
        DocInfo::default()
    }

    /// Whether the document has a form to fill in.
    fn has_form(&self) -> bool {
        false
    }

    /// How the page's own coordinates (PDF user space: points, y up) map to points from the
    /// top-left of the page as shown: x' = a x + c y + e, y' = b x + d y + f, as [a, b, c, d, e, f].
    fn page_transform(&self, _page: usize) -> Option<[f64; 6]> {
        None
    }
}

/// A document's format and the facts it records about itself, for a properties box. Each of the
/// optional ones is None when the document doesn't say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocInfo {
    /// Such as "PDF 1.7".
    pub format: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    /// The program the document was made in, and the one that turned it into this file.
    pub creator: Option<String>,
    pub producer: Option<String>,
    /// As "2023-03-15 12:00", in the time zone the document gives.
    pub created: Option<String>,
    pub modified: Option<String>,
}

/// Where a link or an outline entry leads.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A page, and the spot on it to show at the top, in points from the page's top, if given.
    Page { page: usize, y: Option<f32> },
    /// A web address or another URI, as written in the PDF.
    Uri(String),
}

/// An annotation in a PDF.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    /// Its place in the page's list of annotations, every kind counted.
    pub index: usize,
    /// The PDF's name for its kind: Text (a note), Highlight, Ink, FreeText…
    pub kind: String,
    /// x0, y0, x1, y1 in points from the page's top-left corner.
    pub rect: [f32; 4],
    /// Its text: a note's, or a comment on a highlight.
    pub contents: String,
}

/// A clickable area on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// x0, y0, x1, y1 in points from the page's top-left corner.
    pub rect: [f32; 4],
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutlineItem {
    pub title: String,
    pub target: Option<Target>,
    /// Whether the PDF asks for the entry to start expanded.
    pub open: bool,
    pub children: Vec<OutlineItem>,
}

/// A character on a page and the box it covers, in points from the page's top-left corner
/// (y down), the same space as `PageSize`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextChar {
    pub ch: char,
    /// x0, y0, x1, y1.
    pub rect: [f32; 4],
}

/// A page's text in the order the PDF draws it. Spaces and line breaks that the PDF only implies
/// by where it puts glyphs are added as characters too; line breaks have an empty box at the end of
/// their line.
#[derive(Clone, Debug, Default)]
pub struct PageText {
    pub chars: Vec<TextChar>,
}

/// How to open a document.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Record each page once and replay it per tile and zoom level. Off renders every tile from
    /// scratch, which is only useful for comparing the two.
    pub display_lists: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { display_lists: true }
    }
}

pub trait PageRenderer {
    /// Draws `area` of `page` at `scale` pixels per point, on white, as 0x00RRGGBB pixels row by row.
    /// `area` is in pixels of the whole page at that scale, so tiles of one page line up.
    fn render(&mut self, page: usize, scale: f32, area: PixelRect) -> Vec<u32>;
}

#[derive(Debug)]
pub enum OpenError {
    Io(std::io::Error),
    /// No engine reads this format.
    Unsupported,
    /// Needs a password, or the one given is wrong.
    NeedsPassword,
    /// Encrypted in a way this engine can't decrypt.
    UnsupportedEncryption,
    Damaged,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            OpenError::Io(e) => e.fmt(f),
            OpenError::Unsupported => f.write_str("not a PDF"),
            OpenError::NeedsPassword => f.write_str("it needs a password"),
            OpenError::UnsupportedEncryption => f.write_str("it's encrypted in a way this reader can't open"),
            OpenError::Damaged => f.write_str("the file is damaged"),
        }
    }
}

impl From<std::io::Error> for OpenError {
    fn from(e: std::io::Error) -> Self {
        OpenError::Io(e)
    }
}

/// The bytes of an open file.
enum Bytes {
    Read(Vec<u8>),
    Mapped(memmap2::Mmap),
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Bytes::Read(v) => v,
            Bytes::Mapped(m) => m,
        }
    }
}

pub fn open(path: &Path) -> Result<Arc<dyn Engine>, OpenError> {
    open_with(path, Options::default())
}

pub fn open_with(path: &Path, options: Options) -> Result<Arc<dyn Engine>, OpenError> {
    open_inner(path, options, "")
}

/// Opens a password-protected document. Either of its passwords works: the one for reading or the
/// owner's.
pub fn open_with_password(path: &Path, password: &str) -> Result<Arc<dyn Engine>, OpenError> {
    open_inner(path, Options::default(), password)
}

/// Opens a PDF held in memory: one with changes not saved to its file yet.
pub fn open_pdf_bytes(data: Vec<u8>, password: &str) -> Result<Arc<dyn Engine>, OpenError> {
    let head = &data[..data.len().min(1024)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(OpenError::Unsupported);
    }
    Ok(Arc::new(pdf::PdfEngine::open(Arc::new(Bytes::Read(data)), Options::default().display_lists, password)?))
}

fn open_inner(path: &Path, options: Options, password: &str) -> Result<Arc<dyn Engine>, OpenError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let bytes = if len > MAP_ABOVE {
        // SAFETY: Windows won't let another program truncate a mapped file. One that writes into it
        // in place would change these bytes under us; Sumatra takes the same risk for big files.
        Bytes::Mapped(unsafe { memmap2::Mmap::map(&file)? })
    } else {
        let mut v = Vec::with_capacity(len as usize);
        file.read_to_end(&mut v)?;
        Bytes::Read(v)
    };
    // the header may follow some junk, which readers accept
    let head = &bytes.as_ref()[..bytes.as_ref().len().min(1024)];
    if head.windows(5).any(|w| w == b"%PDF-") {
        return Ok(Arc::new(pdf::PdfEngine::open(Arc::new(bytes), options.display_lists, password)?));
    }
    Err(OpenError::Unsupported)
}
