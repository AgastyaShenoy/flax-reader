//! Links and the outline, read straight from the PDF's objects: link annotations on pages, the
//! catalog's outline tree, and the destinations both point to (explicit, or named in the catalog's
//! `Dests` dictionary or `Names` tree).

use crate::{Annotation, Link, OutlineItem, Target};
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, ObjectIdentifier};
use hayro_interpret::util::TransformExt;
use kurbo::{Point, Rect};
use std::collections::{HashMap, HashSet};

/// Outline entries read at most, and how deep, in case the tree loops or is huge.
const MAX_OUTLINE: usize = 20_000;
const MAX_DEPTH: usize = 32;

/// Page objects to page numbers, which destinations need. Built once per document.
pub(crate) fn page_ids(pdf: &Pdf) -> HashMap<ObjectIdentifier, usize> {
    pdf.pages().iter().enumerate().filter_map(|(i, p)| Some((p.raw().obj_id()?, i))).collect()
}

pub(crate) struct Nav<'a> {
    pdf: &'a Pdf,
    pages: &'a HashMap<ObjectIdentifier, usize>,
    catalog: Option<Dict<'a>>,
}

impl<'a> Nav<'a> {
    pub(crate) fn new(pdf: &'a Pdf, pages: &'a HashMap<ObjectIdentifier, usize>) -> Self {
        let catalog = pdf.xref().get::<Dict>(pdf.xref().root_id());
        Self { pdf, pages, catalog }
    }

    pub(crate) fn links(&self, page: usize) -> Vec<Link> {
        let Some(p) = self.pdf.pages().get(page) else { return Vec::new() };
        let Some(annots) = p.raw().get::<Array>("Annots") else { return Vec::new() };
        let to_page = p.initial_transform(true).to_kurbo();
        let mut links = Vec::new();
        for annot in annots.iter::<Dict>() {
            if annot.get::<Name>("Subtype").as_deref() != Some(b"Link".as_slice()) {
                continue;
            }
            let Some([x0, y0, x1, y1]) = annot.get::<[f64; 4]>("Rect") else { continue };
            let target = match annot.get::<Dict>("A") {
                Some(action) => self.action(&action, page),
                None => annot.get::<Object>("Dest").and_then(|d| self.destination(d)),
            };
            if let Some(target) = target {
                let r = to_page.transform_rect_bbox(Rect::new(x0, y0, x1, y1));
                links.push(Link { rect: [r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32], target });
            }
        }
        links
    }

    pub(crate) fn annotations(&self, page: usize) -> Vec<Annotation> {
        let Some(p) = self.pdf.pages().get(page) else { return Vec::new() };
        let Some(annots) = p.raw().get::<Array>("Annots") else { return Vec::new() };
        let to_page = p.initial_transform(true).to_kurbo();
        let mut found = Vec::new();
        // one at a time, so the count matches the array's even past an entry that isn't a dictionary
        let mut items = annots.flex_iter();
        for index in 0.. {
            let Some(annot) = items.next::<Dict>() else { break };
            let kind = annot.get::<Name>("Subtype").map(|n| String::from_utf8_lossy(n.as_ref()).into_owned()).unwrap_or_default();
            // hidden ones aren't drawn, so they aren't offered either
            if matches!(kind.as_str(), "Link" | "Widget" | "Popup" | "") || annot.get::<u32>("F").unwrap_or(0) & 2 != 0 {
                continue;
            }
            let Some([x0, y0, x1, y1]) = annot.get::<[f64; 4]>("Rect") else { continue };
            let r = to_page.transform_rect_bbox(Rect::new(x0, y0, x1, y1));
            let contents = annot
                .get::<hayro_interpret::hayro_syntax::object::String>("Contents")
                .map_or_else(String::new, |t| text_string(t.as_bytes()).trim().to_owned());
            found.push(Annotation { index, kind, rect: [r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32], contents });
        }
        found
    }

    pub(crate) fn outline(&self) -> Vec<OutlineItem> {
        let Some(first) = self.catalog.as_ref().and_then(|c| c.get::<Dict>("Outlines")).and_then(|o| o.get::<Dict>("First"))
        else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        self.siblings(first, 0, &mut seen)
    }

    /// An outline entry and the ones after it, with their children.
    fn siblings(&self, first: Dict<'a>, depth: usize, seen: &mut HashSet<ObjectIdentifier>) -> Vec<OutlineItem> {
        let mut items = Vec::new();
        let mut next = Some(first);
        while let Some(entry) = next {
            // a loop in the tree, or more than anyone can read
            if depth >= MAX_DEPTH || seen.len() >= MAX_OUTLINE || entry.obj_id().is_some_and(|id| !seen.insert(id)) {
                break;
            }
            let title = entry.get::<hayro_interpret::hayro_syntax::object::String>("Title").map_or_else(String::new, |t| text_string(t.as_bytes()));
            let target = match entry.get::<Dict>("A") {
                Some(action) => self.action(&action, 0),
                None => entry.get::<Object>("Dest").and_then(|d| self.destination(d)),
            };
            let children = entry.get::<Dict>("First").map(|c| self.siblings(c, depth + 1, seen)).unwrap_or_default();
            // a positive count means the entry's children are shown
            let open = entry.get::<i64>("Count").is_some_and(|n| n > 0);
            items.push(OutlineItem { title, target, open, children });
            next = entry.get::<Dict>("Next");
        }
        items
    }

