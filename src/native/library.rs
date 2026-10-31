//! Library screen: search, shelf filters, the reader's own collections, sorting, and a card
//! per work with progress, favourite, shelf, collections, export and delete.

use super::gfx::*;
use super::icons;
use super::reader::preface::thousands;
use super::ui::*;
use crate::ao3::models::{LibraryEntry, ShelfStatus, WorkMeta};
use crate::storage::library::{Collection, LibraryManager};

/// A card's height with everything on it; books without tags or a summary get shorter ones.
const CARD_H: f32 = 192.0;
const GAP: f32 = 10.0;
const MAX_W: f32 = 960.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Html,
    Markdown,
    Text,
}

pub enum LibraryAction {
    Open(String),
    ToggleFavorite(String),
    SetShelf(String, ShelfStatus),
    Export(String, ExportFormat),
    /// Save AO3's own EPUB of a work.
    SaveAo3Epub(String),
    Delete(String),
    GoSearch,
    GoImport,
    /// Make a collection, with a book in it if it was made from that book's menu.
    NewCollection { name: String, with: Option<String> },
    RenameCollection(String, String),
    DeleteCollection(String),
    /// Put a book in a collection (true) or take it out.
    SetInCollection(String, String, bool),
}

/// What a collection name being typed is for.
enum NameFor {
    /// A new collection, holding this book if there is one.
    New(Option<String>),
    Rename(String),
}

struct Naming {
    what: NameFor,
    text: String,
    focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Recent,
    Title,
    Added,
    Author,
}

const SORTS: [(Sort, &str); 4] = [(Sort::Recent, "Recently read"), (Sort::Title, "Title"), (Sort::Added, "Recently added"), (Sort::Author, "Author")];
const SHELVES: [(ShelfStatus, &str); 4] = [
    (ShelfStatus::Reading, "Reading"),
    (ShelfStatus::Completed, "Completed"),
    (ShelfStatus::MarkedForLater, "For later"),
    (ShelfStatus::Favorites, "Favorites"),
];

pub struct LibraryScreen {
    query: String,
    filter: usize,
    sort: Sort,
    confirm_delete: Option<String>,
    /// The collection shown, if one is picked: the shelves then filter within it.
    collection: Option<String>,
    naming: Option<Naming>,
    /// Asking before deleting the collection shown.
    confirm_drop: bool,
}

impl Default for LibraryScreen {
    fn default() -> Self {
        Self { query: String::new(), filter: 0, sort: Sort::Recent, confirm_delete: None, collection: None, naming: None, confirm_drop: false }
    }
}

fn shelf_label(s: ShelfStatus) -> &'static str {
    SHELVES.iter().find(|(x, _)| *x == s).map_or("Reading", |(_, l)| l)
}

/// Filter 0 = all, 1..=3 = shelves, 4 = favourites (the heart or the Favorites shelf).
fn in_filter(e: &LibraryEntry, filter: usize) -> bool {
    match filter {
        0 => true,
        1 => e.shelf == ShelfStatus::Reading,
        2 => e.shelf == ShelfStatus::Completed,
        3 => e.shelf == ShelfStatus::MarkedForLater,
        _ => e.is_favorite || e.shelf == ShelfStatus::Favorites,
    }
}

/// Where the next of a wrapping row of chips goes, `cw` wide; rows start at `x` and are `w` wide.
fn flow(cx: &mut f32, cy: &mut f32, x: f32, w: f32, cw: f32) -> (f32, f32) {
    if *cx > x && *cx + cw > x + w {
        *cx = x;
        *cy += 38.0;
    }
    let at = (*cx, *cy);
    *cx += cw + 8.0;
    at
}

/// What a card asks of the screen.
enum CardChange {
    /// Ask before deleting this work, or stop asking.
    Confirm(Option<String>),
    /// Name a new collection for this work.
    NewCollection(String),
}

impl LibraryScreen {
    /// Shows one collection's books (a new one, say).
    pub fn show_collection(&mut self, id: &str) {
        self.collection = Some(id.to_string());
        self.filter = 0;
    }

