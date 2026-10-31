//! PDF books. A PDF is kept as it is (pages are drawn from the file, never converted to text),
//! in the book's folder next to its library entry, with its page count, title and author.

use crate::ao3::models::{Work, WorkMeta};
use crate::storage::library::LibraryManager;
pub use pdf_engine::{Annotation, Engine, OpenError, PageRenderer, PageSize, PixelRect};
pub use pdf_render::{Done, Job, Look, RenderPool, TileKey};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Name of the PDF in the book's folder.
pub const FILE: &str = "book.pdf";

/// True for a PDF's contents (its header may follow some junk, which readers accept).
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

/// A library entry for a PDF: what the document says about itself, the file name when it says
/// nothing (`name`), and the file itself, saved with the book. A PDF locked with a password comes in
/// with its file name and no pages; the rest is read when it's first opened (`describe`).
pub fn import(bytes: &[u8], name: Option<&str>) -> Result<Work, String> {
    let engine = match pdf_engine::open_pdf_bytes(bytes.to_vec(), "") {
        Ok(engine) => Some(engine),
        Err(OpenError::NeedsPassword) => None,
        Err(e) => return Err(format!("Couldn't open the PDF: {e}")),
    };
    let mut meta = WorkMeta {
        id: {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            format!("pdf-{:x}", h.finish())
        },
        title: given(name.map(str::to_string)).unwrap_or_else(|| "Untitled PDF".into()),
        authors: Vec::new(),
        summary_text: String::new(),
        language: String::new(),
        is_complete: true,
        pdf_pages: Some(0),
        ..WorkMeta::default()
    };
    if let Some(engine) = engine {
        if engine.page_count() == 0 {
            return Err("This PDF has no pages.".into());
        }
        describe(&mut meta, engine.as_ref());
    }
    Ok(Work { meta, chapters: Vec::new(), fetched_at: chrono::Utc::now(), resources: vec![(FILE.into(), bytes.to_vec())] })
}

fn given(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Fills in a PDF's page count, and the title, author and subject it gives, from the open document.
pub fn describe(meta: &mut WorkMeta, engine: &dyn Engine) {
    let info = engine.info();
    meta.pdf_pages = Some(engine.page_count() as u32);
    if let Some(title) = given(info.title) {
        meta.title = title;
    }
    if let Some(author) = given(info.author) {
        meta.authors = vec![author];
    }
    if let Some(subject) = given(info.subject) {
        meta.summary_text = subject;
    }
}

/// True for a PDF imported while locked, whose pages aren't known until it's opened.
pub fn is_locked(meta: &WorkMeta) -> bool {
    meta.pdf_pages == Some(0)
}

/// Passwords that opened PDFs, by book, for as long as the app runs. They're never written down,
/// so a locked PDF asks again after a restart.
static PASSWORDS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Opens a PDF book from the library, with the password that opened it earlier if there was one.
/// `OpenError::NeedsPassword` means it's locked: ask for the password and `unlock` it.
pub fn open(work_id: &str) -> Result<Arc<dyn Engine>, OpenError> {
    let known = PASSWORDS.lock().ok().and_then(|p| p.as_ref()?.get(work_id).cloned()).unwrap_or_default();
    pdf_engine::open_with_password(&LibraryManager::resource_path(work_id, FILE), &known)
}

/// Opens a locked PDF book with `password`, and remembers it if it works. (The owner's password
/// works too, but only for AES-256 PDFs: hayro checks it for no other kind.)
pub fn unlock(work_id: &str, password: &str) -> Result<Arc<dyn Engine>, OpenError> {
    let engine = pdf_engine::open_with_password(&LibraryManager::resource_path(work_id, FILE), password)?;
    if let Ok(mut p) = PASSWORDS.lock() {
        p.get_or_insert_with(HashMap::new).insert(work_id.to_string(), password.to_string());
    }
    Ok(engine)
}

/// What to call an annotation, by the PDF's name for its kind.
pub fn annotation_label(kind: &str) -> &'static str {
    match kind {
        "Text" => "Note",
        "FreeText" => "Text box",
        "Highlight" => "Highlight",
        "Underline" | "Squiggly" => "Underline",
        "StrikeOut" => "Strikethrough",
        "Ink" => "Drawing",
        "Square" | "Circle" | "Line" | "Polygon" | "PolyLine" => "Shape",
        "Stamp" => "Stamp",
        "FileAttachment" => "Attachment",
        _ => "Comment",
    }
}

/// The page's annotations that say something: notes, and comments on highlights and drawings.
pub fn page_notes(engine: &dyn Engine, page: usize) -> Vec<Annotation> {
    let mut notes = engine.page_annotations(page);
    notes.retain(|a| !a.contents.trim().is_empty());
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF, with a title in its document info when given.
    fn tiny_pdf(title: Option<&str>) -> Vec<u8> {
        let content = "BT /F1 24 Tf 72 700 Td (Hello) Tj ET";
        let info = if title.is_some() { " /Info 6 0 R" } else { "" };
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len() + 1),
        ];
        if let Some(t) = title {
            objs.push(format!("<< /Title ({t}) /Author (Jane Austen) >>"));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R{info} >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        out
    }

    #[test]
    fn imports_a_pdf_with_what_it_says_about_itself() {
        let bytes = tiny_pdf(Some("Hello Book"));
        assert!(is_pdf(&bytes));
        let w = import(&bytes, Some("file-name")).unwrap();
        assert_eq!(w.meta.title, "Hello Book");
        assert_eq!(w.meta.authors, vec!["Jane Austen".to_string()]);
        assert_eq!(w.meta.pdf_pages, Some(1));
        assert!(w.meta.id.starts_with("pdf-") && w.chapters.is_empty());
        assert_eq!(w.resources[0].0, FILE);
    }

    #[test]
    fn untitled_pdf_takes_the_file_name() {
        let w = import(&tiny_pdf(None), Some("Field Notes")).unwrap();
        assert_eq!(w.meta.title, "Field Notes");
        assert!(w.meta.authors.is_empty());
        assert!(!is_pdf(b"PK\x03\x04 not a pdf"));
    }

    #[test]
    fn locked_pdf_comes_in_with_its_file_name_until_opened() {
        let bytes = include_bytes!("../tests/data/locked.pdf");
        let w = import(bytes, Some("Locked file")).unwrap();
        assert_eq!(w.meta.title, "Locked file");
        assert!(w.meta.is_pdf() && is_locked(&w.meta));
        assert!(matches!(pdf_engine::open_pdf_bytes(bytes.to_vec(), "wrong"), Err(OpenError::NeedsPassword)));
        // its password opens it, and then it says what it is
        let engine = pdf_engine::open_pdf_bytes(bytes.to_vec(), "flax").unwrap();
        let mut meta = w.meta.clone();
        describe(&mut meta, engine.as_ref());
        assert_eq!((meta.title.as_str(), meta.pdf_pages), ("Locked Book", Some(1)));
        assert_eq!(meta.authors, vec!["Jane Austen".to_string()]);
    }

    #[test]
    fn pdf_that_only_forbids_copying_opens_without_a_password() {
        let w = import(include_bytes!("../tests/data/no-copying.pdf"), Some("No copying")).unwrap();
        assert_eq!((w.meta.title.as_str(), w.meta.pdf_pages), ("Locked Book", Some(1)));
    }
}