    /// Where an action leads. `page` is the page it's on, for "next page" and the like.
    fn action(&self, action: &Dict<'a>, page: usize) -> Option<Target> {
        let last = self.pages.len().saturating_sub(1);
        match action.get::<Name>("S")?.as_ref() {
            b"URI" => {
                let uri = action.get::<hayro_interpret::hayro_syntax::object::String>("URI")?;
                Some(Target::Uri(String::from_utf8_lossy(uri.as_bytes()).trim().to_owned()))
            }
            b"GoTo" => self.destination(action.get::<Object>("D")?),
            b"Named" => {
                let page = match action.get::<Name>("N")?.as_ref() {
                    b"NextPage" => (page + 1).min(last),
                    b"PrevPage" => page.saturating_sub(1),
                    b"FirstPage" => 0,
                    b"LastPage" => last,
                    _ => return None,
                };
                Some(Target::Page { page, y: None })
            }
            // other documents, programs and scripts aren't followed
            _ => None,
        }
    }

    /// A destination: an array naming a page and a view, or the name of one.
    fn destination(&self, dest: Object<'a>) -> Option<Target> {
        let array = match dest {
            Object::Array(a) => a,
            Object::Name(n) => self.named(n.as_ref())?,
            Object::String(s) => self.named(s.as_bytes())?,
            // a named destination's value can be a dictionary holding the array
            Object::Dict(d) => d.get::<Array>("D")?,
            _ => return None,
        };
        let mut items = array.raw_iter();
        let page = match items.next()? {
            MaybeRef::Ref(r) => *self.pages.get(&ObjectIdentifier::from(r))?,
            // remote destinations use page numbers
            MaybeRef::NotRef(Object::Number(n)) => usize::try_from(n.as_i64()).ok()?,
            MaybeRef::NotRef(_) => return None,
        };
        let numbers: Vec<Option<f64>> = items
            .map(|item| match item {
                MaybeRef::NotRef(Object::Number(n)) => Some(n.as_f64()),
                _ => None,
            })
            .collect();
        let kind = match array.raw_iter().nth(1) {
            Some(MaybeRef::NotRef(Object::Name(n))) => n.as_ref().to_vec(),
            _ => Vec::new(),
        };
        // where the top of the view goes, in PDF coordinates, for the kinds that say
        let top = match kind.as_slice() {
            b"XYZ" => numbers.get(2).copied().flatten(),
            b"FitH" | b"FitBH" => numbers.get(1).copied().flatten(),
            b"FitR" => numbers.get(4).copied().flatten(),
            _ => None,
        };
        let y = top.and_then(|top| {
            let p = self.pdf.pages().get(page)?;
            let at = p.initial_transform(true).to_kurbo() * Point::new(0.0, top);
            Some(at.y.max(0.0) as f32)
        });
        Some(Target::Page { page: page.min(self.pages.len().saturating_sub(1)), y })
    }

    /// A named destination's array, from the catalog's `Dests` or its `Names` tree.
    fn named(&self, name: &[u8]) -> Option<Array<'a>> {
        let catalog = self.catalog.as_ref()?;
        let value = catalog
            .get::<Dict>("Dests")
            .and_then(|d| d.get::<Object>(name))
            .or_else(|| find_in_tree(&catalog.get::<Dict>("Names")?.get::<Dict>("Dests")?, name, 0))?;
        match value {
            Object::Array(a) => Some(a),
            Object::Dict(d) => d.get::<Array>("D"),
            _ => None,
        }
    }
}

/// Looks `key` up in a name tree.
fn find_in_tree<'a>(node: &Dict<'a>, key: &[u8], depth: usize) -> Option<Object<'a>> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(names) = node.get::<Array>("Names") {
        // alternating keys and values
        let mut items = names.iter::<Object>();
        while let (Some(k), Some(v)) = (items.next(), items.next()) {
            if let Object::String(s) = k
                && s.as_bytes() == key
            {
                return Some(v);
            }
        }
    }
    for kid in node.get::<Array>("Kids")?.iter::<Dict>() {
        // skip subtrees whose range can't hold the key
        if let Some(limits) = kid.get::<Array>("Limits") {
            let mut l = limits.iter::<hayro_interpret::hayro_syntax::object::String>();
            if let (Some(lo), Some(hi)) = (l.next(), l.next())
                && (key < lo.as_bytes() || key > hi.as_bytes())
            {
                continue;
            }
        }
        if let Some(found) = find_in_tree(&kid, key, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// A PDF text string: UTF-16 with a byte order mark, UTF-8 with one, or PDFDocEncoding.
pub(crate) fn text_string(bytes: &[u8]) -> String {
    let text = match bytes {
        [0xFE, 0xFF, rest @ ..] => {
            String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>())
        }
        [0xFF, 0xFE, rest @ ..] => {
            String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
        }
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => bytes.iter().map(|&b| pdf_doc_char(b)).collect(),
    };
    // line breaks and tabs inside titles show as spaces
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_owned()
}

/// PDFDocEncoding: Latin-1, except for the typographic characters in 0x18–0x1F and 0x80–0xA0.
fn pdf_doc_char(b: u8) -> char {
    const HIGH: [char; 33] = [
        '•', '†', '‡', '…', '—', '–', 'ƒ', '⁄', '‹', '›', '−', '‰', '„', '“', '”', '‘', '’', '‚', '™', 'ﬁ', 'ﬂ',
        'Ł', 'Œ', 'Š', 'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '\u{FFFD}', '€',
    ];
    const LOW: [char; 8] = ['˘', 'ˇ', 'ˆ', '˙', '˝', '˛', '˚', '˜'];
    match b {
        0x18..=0x1F => LOW[usize::from(b - 0x18)],
        0x80..=0xA0 => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_strings() {
        assert_eq!(text_string(b"Introduction"), "Introduction");
        assert_eq!(text_string(&[0xFE, 0xFF, 0x00, 0x41, 0x00, 0xE9]), "Aé");
        assert_eq!(text_string(&[0x93, b'n', b'd', b'\r']), "ﬁnd");
    }
}