    /// The collections row: a chip per collection (pick one to see only its books), one to
    /// make a new one, and rename and delete for the one shown; or a name box while one is
    /// being named, or the question before deleting one. Returns the row's bottom.
    fn collections_row(&mut self, ui: &mut Ui, x: f32, y: f32, w: f32, library: &LibraryManager, out: &mut Vec<LibraryAction>) -> f32 {
        let c = ui.c;
        if let Some(naming) = &mut self.naming {
            let input = Rect::new(x, y, (w * 0.4).clamp(220.0, 320.0), 32.0);
            let resp = ui.text_input(id("lib-coll-name"), input, &mut naming.text, "Name the collection", false);
            if !naming.focused {
                // Renaming starts with the old name selected, so typing replaces it.
                ui.focus_input_selected(id("lib-coll-name"));
                naming.focused = true;
            }
            let ok_label = if matches!(naming.what, NameFor::Rename(_)) { "Rename" } else { "Create" };
            let named = !naming.text.trim().is_empty();
            let ow = ui.button_width(ok_label, Some(icons::CHECK));
            let ok = ui.button_ex(id("lib-coll-ok"), Rect::new(input.right() + 8.0, y, ow, 32.0), ok_label, Some(icons::CHECK), ButtonKind::Primary, named);
            let cancel = ui.button(id("lib-coll-cancel"), Rect::new(input.right() + 16.0 + ow, y, 90.0, 32.0), "Cancel");
            if (ok || resp.submitted) && named {
                let name = naming.text.trim().to_string();
                out.push(match &naming.what {
                    NameFor::New(with) => LibraryAction::NewCollection { name, with: with.clone() },
                    NameFor::Rename(cid) => LibraryAction::RenameCollection(cid.clone(), name),
                });
                self.naming = None;
            } else if cancel || resp.escaped {
                self.naming = None;
            }
            return y + 32.0;
        }

        let shown = self.collection.as_deref().and_then(|cid| library.collection(cid));
        if let (true, Some(col)) = (self.confirm_drop, shown) {
            let q = format!("Delete the collection “{}”? Its books stay in your library.", col.name);
            ui.text_opts(&q, &TextStyle::ui(13.5), TextOpts::ellipsize((w - 230.0).max(0.0)), x, y + 7.0, c.text);
            if ui.button(id("lib-drop-no"), Rect::new(x + w - 212.0, y, 96.0, 32.0), "Cancel") {
                self.confirm_drop = false;
            }
            if ui.button_ex(id("lib-drop-yes"), Rect::new(x + w - 108.0, y, 108.0, 32.0), "Delete", Some(icons::DELETE), ButtonKind::Danger, true) {
                out.push(LibraryAction::DeleteCollection(col.id.clone()));
                self.collection = None;
                self.confirm_drop = false;
            }
            return y + 32.0;
        }

        let label = ui.measure("Collections", &TextStyle::ui(13.0).semibold(), TextOpts::default());
        ui.g.draw_text(label, x, y + (30.0 - label.h) / 2.0, c.muted);
        let (mut cx, mut cy) = (x + label.w + 14.0, y);
        let mut pick: Option<Option<String>> = None;
        if !library.collections().is_empty() {
            let (n, on) = (library.entries().len(), shown.is_none());
            let (px, py) = flow(&mut cx, &mut cy, x, w, ui.chip_width("All books", n, on));
            if ui.chip(id("lib-coll-all"), px, py, "All books", n, Badge::Count, on).1 {
                pick = Some(None);
            }
        }
        for col in library.collections() {
            let on = shown.is_some_and(|s| s.id == col.id);
            let n = col.works.len();
            let (px, py) = flow(&mut cx, &mut cy, x, w, ui.chip_width(&col.name, n, on));
            if ui.chip(id2(id("lib-coll"), &col.id), px, py, &col.name, n, Badge::Count, on).1 {
                pick = Some(if on { None } else { Some(col.id.clone()) });
            }
        }
        let (px, py) = flow(&mut cx, &mut cy, x, w, ui.chip_width("+  New collection", 0, false));
        if ui.chip(id("lib-coll-new"), px, py, "+  New collection", 0, Badge::Count, false).1 {
            self.naming = Some(Naming { what: NameFor::New(None), text: String::new(), focused: false });
        }
        if let Some(col) = shown {
            let (px, py) = flow(&mut cx, &mut cy, x, w, 30.0);
            let r = Rect::new(px, py, 30.0, 30.0);
            if ui.icon_button(id("lib-coll-rename"), r, icons::EDIT, &format!("Rename “{}”", col.name), false) {
                self.naming = Some(Naming { what: NameFor::Rename(col.id.clone()), text: col.name.clone(), focused: false });
            }
            let (px, py) = flow(&mut cx, &mut cy, x, w, 30.0);
            let r = Rect::new(px, py, 30.0, 30.0);
            if ui.icon_button(id("lib-coll-delete"), r, icons::DELETE, &format!("Delete the collection “{}”", col.name), false) {
                self.confirm_drop = true;
            }
        }
        if let Some(p) = pick {
            self.collection = p;
            self.confirm_drop = false;
            ui.scroll_to_top(id("lib-list"));
        }
        cy + 30.0
    }

