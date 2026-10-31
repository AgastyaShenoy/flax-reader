//! PDF books. A PDF is kept as it is (pages are drawn from the file, never converted to text),
//! in the book's folder next to its library entry, with its page count, title and author.

use crate::ao3::models::{Work, WorkMeta};
use crate::storage::library::LibraryManager;
pub use pdf_engine::{Engine, PageRenderer, PageSize, PixelRect};
pub use pdf_render::{Done, Job, Look, RenderPool, TileKey};
use std::sync::Arc;

/// Name of the PDF in the book's folder.
pub const FILE: &str = "book.pdf";

/// True for a PDF's contents (its header may follow some junk, which readers accept).
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

/// A library entry for a PDF: what the document says about itself, the file name when it says
/// nothing (`name`), and the file itself, saved with the book.
pub fn import(bytes: &[u8], name: Option<&str>) -> Result<Work, String> {
    let engine = pdf_engine::open_bytes(bytes.to_vec(), Default::default()).map_err(|e| format!("Couldn't open the PDF: {e}"))?;
    let info = engine.info();
    let pages = engine.page_count();
    if pages == 0 {
        return Err("This PDF has no pages.".into());
    }
    let given = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let meta = WorkMeta {
        id: {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            format!("pdf-{:x}", h.finish())
        },
        title: given(info.title).or_else(|| given(name.map(str::to_string))).unwrap_or_else(|| "Untitled PDF".into()),
        authors: given(info.author).into_iter().collect(),
        summary_text: given(info.subject).unwrap_or_default(),
        language: String::new(),
        is_complete: true,
        pdf_pages: Some(pages as u32),
        ..WorkMeta::default()
    };
    Ok(Work { meta, chapters: Vec::new(), fetched_at: chrono::Utc::now(), resources: vec![(FILE.into(), bytes.to_vec())] })
}

/// Opens a PDF book from the library.
pub fn open(work_id: &str) -> Result<Arc<dyn Engine>, String> {
    pdf_engine::open(&LibraryManager::resource_path(work_id, FILE)).map_err(|e| format!("Couldn't open the PDF: {e}"))
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
}