    pub fn frame(&mut self, ui: &mut Ui, body: Rect, library: &LibraryManager) -> Vec<LibraryAction> {
        let mut out = Vec::new();
        let c = ui.c;
        let w = (body.w - 48.0).min(MAX_W);
        let x = body.x + (body.w - w) / 2.0;
        // A collection that's gone shows the whole library again.
        if self.collection.as_deref().is_some_and(|cid| library.collection(cid).is_none()) {
            self.collection = None;
            self.confirm_drop = false;
        }
        let shown: Option<&Collection> = self.collection.as_deref().and_then(|cid| library.collection(cid));
        let in_shown = move |e: &LibraryEntry| shown.is_none_or(|col| col.works.contains(&e.meta.id));
        // The shelves count, and filter, within the collection shown.
        let all: Vec<&LibraryEntry> = library.entries().values().filter(|e| in_shown(e)).collect();

        let title = ui.text("Library", &TextStyle::ui(26.0).semibold(), x, body.y + 22.0, c.text);
        let search_w = (w * 0.38).clamp(200.0, 320.0);
        let search = Rect::new(x + w - search_w, body.y + 22.0, search_w, 36.0);
        let it = ui.measure(icons::SEARCH, &TextStyle::icons(14.0), TextOpts::default());
        ui.g.draw_text(it, search.x - 24.0, search.y + (36.0 - it.h) / 2.0, c.muted);
        ui.text_input(id("lib-search"), search, &mut self.query, "Search title, author or tag", false);

        // Filters, each with a count badge (none for an empty shelf), and sorting.
        let y = body.y + 22.0 + title.h + 14.0;
        let names = ["All", "Reading", "Completed", "For later", "Favorites"];
        let items: Vec<(&str, usize)> = names.iter().enumerate().map(|(f, n)| (*n, all.iter().filter(|e| in_filter(e, f)).count())).collect();
        let sort_w = 170.0;
        let seg_w = (w - sort_w - 12.0).min(660.0);
        if ui.segmented_counts(id("lib-filter"), Rect::new(x, y, seg_w, 34.0), &items, &mut self.filter, Badge::Count) {
            ui.scroll_to_top(id("lib-list"));
        }
        let sort_label = SORTS.iter().find(|(s, _)| *s == self.sort).map_or("", |(_, l)| l);
        let items: Vec<(&str, bool)> = SORTS.iter().map(|(_, l)| (*l, false)).collect();
        let current = SORTS.iter().position(|(s, _)| *s == self.sort);
        if let Some(i) = ui.menu_button(id("lib-sort"), Rect::new(x + w - sort_w, y, sort_w, 34.0), sort_label, Some("\u{E8CB}"), &items, current) {
            self.sort = SORTS[i].0;
        }

        let mut list: Vec<&LibraryEntry> = library.query_entries(&self.query, None, false).into_iter().filter(|e| in_filter(e, self.filter) && in_shown(e)).collect();
        match self.sort {
            Sort::Recent => {}
            Sort::Title => list.sort_by_key(|e| e.meta.title.to_lowercase()),
            Sort::Added => list.sort_by(|a, b| b.downloaded_at.cmp(&a.downloaded_at)),
            Sort::Author => list.sort_by_key(|e| e.meta.authors.first().map(|a| a.to_lowercase()).unwrap_or_default()),
        }

        let top = if library.entries().is_empty() { y + 50.0 } else { self.collections_row(ui, x, y + 48.0, w, library, &mut out) + 16.0 };
        let area = Rect::new(body.x, top, body.w, (body.bottom() - top).max(0.0));
        if list.is_empty() {
            let r = Rect::new(x, top + 4.0, w, 150.0);
            ui.card(r);
            let (msg, empty) = if library.entries().is_empty() {
                ("Your library is empty. Search AO3 or paste a link to a work to start reading.".to_string(), true)
            } else if let Some(col) = shown.filter(|col| col.works.is_empty()) {
                (format!("“{}” is empty. Use ⋯ on a book to add it here.", col.name), false)
            } else {
                ("No works match this search or filter.".to_string(), false)
            };
            ui.text_in(&msg, &TextStyle::ui(14.0), Rect::new(r.x, r.y + 30.0, r.w, 24.0), Align::Center, c.muted);
            if empty {
                let sw = ui.button_width("Search AO3", Some(icons::SEARCH));
                let iw = ui.button_width("Import a file", Some(icons::OPEN_FILE));
                let bx = r.x + (r.w - sw - 8.0 - iw) / 2.0;
                if ui.button_ex(id("lib-empty-search"), Rect::new(bx, r.y + 76.0, sw, 36.0), "Search AO3", Some(icons::SEARCH), ButtonKind::Primary, true) {
                    out.push(LibraryAction::GoSearch);
                }
                if ui.button_ex(id("lib-empty-import"), Rect::new(bx + sw + 8.0, r.y + 76.0, iw, 36.0), "Import a file", Some(icons::OPEN_FILE), ButtonKind::Secondary, true) {
                    out.push(LibraryAction::GoImport);
                }
            }
            return out;
        }

        let confirm = self.confirm_delete.clone();
        let collections = library.collections();
        let mut change: Option<CardChange> = None;
        ui.scroll_area(id("lib-list"), area, |ui, view, y0| {
            let mut y = y0 + 2.0;
            for e in &list {
                let r = Rect::new(x, y, w, card_height(&e.meta));
                y += r.h + GAP;
                if r.bottom() < view.y || r.y > view.bottom() {
                    continue;
                }
                let confirming = confirm.as_deref() == Some(e.meta.id.as_str());
                if let Some(c) = card(ui, r, e, confirming, collections, &mut out) {
                    change = Some(c);
                }
            }
            y - y0 + 22.0
        });
        match change {
            Some(CardChange::Confirm(c)) => self.confirm_delete = c,
            Some(CardChange::NewCollection(wid)) => self.naming = Some(Naming { what: NameFor::New(Some(wid)), text: String::new(), focused: false }),
            None => {}
        }
        out
    }
}

/// Where a card's summary goes (below its tags, if it has any), and the card's height.
fn card_layout(m: &WorkMeta) -> (f32, f32) {
    let has_pills = !super::reader::preface::card_pills(m, 3).is_empty();
    let has_summary = !m.summary_text.trim().is_empty();
    let summary_y = if has_pills { 98.0 } else { 68.0 };
    let h = if has_summary { summary_y + 40.0 } else if has_pills { 96.0 } else { 64.0 } + 56.0;
    (summary_y, h.min(CARD_H))
}

fn card_height(m: &WorkMeta) -> f32 {
    card_layout(m).1
}

/// Draws one work card. Returns what it asks of the screen, if anything.
fn card(ui: &mut Ui, r: Rect, e: &LibraryEntry, confirming: bool, collections: &[Collection], out: &mut Vec<LibraryAction>) -> Option<CardChange> {
    let c = ui.c;
    let m = &e.meta;
    let wid = &m.id;
    ui.card(r);
    let mut result = None;

    // Title row with favourite toggle.
    let heart = Rect::new(r.right() - 48.0, r.y + 10.0, 36.0, 34.0);
    let heart_icon = if e.is_favorite { "\u{EB52}" } else { "\u{EB51}" };
    let fav_tip = if e.is_favorite { "Remove from favourites" } else { "Add to favourites" };
    if ui.icon_button(id2(id("lib-fav"), wid), heart, heart_icon, fav_tip, false) {
        out.push(LibraryAction::ToggleFavorite(wid.clone()));
    }
    if e.is_favorite {
        let t = ui.measure("\u{EB52}", &TextStyle::icons(15.0), TextOpts::default());
        ui.g.draw_text(t, heart.x + (heart.w - t.w) / 2.0, heart.y + (heart.h - t.h) / 2.0, Color::from_u8(225, 29, 72));
    }
    let tw = r.w - 92.0;
    let title_r = Rect::new(r.x + 16.0, r.y + 14.0, tw, 28.0);
    let tresp = ui.interact_as(id2(id("lib-title"), wid), title_r, Role::Button, &m.title);
    let tt = ui.text_opts(&m.title, &TextStyle::ui(17.0).semibold(), TextOpts::ellipsize(tw), title_r.x, title_r.y, c.text);
    if tresp.hovered {
        ui.g.fill_rect(Rect::new(title_r.x, title_r.y + tt.h - 1.0, tt.w, 1.0), c.text.alpha(0.6));
    }
    if tresp.clicked {
        out.push(LibraryAction::Open(wid.clone()));
    }
    // No "by" for a book that names no author (a PDF that doesn't say).
    let mut parts = Vec::new();
    if !m.authors.is_empty() {
        parts.push(format!("by {}", m.authors.join(", ")));
    }
    if !m.fandoms.is_empty() {
        parts.push(m.fandoms.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
    }
    let byline = parts.join("  ·  ");
    ui.text_opts(&byline, &TextStyle::ui(13.0), TextOpts::ellipsize(tw), r.x + 16.0, r.y + 44.0, c.muted);

    // Tag pills on one row: whole pills only, then "+2" for the rest.
    let pills = super::reader::preface::card_pills(m, 3);
    super::reader::preface::pill_row(ui, r.x + 16.0, r.y + 67.0, r.w - 32.0, &pills);

    let summary = m.summary_text.split_whitespace().collect::<Vec<_>>().join(" ");
    ui.text_opts(&summary, &TextStyle::ui(13.0), TextOpts::wrap(r.w - 32.0).max_lines(2).line_height(1.35), r.x + 16.0, r.y + card_layout(m).0, c.text.alpha(0.82));

    // Bottom row: progress and actions, or the delete confirmation.
    let by = r.bottom() - 46.0;
    if confirming {
        ui.g.fill_round(Rect::new(r.x + 1.0, by - 4.0, r.w - 2.0, 49.0), 0.0, c.danger.alpha(0.08));
        ui.text_in("Remove this work and its downloaded text?", &TextStyle::ui(13.5), Rect::new(r.x + 16.0, by, r.w - 260.0, 36.0), Align::Left, c.text);
        if ui.button(id2(id("lib-del-no"), wid), Rect::new(r.right() - 220.0, by, 96.0, 34.0), "Cancel") {
            result = Some(CardChange::Confirm(None));
        }
        if ui.button_ex(id2(id("lib-del-yes"), wid), Rect::new(r.right() - 116.0, by, 100.0, 34.0), "Delete", Some(icons::DELETE), ButtonKind::Danger, true) {
            out.push(LibraryAction::Delete(wid.clone()));
            result = Some(CardChange::Confirm(None));
        }
        return result;
    }

    let p = &e.progress;
    let started = p.current_chapter > 1 || p.percentage > 0.0 || p.position > 0.0;
    let label = if p.completed { "Read again" } else if started { "Resume" } else { "Read" };
    let open_w = ui.button_width(label, Some(icons::READING)).max(108.0);
    let more = Rect::new(r.right() - 16.0 - 36.0, by, 36.0, 34.0);
    let shelf_w = 140.0;
    let shelf_r = Rect::new(more.x - 8.0 - shelf_w, by, shelf_w, 34.0);
    let open_r = Rect::new(shelf_r.x - 8.0 - open_w, by, open_w, 34.0);
    if ui.button_ex(id2(id("lib-open"), wid), open_r, label, Some(icons::READING), ButtonKind::Tonal, true) {
        out.push(LibraryAction::Open(wid.clone()));
    }
    let shelf_items: Vec<(&str, bool)> = SHELVES.iter().map(|(_, l)| (*l, false)).collect();
    let current = SHELVES.iter().position(|(s, _)| *s == e.shelf);
    if let Some(i) = ui.menu_button(id2(id("lib-shelf"), wid), shelf_r, shelf_label(e.shelf), Some("\u{E8F1}"), &shelf_items, current) {
        out.push(LibraryAction::SetShelf(wid.clone(), SHELVES[i].0));
    }
    // More: in or out of each collection, a new one, export (a PDF is already its own file:
    // nothing to export), and delete.
    let mut labels: Vec<String> = collections
        .iter()
        .map(|col| if col.works.contains(wid) { format!("Remove from “{}”", col.name) } else { format!("Add to “{}”", col.name) })
        .collect();
    labels.push("New collection…".into());
    let exports: &[(&str, ExportFormat)] = if m.is_pdf() {
        &[]
    } else {
        &[("Export as HTML", ExportFormat::Html), ("Export as Markdown", ExportFormat::Markdown), ("Export as text", ExportFormat::Text)]
    };
    labels.extend(exports.iter().map(|(l, _)| l.to_string()));
    // AO3 makes its own EPUB of each work (with its tags and notes), to keep or read elsewhere.
    let ao3_epub = m.is_ao3();
    if ao3_epub {
        labels.push("Save AO3's EPUB…".into());
    }
    labels.push("Delete from library".into());
    let items: Vec<(&str, bool)> = labels.iter().enumerate().map(|(i, l)| (l.as_str(), i + 1 == labels.len())).collect();
    let n = collections.len();
    match ui.menu_button(id2(id("lib-more"), wid), more, "", Some("\u{E712}"), &items, None) {
        Some(i) if i < n => out.push(LibraryAction::SetInCollection(collections[i].id.clone(), wid.clone(), !collections[i].works.contains(wid))),
        Some(i) if i == n => result = Some(CardChange::NewCollection(wid.clone())),
        Some(i) if i <= n + exports.len() => out.push(LibraryAction::Export(wid.clone(), exports[i - n - 1].1)),
        Some(i) if ao3_epub && i == n + exports.len() + 1 => out.push(LibraryAction::SaveAo3Epub(wid.clone())),
        Some(_) => result = Some(CardChange::Confirm(Some(wid.clone()))),
        None => {}
    }
    ui.tip(id2(id("lib-more"), wid), more, "More: collections, export or delete");

    let total_ch = m.stats.chapters_published.max(1);
    let status = match (p.completed, started, m.pdf_pages) {
        (true, _, _) => "Finished".to_string(),
        // Imported locked: its pages are counted once its password opens it.
        (_, _, Some(0)) => "Locked with a password".to_string(),
        (_, true, Some(n)) => format!("Page {} of {n}  ·  {:.0}%", (p.position.floor() as u32 + 1).min(n), p.percentage),
        (_, false, Some(n)) => format!("{n} pages  ·  not started"),
        (_, true, None) => format!("Chapter {} of {}  ·  {:.0}%", p.current_chapter, total_ch, p.percentage),
        (_, false, None) => format!("{total_ch} chapters  ·  not started"),
    };
    let left_w = (open_r.x - r.x - 32.0).max(0.0);
    let st = ui.text_opts(&status, &TextStyle::ui(12.5), TextOpts::ellipsize(left_w), r.x + 16.0, by + 2.0, c.text.alpha(0.85));
    let mut stats = match m.pdf_pages {
        Some(_) => "PDF".to_string(),
        // Brought in from a list on a site (AO3 bookmarks, watched threads): opening downloads it.
        None if !e.local_file_exists => "Not downloaded yet: opening it downloads it".to_string(),
        None => format!("{} words", thousands(m.stats.words as u64)),
    };
    if m.stats.kudos > 0 {
        stats.push_str(&format!("  ·  {} kudos", thousands(m.stats.kudos)));
    }
    ui.text_opts(&stats, &TextStyle::ui(12.0), TextOpts::ellipsize(left_w), r.x + 16.0, by + 22.0, c.muted);
    let bar_x = r.x + 26.0 + st.w;
    let bar_w = (open_r.x - 16.0 - bar_x).min(180.0);
    if bar_w > 30.0 {
        let bar = Rect::new(bar_x, by + 9.0, bar_w, 4.0);
        ui.g.fill_round(bar, 2.0, c.border);
        ui.g.fill_round(Rect::new(bar.x, bar.y, bar.w * (p.percentage / 100.0).clamp(0.0, 1.0), 4.0), 2.0, c.accent_mark);
    }
    result
}
