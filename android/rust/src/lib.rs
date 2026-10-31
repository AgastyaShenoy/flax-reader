//! Bridge from the Android app to the AO3 Reader core.
//!
//! Kotlin calls one JNI function, `Core.call(method, json)`, and gets JSON back:
//! `{"ok": ...}` or `{"err": "message"}`. A single coarse entry point keeps the native
//! surface tiny and needs no binding generator or JNA at runtime. Network methods
//! (`fetch`, `ao3.work`, `epub`, `login`, `cookie`, `kudos`) block, so Kotlin calls them off the UI thread;
//! they do not hold the state lock while waiting on AO3.

use ao3reader::ao3::client::Ao3Client;
use ao3reader::ao3::models::{Bookmark, Category, Highlight, LibraryEntry, ReadingProgress, ShelfStatus, UserSession, Work, WorkMeta};
use ao3reader::reader::pace::PaceTracker;
use ao3reader::reader::rich_text::{image_source, is_landmark, paragraph_display, parse_inline, table_cells, with_word_starts, word_count, ParagraphKind, RichText};
use ao3reader::reader::theme::{ReaderThemeType, Rgb};
use ao3reader::storage::config::AppConfig;
use ao3reader::ao3::search::{Blurb, Feed, FeedKind, Listing, SearchQuery};
use ao3reader::storage::export::WorkExporter;
use ao3reader::storage::follows::{FollowStore, Mark, Scope, STALE_MINUTES};
use ao3reader::storage::library::LibraryManager;
use ao3reader::sources::opds::{Feed as Catalogue, Search};
use ao3reader::sources::{Catalog, CatalogClient};
use serde_json::{json, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;

/// The system allocator plus a running count of the bytes the Rust core holds, so the memory
/// readout in Settings can tell the core apart from Android's own native memory.
struct Counting;

static RUST_LIVE: AtomicUsize = AtomicUsize::new(0);
static RUST_PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = RUST_LIVE.fetch_add(by, Relaxed) + by;
    RUST_PEAK.fetch_max(now, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            grew(l.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(l);
        if !p.is_null() {
            grew(l.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        RUST_LIVE.fetch_sub(l.size(), Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let q = System.realloc(p, l, new_size);
        if !q.is_null() {
            if new_size >= l.size() {
                grew(new_size - l.size());
            } else {
                RUST_LIVE.fetch_sub(l.size() - new_size, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const MAX_MATCHES: usize = 2000;

struct State {
    config: AppConfig,
    library: LibraryManager,
    client: Ao3Client,
    follows: FollowStore,
    /// The work open in the reader.
    work: Option<Work>,
    /// Words in each paragraph of each of its chapters, counted once when it opens: progress
    /// is worked out from them on every scroll.
    words: Vec<Vec<usize>>,
    /// For catalogs (Project Gutenberg and the reader's own).
    catalogs: CatalogClient,
    /// Learns the reading speed from how fast the reading position moves.
    pace: PaceTracker,
    started: std::time::Instant,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// The PDF open in the reader: the desktop's render threads, drawing the tiles the page view asks
/// for. Kept apart from the state so pages render without holding its lock.
static PDF: Mutex<Option<std::sync::Arc<PdfTiles>>> = Mutex::new(None);

struct PdfTiles {
    /// Which opening of a PDF this is, so the page view of a closed one gets nothing.
    doc: i32,
    pages: usize,
    /// The document, for the notes on its pages.
    engine: std::sync::Arc<dyn ao3reader::pdf::Engine>,
    pool: ao3reader::pdf::RenderPool,
    /// Tiles finished and not yet handed over.
    done: Mutex<std::collections::VecDeque<ao3reader::pdf::Done>>,
    /// (a tile finished, the PDF closed), and the page view's tile thread waiting for either.
    wake: std::sync::Arc<(Mutex<(bool, bool)>, std::sync::Condvar)>,
}

static PDF_DOCS: AtomicUsize = AtomicUsize::new(0);

/// Starts the render threads for a PDF just opened: its number and its pages' sizes.
fn open_pdf(engine: std::sync::Arc<dyn ao3reader::pdf::Engine>) -> Result<(i32, Vec<Value>), String> {
    close_pdf();
    let pages = engine.page_count();
    let sizes = (0..pages).map(|p| engine.page_size(p)).map(|s| json!([s.width, s.height])).collect();
    let wake = std::sync::Arc::new((Mutex::new((false, false)), std::sync::Condvar::new()));
    let ping = wake.clone();
    // Two pages at once (the two on screen) when there are cores to spare. Each thread holds a
    // page's worth of scratch memory while it works, so no more.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 2);
    let pool = ao3reader::pdf::RenderPool::new(engine.clone(), threads, move || {
        if let Ok(mut w) = ping.0.lock() {
            w.0 = true;
        }
        ping.1.notify_all();
    });
    let doc = PDF_DOCS.fetch_add(1, Relaxed) as i32 + 1;
    *PDF.lock().map_err(|_| "pdf poisoned")? = Some(std::sync::Arc::new(PdfTiles { doc, pages, engine, pool, done: Mutex::default(), wake }));
    Ok((doc, sizes))
}

fn close_pdf() {
    let old = PDF.lock().ok().and_then(|mut p| p.take());
    if let Some(t) = old {
        if let Ok(mut w) = t.wake.0.lock() {
            w.1 = true;
        }
        t.wake.1.notify_all();
    }
}

fn open_tiles(doc: i32) -> Option<std::sync::Arc<PdfTiles>> {
    PDF.lock().ok()?.clone().filter(|t| t.doc == doc)
}

/// The parts of pages the page view wants rendered, most important first, replacing the ones it
/// wanted before: (page, column, row, x, y, width, height, scale) for each, the column and row
/// naming the part, x to height in page pixels at the scale (pixels per point, as f32 bits).
/// Coloured for the theme (`look`: 0 as printed, 1 dark, 2 sepia, 3 black).
pub fn pdf_want(doc: i32, look: u8, jobs: &[i32]) {
    use ao3reader::pdf::{Job, PixelRect, TileKey};
    let Some(t) = open_tiles(doc) else { return };
    let u = |v: i32| v.max(0) as u32;
    let jobs = jobs
        .chunks_exact(8)
        .filter(|j| j[5] > 0 && j[6] > 0 && (j[0] as usize) < t.pages)
        .map(|j| Job {
            key: TileKey { page: u(j[0]) as usize, scale: j[7] as u32, col: u(j[1]), row: u(j[2]), look },
            area: PixelRect { x: u(j[3]), y: u(j[4]), width: u(j[5]), height: u(j[6]) },
        })
        .collect();
    t.pool.set_jobs(jobs);
}

/// Waits for the next rendered part: (page, column, row, look, scale as f32 bits, width, height)
/// then its ARGB pixels, none if the engine failed on it. None once the PDF is closed.
pub fn pdf_tile(doc: i32) -> Option<Vec<i32>> {
    let t = open_tiles(doc)?;
    // Half-second waits with nothing rendered, since the last part was handed over.
    let mut quiet = 0;
    loop {
        let next = t.done.lock().ok()?.pop_front();
        if let Some(d) = next {
            let (w, h) = (d.width, d.height);
            let mut out = vec![d.key.page as i32, d.key.col as i32, d.key.row as i32, i32::from(d.key.look), d.key.scale as i32, w as i32, h as i32];
            if !d.pixels.is_empty() {
                let mut row = vec![0u32; w as usize];
                out.reserve((w * h) as usize);
                for y in 0..h {
                    d.pixels.read_row(w, y, 0, &mut row);
                    // 0x00RRGGBB, made opaque.
                    out.extend(row.iter().map(|&p| (p | 0xFF00_0000) as i32));
                }
            }
            return Some(out);
        }
        let fresh = t.pool.take_finished();
        if !fresh.is_empty() {
            t.done.lock().ok()?.extend(fresh);
            continue;
        }
        let mut w = t.wake.0.lock().ok()?;
        if !w.0 && !w.1 {
            let (guard, wait) = t.wake.1.wait_timeout(w, std::time::Duration::from_millis(500)).ok()?;
            w = guard;
            if wait.timed_out() {
                quiet += 1;
                // The render threads let go of their page-sized buffers after 3 s with nothing
                // to do; then hand that memory back to Android, which the allocator keeps
                // otherwise.
                if quiet == 8 {
                    release_free_memory();
                }
            }
        }
        if w.1 {
            return None;
        }
        w.0 = false;
    }
}

/// Returns memory freed in the Rust core to the system (bionic's `M_PURGE`).
fn release_free_memory() {
    #[cfg(target_os = "android")]
    {
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        const M_PURGE: i32 = -101;
        // SAFETY: mallopt only reads its two integer arguments.
        unsafe { mallopt(M_PURGE, 0) };
    }
}

type R = Result<Value, String>;

/// The AO3 client, the open work's id, and the AO3 id of its chapter `chapter` (0-based; none for
/// a one-shot or a work from a file), for its comments.
fn open_chapter(a: &Value) -> Result<(Ao3Client, String, Option<String>), String> {
    let i = arg_u64(a, "chapter")? as usize;
    with_state(|s| {
        let work = s.work.as_ref().ok_or("No work is open.")?;
        Ok((s.client.clone(), work.meta.id.clone(), work.chapters.get(i).and_then(|c| c.id.clone())))
    })
}

fn with_state<T>(f: impl FnOnce(&mut State) -> Result<T, String>) -> Result<T, String> {
    let mut guard = STATE.lock().map_err(|_| "state poisoned".to_string())?;
    let st = guard.as_mut().ok_or("not initialised")?;
    f(st)
}

fn arg_str<'a>(a: &'a Value, key: &str) -> Result<&'a str, String> {
    a.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing '{key}'"))
}

fn arg_u64(a: &Value, key: &str) -> Result<u64, String> {
    a.get(key).and_then(Value::as_u64).ok_or_else(|| format!("missing '{key}'"))
}

fn hex(c: Rgb) -> u32 {
    ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32
}

/// Byte offset in `s` to a UTF-16 index, which is what Android text APIs use.
fn u16_index(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].encode_utf16().count()
}

fn rich_json(r: &RichText) -> Value {
    let spans: Vec<Value> = r
        .spans
        .iter()
        .map(|s| {
            let style = match s.style {
                ao3reader::reader::rich_text::InlineStyle::Italic => "i",
                ao3reader::reader::rich_text::InlineStyle::Bold => "b",
                ao3reader::reader::rich_text::InlineStyle::Strike => "s",
            };
            json!([u16_index(&r.text, s.start), u16_index(&r.text, s.end), style])
        })
        .collect();
    json!({ "text": r.text, "spans": spans })
}

fn category_color(c: &Category) -> u32 {
    match c {
        Category::MM => 0x2E75B6,
        Category::FM => 0x9B59B6,
        Category::FF => 0xE74C3C,
        Category::Gen => 0x27AE60,
        Category::Multi => 0xE67E22,
        _ => 0x6E7781,
    }
}

fn meta_json(m: &ao3reader::ao3::models::WorkMeta) -> Value {
    let [r, g, b] = m.rating.badge_color();
    json!({
        "id": m.id,
        "title": m.title,
        "authors": m.authors,
        "summary": m.summary_text,
        "rating": m.rating.display_name(),
        "rating_color": ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
        "categories": m.categories.iter().map(|c| json!({"name": c.display_name(), "color": category_color(c)})).collect::<Vec<_>>(),
        "warnings": m.warnings,
        "fandoms": m.fandoms,
        "relationships": m.relationships,
        "characters": m.characters,
        "tags": m.freeform_tags,
        "words": m.stats.words,
        "chapters": m.stats.chapters_published,
        "chapters_total": m.stats.chapters_total,
        "kudos": m.stats.kudos,
        "bookmarks": m.stats.bookmarks,
        "hits": m.stats.hits,
        "published": m.stats.published_date,
        "complete": m.is_complete,
        "restricted": m.is_restricted,
        // From AO3 (ratings, kudos and "complete" apply) rather than a book from a file.
        "ao3": m.is_ao3(),
        // A PDF, drawn page by page: its page count.
        "pdf_pages": m.pdf_pages,
    })
}

fn shelf_name(s: ShelfStatus) -> &'static str {
    match s {
        ShelfStatus::Reading => "reading",
        ShelfStatus::Completed => "completed",
        ShelfStatus::MarkedForLater => "later",
        ShelfStatus::Favorites => "favorites",
    }
}

fn entry_json(e: &LibraryEntry) -> Value {
    json!({
        "meta": meta_json(&e.meta),
        "shelf": shelf_name(e.shelf),
        "favorite": e.is_favorite,
        "chapter": e.progress.current_chapter,
        "position": e.progress.position,
        "percentage": e.progress.percentage,
        "completed": e.progress.completed,
        "last_read": e.progress.last_read.timestamp(),
        "added": e.downloaded_at.timestamp(),
        // False for works brought in from a list on a site: opening one downloads it.
        "downloaded": e.local_file_exists,
    })
}

fn blurb_json(b: &Blurb, lib: &LibraryManager) -> Value {
    let mut v = meta_json(&b.meta);
    v["updated"] = json!(b.updated);
    v["comments"] = json!(b.meta.stats.comments);
    v["language"] = json!(b.meta.language);
    v["in_library"] = json!(lib.get_entry(&b.meta.id).is_some());
    // What a story or a catalogue's book leads to (none for AO3 works).
    v["link"] = serde_json::to_value(&b.link).unwrap_or(Value::Null);
    v
}

fn listing_json(l: &Listing, lib: &LibraryManager) -> Value {
    json!({
        "works": l.works.iter().map(|b| blurb_json(b, lib)).collect::<Vec<_>>(),
        "total": l.total,
        "page": l.page,
        "has_next": l.has_next,
    })
}

fn feed_kind(a: &Value) -> FeedKind {
    match a.get("kind").and_then(Value::as_str) {
        Some("author") => FeedKind::Author,
        Some("collection") => FeedKind::Collection,
        Some("story") => FeedKind::Story,
        Some("shelf") => FeedKind::Shelf,
        _ => FeedKind::Tag,
    }
}

/// The feeds an argument names: one feed (`id`), one kind (`kind`), or all of them.
fn scope_of(a: &Value) -> Scope<'_> {
    match (a.get("id").and_then(Value::as_str), a.get("kind").and_then(Value::as_str)) {
        (Some(id), _) => Scope::One(id),
        (None, Some(_)) => Scope::Kind(feed_kind(a)),
        _ => Scope::All,
    }
}

fn follows_json(s: &State) -> Value {
    let stale = s.follows.stale(STALE_MINUTES);
    json!({
        "unread": s.follows.unread_total(),
        "unread_by_kind": {
            "author": s.follows.unread_count(Scope::Kind(FeedKind::Author)),
            "tag": s.follows.unread_count(Scope::Kind(FeedKind::Tag)),
            "collection": s.follows.unread_count(Scope::Kind(FeedKind::Collection)),
            "story": s.follows.unread_count(Scope::Kind(FeedKind::Story)),
            "shelf": s.follows.unread_count(Scope::Kind(FeedKind::Shelf)),
        },
        "follows": s.follows.list().iter().map(|f| json!({
            "id": f.id(),
            "kind": f.feed.kind.label().to_lowercase(),
            "name": f.feed.name,
            "url": f.feed.url(1),
            "link": f.feed.link,
            "unread": f.unread.len(),
            "total": f.total,
            "checked": f.checked.map(|c| c.timestamp()),
            "stale": stale.contains(&f.id()),
            "error": f.error,
        })).collect::<Vec<_>>(),
    })
}

fn bookmarks_json(e: Option<&LibraryEntry>) -> Value {
    let mut list: Vec<&Bookmark> = e.map(|e| e.bookmarks.iter().collect()).unwrap_or_default();
    list.sort_by_key(|b| (b.chapter_index, b.paragraph_index));
    Value::Array(
        list.iter()
            .map(|b| json!({"id": b.id, "chapter": b.chapter_index, "para": b.paragraph_index, "note": b.note, "excerpt": b.excerpt}))
            .collect(),
    )
}

/// Highlights in reading order. Their ends are [paragraph, byte offset into its text as shown].
fn highlights_json(e: Option<&LibraryEntry>) -> Value {
    let mut list: Vec<&Highlight> = e.map(|e| e.highlights.iter().collect()).unwrap_or_default();
    list.sort_by_key(|h| (h.chapter, h.start));
    Value::Array(
        list.iter()
            .map(|h| json!({"id": h.id, "chapter": h.chapter, "start": [h.start.0, h.start.1], "end": [h.end.0, h.end.1], "color": h.color, "note": h.note, "text": h.text}))
            .collect(),
    )
}

fn arg_pair(a: &Value, key: &str) -> Result<(usize, usize), String> {
    let v = a.get(key).and_then(Value::as_array).ok_or_else(|| format!("missing {key}"))?;
    let n = |i: usize| v.get(i).and_then(Value::as_u64).map(|n| n as usize).ok_or_else(|| format!("bad {key}"));
    Ok((n(0)?, n(1)?))
}

fn paragraph_words(work: &Work, ch: usize) -> Vec<usize> {
    work.chapters.get(ch).map(|c| c.paragraphs.iter().map(|p| word_count(p)).collect()).unwrap_or_default()
}

/// (percentage, words left in work, words left in chapter) for a reading position.
fn progress_stats(words: &[Vec<usize>], ch: usize, position: f64) -> (f64, usize, usize) {
    let chapter_words: Vec<usize> = words.iter().map(|p| p.iter().sum()).collect();
    let total: usize = chapter_words.iter().sum();
    let paras: &[usize] = words.get(ch).map_or(&[], Vec::as_slice);
    let idx = position.max(0.0).floor() as usize;
    let mut in_ch: f64 = paras.iter().take(idx).sum::<usize>() as f64;
    if let Some(w) = paras.get(idx) {
        in_ch += *w as f64 * (position - position.floor());
    }
    let read = chapter_words[..ch.min(chapter_words.len())].iter().sum::<usize>() as f64 + in_ch;
    let pct = (read / total.max(1) as f64 * 100.0).clamp(0.0, 100.0);
    let ch_words = chapter_words.get(ch).copied().unwrap_or(0) as f64;
    (pct, (total as f64 - read).max(0.0) as usize, (ch_words - in_ch).max(0.0) as usize)
}

/// Every theme's colours; "Your colours" comes from the settings.
fn themes_json(cfg: &AppConfig) -> Value {
    Value::Array(
        ReaderThemeType::all()
            .iter()
            .map(|t| {
                let p = cfg.palette_of(*t);
                json!({
                    "id": serde_json::to_value(t).unwrap_or(Value::Null),
                    "name": t.display_name(),
                    "dark": p.is_dark,
                    "bg": hex(p.bg), "surface": hex(p.surface), "card": hex(p.card), "text": hex(p.text),
                    "muted": hex(p.text_muted), "accent": hex(p.accent), "border": hex(p.border),
                    "quote_bg": hex(p.quote_bg), "notes_bg": hex(p.notes_bg),
                    "accent_text": hex(p.accent_text()), "accent_mark": hex(p.accent_mark()), "danger_text": hex(p.danger_text()),
                    "warning_text": hex(p.warning_text()), "on_accent": hex(p.on_accent()), "romance": hex(p.romance()),
                })
            })
            .collect(),
    )
}

/// Every catalog: the built-in ones, then the reader's.
fn catalogs(cfg: &AppConfig) -> Vec<Catalog> {
    ao3reader::sources::builtin().into_iter().chain(cfg.catalogs.iter().cloned()).collect()
}

fn catalogs_json(cfg: &AppConfig) -> Value {
    let builtin = ao3reader::sources::builtin().len();
    Value::Array(catalogs(cfg).iter().enumerate().map(|(i, c)| json!({ "name": c.name, "url": c.url, "builtin": i < builtin })).collect())
}

/// A catalog page for the app: entries with what can be done with each (get a book, open a shelf).
fn catalogue_json(f: &Catalogue) -> Value {
    json!({
        "title": f.title,
        "next": f.next,
        "search": match &f.search {
            Some(Search::Template(t)) => json!({ "kind": "template", "url": t }),
            Some(Search::Description(u)) => json!({ "kind": "description", "url": u }),
            None => Value::Null,
        },
        "entries": f.entries.iter().map(|e| json!({
            "key": if e.id.is_empty() { &e.title } else { &e.id },
            "title": e.title,
            "authors": e.authors,
            "summary": e.summary,
            "book": e.book().map(|l| json!({ "href": l.href, "mime": l.mime })),
            "feed": e.feed().map(|l| json!({ "href": l.href })),
        })).collect::<Vec<_>>(),
    })
}

/// The catalog an argument names by its place in `catalogs`.
fn catalog_arg(a: &Value) -> Result<(Catalog, CatalogClient), String> {
    let i = arg_u64(a, "index")? as usize;
    with_state(|s| Ok((catalogs(&s.config).get(i).cloned().ok_or("no such catalog")?, s.catalogs.clone())))
}

fn signed_in(s: &UserSession) -> bool {
    s.is_logged_in && (s.session_cookie.is_some() || s.remember_token.is_some())
}

fn session_json(s: &UserSession) -> Value {
    json!({ "signed_in": signed_in(s), "stale": s.is_logged_in && !signed_in(s), "username": s.username })
}

pub fn handle(method: &str, arg: &str) -> String {
    let a: Value = serde_json::from_str(arg).unwrap_or(Value::Null);
    let res = dispatch(method, &a);
    match res {
        Ok(v) => json!({ "ok": v }).to_string(),
        Err(e) => json!({ "err": e }).to_string(),
    }
}

fn dispatch(method: &str, a: &Value) -> R {
    match method {
        "init" => {
            let dir = arg_str(a, "dir")?;
            // The core resolves its data folder from this variable (set before any file access).
            std::env::set_var("AO3READER_DATA_DIR", dir);
            let config = AppConfig::load();
            let client = Ao3Client::new(Some(&config.session));
            let library = LibraryManager::new();
            let mut guard = STATE.lock().map_err(|_| "state poisoned")?;
            let follows = FollowStore::load();
            let themes = themes_json(&config);
            *guard = Some(State { config, library, client, follows, work: None, words: Vec::new(), catalogs: CatalogClient::new(), pace: PaceTracker::default(), started: std::time::Instant::now() });
            Ok(json!({ "themes": themes }))
        }
        // After "Your colours" change.
        "themes" => with_state(|s| Ok(themes_json(&s.config))),
        "config.get" => with_state(|s| serde_json::to_value(&s.config).map_err(|e| e.to_string())),
        "config.set" => with_state(|s| {
            let mut cfg: AppConfig = serde_json::from_value(a.get("config").cloned().unwrap_or(Value::Null)).map_err(|e| e.to_string())?;
            // The session is only changed by sign-in and sign-out, and the measured reading speed
            // only by reading (or `speed.reset`).
            cfg.session = s.config.session.clone();
            cfg.reading_speed = s.config.reading_speed;
            // Catalogues and forum sign-ins change through their own calls; the screen's copy of
            // the settings may be older than them.
            cfg.catalogs = s.config.catalogs.clone();
            cfg.forum_accounts = s.config.forum_accounts.clone();
            s.config = cfg;
            s.config.save()?;
            Ok(Value::Null)
        }),
        "session" => with_state(|s| Ok(session_json(&s.config.session))),
        "speed" => with_state(|s| {
            let sp = s.config.reading_speed;
            Ok(json!({ "measured": sp.wpm(), "minutes": sp.minutes(), "wpm": s.config.wpm(), "has_data": sp.secs > 0.0 }))
        }),
        "speed.reset" => with_state(|s| {
            s.config.reading_speed = Default::default();
            s.config.save()?;
            Ok(Value::Null)
        }),
        "library" => with_state(|s| {
            let mut list: Vec<&LibraryEntry> = s.library.entries().values().collect();
            list.sort_by(|x, y| y.progress.last_read.cmp(&x.progress.last_read));
            Ok(Value::Array(list.into_iter().map(entry_json).collect()))
        }),
        "library.favorite" => with_state(|s| {
            s.library.toggle_favorite(arg_str(a, "id")?);
            Ok(Value::Null)
        }),
        "library.shelf" => with_state(|s| {
            let shelf = match arg_str(a, "shelf")? {
                "completed" => ShelfStatus::Completed,
                "later" => ShelfStatus::MarkedForLater,
                "favorites" => ShelfStatus::Favorites,
                _ => ShelfStatus::Reading,
            };
            s.library.set_shelf(arg_str(a, "id")?, shelf);
            Ok(Value::Null)
        }),
        "library.delete" => with_state(|s| {
            let id = arg_str(a, "id")?;
            if s.work.as_ref().is_some_and(|w| w.meta.id == id) {
                s.work = None;
            }
            s.library.delete_work(id);
            Ok(Value::Null)
        }),
        // The reader's own collections: {id, name, works}, in the order they were made.
        "collections" => with_state(|s| Ok(serde_json::to_value(s.library.collections()).unwrap_or(Value::Null))),
        // Makes a collection (with a book in it, if `work` is given) and returns its id, or null
        // for a blank name.
        "collections.create" => with_state(|s| {
            let id = s.library.create_collection(arg_str(a, "name")?);
            if let (Some(id), Some(work)) = (&id, a.get("work").and_then(Value::as_str)) {
                s.library.set_in_collection(id, work, true);
            }
            Ok(json!(id))
        }),
        "collections.rename" => with_state(|s| {
            s.library.rename_collection(arg_str(a, "id")?, arg_str(a, "name")?);
            Ok(Value::Null)
        }),
        "collections.delete" => with_state(|s| {
            s.library.delete_collection(arg_str(a, "id")?);
            Ok(Value::Null)
        }),
        "collections.set" => with_state(|s| {
            let on = a.get("on").and_then(Value::as_bool).unwrap_or(true);
            s.library.set_in_collection(arg_str(a, "id")?, arg_str(a, "work")?, on);
            Ok(Value::Null)
        }),
        "work.open" => with_state(|s| {
            let id = arg_str(a, "id")?;
            let mut work = s.library.load_work(id).ok_or("missing")?;
            for c in &mut work.chapters {
                c.raw_html = String::new();
            }
            s.library.touch_history(id);
            let entry = s.library.get_entry(id);
            let chapters: Vec<Value> = (0..work.chapters.len())
                .map(|i| json!({"title": work.chapters[i].title, "words": paragraph_words(&work, i).iter().sum::<usize>()}))
                .collect();
            let out = json!({
                "meta": meta_json(&work.meta),
                "chapters": chapters,
                // A finished book opens at its start again ("Read again"), not on its last page.
                "chapter": entry.filter(|e| !e.progress.completed).map_or(1, |e| e.progress.current_chapter),
                "position": entry.filter(|e| !e.progress.completed).map_or(0.0, |e| e.progress.position),
                "bookmarks": bookmarks_json(entry),
                "highlights": highlights_json(entry),
            });
            s.library.mark_opened(&work.meta.id);
            s.words = (0..work.chapters.len()).map(|i| paragraph_words(&work, i)).collect();
            s.work = Some(work);
            s.pace.reset();
            Ok(out)
        }),
        "work.chapter" => with_state(|s| {
            let work = s.work.as_ref().ok_or("no work open")?;
            let idx = arg_u64(a, "index")? as usize;
            let ch = work.chapters.get(idx).ok_or("no such chapter")?;
            let mut blocks = Vec::new();
            // "Bold word starts": the first letters of each word in running text.
            let starts = s.config.typography.bold_word_starts;
            let block = |kind: &str, para: usize, r: &RichText| {
                let bold = starts && matches!(kind, "para" | "quote" | "item" | "center");
                let mut v = if bold { rich_json(&with_word_starts(r.clone(), true)) } else { rich_json(r) };
                v["kind"] = json!(kind);
                v["para"] = json!(para);
                v
            };
            if let Some(t) = ch.summary.as_deref().filter(|t| !t.trim().is_empty()) {
                blocks.push(block("summary", 0, &parse_inline(t.trim())));
            }
            if let Some(t) = ch.pre_notes.as_deref().filter(|t| !t.trim().is_empty()) {
                blocks.push(block("notes", 0, &parse_inline(t.trim())));
            }
            for (i, p) in ch.paragraphs.iter().enumerate() {
                if is_landmark(i, p) {
                    continue;
                }
                let v = match paragraph_display(p) {
                    (ParagraphKind::Divider, r) => block("divider", i, &r),
                    (ParagraphKind::Quote, r) => block("quote", i, &r),
                    (ParagraphKind::Heading(level), r) => {
                        let mut v = block("heading", i, &r);
                        v["level"] = json!(level);
                        v
                    }
                    (ParagraphKind::Bullet(depth), r) => {
                        let mut v = block("item", i, &r);
                        v["depth"] = json!(depth);
                        v["marker"] = json!(["\u{2022}", "\u{25E6}", "\u{25AA}"][(depth as usize - 1).min(2)]);
                        v
                    }
                    (ParagraphKind::Numbered(depth, n), r) => {
                        let mut v = block("item", i, &r);
                        v["depth"] = json!(depth);
                        v["marker"] = json!(format!("{n}."));
                        v
                    }
                    (ParagraphKind::Pre, r) => block("pre", i, &r),
                    (ParagraphKind::Center, r) => block("center", i, &r),
                    (ParagraphKind::Image, r) => {
                        let mut v = block("image", i, &r);
                        v["path"] = match image_source(p) {
                            Some((name, _)) => json!(LibraryManager::resource_path(&work.meta.id, name).to_string_lossy()),
                            None => Value::Null,
                        };
                        v
                    }
                    (ParagraphKind::TableRow(header), r) => {
                        let mut v = block("row", i, &r);
                        v["header"] = json!(header);
                        v["cells"] = Value::Array(table_cells(p).iter().map(rich_json).collect());
                        v
                    }
                    (ParagraphKind::Text, r) => block("para", i, &r),
                };
                blocks.push(v);
            }
            if let Some(t) = ch.post_notes.as_deref().filter(|t| !t.trim().is_empty()) {
                blocks.push(block("endnotes", ch.paragraphs.len(), &parse_inline(t.trim())));
            }
            // A page that's only pictures (a cover) shows them without a chapter title.
            let pictures_only = !ch.paragraphs.is_empty() && ch.paragraphs.iter().all(|p| image_source(p).is_some());
            Ok(json!({ "title": ch.title, "paragraphs": ch.paragraphs.len(), "blocks": blocks, "pictures_only": pictures_only }))
        }),
        "progress.stats" => with_state(|s| {
            let work = s.work.as_ref().ok_or("no work open")?;
            let ch = arg_u64(a, "chapter")? as usize;
            let pos = a.get("position").and_then(Value::as_f64).unwrap_or(0.0);
            let _ = work;
            let (pct, left_work, left_ch) = progress_stats(&s.words, ch, pos);
            // Every move through the work teaches the reading speed (jumps and pauses are ignored).
            let total: usize = s.words.iter().flatten().sum();
            let read = total.saturating_sub(left_work) as f64;
            let t = s.started.elapsed().as_secs_f64();
            if s.pace.observe(t, read, &mut s.config.reading_speed) {
                s.config.save()?;
            }
            Ok(json!({ "percentage": pct, "words_left": left_work, "chapter_words_left": left_ch, "wpm": s.config.wpm() }))
        }),
        "progress.save" => with_state(|s| {
            let work = s.work.as_ref().ok_or("no work open")?;
            let id = work.meta.id.clone();
            let ch = arg_u64(a, "chapter")? as usize;
            let pos = a.get("position").and_then(Value::as_f64).unwrap_or(0.0);
            let completed = a.get("completed").and_then(Value::as_bool).unwrap_or(false);
            // A PDF's position is page + fraction; text works count words read.
            let pct = match work.meta.pdf_pages {
                Some(n) => (pos / f64::from(n.max(1)) * 100.0).clamp(0.0, 100.0),
                None => progress_stats(&s.words, ch, pos).0,
            };
            s.library.update_progress(
                &id,
                ReadingProgress {
                    current_chapter: ch + 1,
                    scroll_offset: 0.0,
                    position: pos as f32,
                    percentage: if completed { 100.0 } else { pct as f32 },
                    last_read: chrono::Utc::now(),
                    completed,
                },
            );
            Ok(Value::Null)
        }),
        "bookmark.add" => with_state(|s| {
            let id = arg_str(a, "id")?.to_string();
            let ch = arg_u64(a, "chapter")? as usize;
            let para = arg_u64(a, "para")? as usize;
            let now = chrono::Utc::now();
            let excerpt = s
                .work
                .as_ref()
                .and_then(|w| w.chapters.get(ch.saturating_sub(1)))
                .and_then(|c| c.paragraphs.iter().enumerate().skip(para).find(|(i, p)| !is_landmark(*i, p)))
                .map(|(_, p)| paragraph_display(p).1.text.chars().take(160).collect())
                .unwrap_or_default();
            s.library.add_bookmark(
                &id,
                Bookmark {
                    id: format!("bm_{}_{}", ch, now.timestamp_millis()),
                    chapter_index: ch,
                    paragraph_index: para,
                    note: a.get("note").and_then(Value::as_str).unwrap_or("").trim().to_string(),
                    excerpt,
                    created_at: now,
                },
            );
            Ok(bookmarks_json(s.library.get_entry(&id)))
        }),
        // A new highlight (no id), or a new colour or note for one.
        "highlight.save" => with_state(|s| {
            let id = arg_str(a, "id")?.to_string();
            let h = a.get("highlight").ok_or("missing highlight")?;
            let hid = h.get("id").and_then(Value::as_str).filter(|i| !i.is_empty()).map(str::to_string);
            let old = hid.as_ref().and_then(|hid| s.library.get_entry(&id)?.highlights.iter().find(|x| &x.id == hid).cloned());
            let now = chrono::Utc::now();
            s.library.save_highlight(
                &id,
                Highlight {
                    id: hid.unwrap_or_else(|| format!("h{}", now.timestamp_millis())),
                    chapter: arg_u64(h, "chapter")? as usize,
                    start: arg_pair(h, "start")?,
                    end: arg_pair(h, "end")?,
                    color: h.get("color").and_then(Value::as_u64).unwrap_or(0).min(3) as u8,
                    note: h.get("note").and_then(Value::as_str).unwrap_or("").trim().to_string(),
                    text: h.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                    created_at: old.map_or(now, |o| o.created_at),
                },
            );
            Ok(highlights_json(s.library.get_entry(&id)))
        }),
        "highlight.remove" => with_state(|s| {
            let id = arg_str(a, "id")?;
            s.library.remove_highlight(id, arg_str(a, "highlight")?);
            Ok(highlights_json(s.library.get_entry(id)))
        }),
        // Wiktionary's meanings of a word (a network call: no lock held).
        "define" => {
            let word = ao3reader::sources::dictionary::word_of(arg_str(a, "word")?);
            let meanings = ao3reader::sources::dictionary::define(&word)?;
            Ok(json!({
                "word": word,
                "meanings": meanings.iter().map(|m| json!({"part": m.part, "definitions": m.definitions})).collect::<Vec<_>>(),
            }))
        }
        "bookmark.remove" => with_state(|s| {
            let id = arg_str(a, "id")?;
            s.library.remove_bookmark(id, arg_str(a, "bookmark")?);
            Ok(bookmarks_json(s.library.get_entry(id)))
        }),
        "search" => with_state(|s| {
            let work = s.work.as_ref().ok_or("no work open")?;
            let q = arg_str(a, "query")?.trim();
            if q.chars().count() < 2 {
                return Ok(json!([]));
            }
            let re = regex::RegexBuilder::new(&regex::escape(q)).case_insensitive(true).build().map_err(|e| e.to_string())?;
            let cur = a.get("chapter").and_then(Value::as_u64).unwrap_or(0) as usize;
            let n = work.chapters.len();
            let order: Vec<usize> = if a.get("all").and_then(Value::as_bool).unwrap_or(true) { (cur..n).chain(0..cur).collect() } else { vec![cur] };
            let mut out = Vec::new();
            'outer: for ci in order {
                for (pi, p) in work.chapters[ci].paragraphs.iter().enumerate() {
                    if is_landmark(pi, p) {
                        continue;
                    }
                    let (_, r) = paragraph_display(p);
                    for m in re.find_iter(&r.text) {
                        out.push(json!([ci, pi, u16_index(&r.text, m.start()), u16_index(&r.text, m.end())]));
                        if out.len() >= MAX_MATCHES {
                            break 'outer;
                        }
                    }
                }
            }
            Ok(Value::Array(out))
        }),
        "export" => with_state(|s| {
            let work = s.library.load_work(arg_str(a, "id")?).ok_or("This work's text isn't downloaded.")?;
            let text = match arg_str(a, "format")? {
                "md" => WorkExporter::to_markdown(&work),
                "txt" => WorkExporter::to_txt(&work),
                _ => WorkExporter::to_html(&work),
            };
            Ok(json!({ "title": work.meta.title, "text": text }))
        }),
        "ao3.search" => {
            let q: SearchQuery = serde_json::from_value(a.get("query").cloned().unwrap_or(Value::Null)).map_err(|e| e.to_string())?;
            let page = a.get("page").and_then(Value::as_u64).unwrap_or(1) as u32;
            let client = with_state(|s| Ok(s.client.clone()))?;
            let listing = client.search(&q, page)?;
            with_state(|s| Ok(listing_json(&listing, &s.library)))
        }
        "ao3.feed" => {
            let feed = Feed::parse(arg_str(a, "name")?, feed_kind(a)).ok_or("Enter a name or an AO3 link.")?;
            let page = a.get("page").and_then(Value::as_u64).unwrap_or(1) as u32;
            let client = with_state(|s| Ok(s.client.clone()))?;
            let listing = client.feed(&feed, page)?;
            with_state(|s| Ok(listing_json(&listing, &s.library)))
        }
        "follows" => with_state(|s| Ok(follows_json(s))),
        "follows.add" => with_state(|s| {
            let feed = Feed::parse(arg_str(a, "name")?, feed_kind(a)).ok_or("Enter a name or an AO3 link.")?;
            let name = feed.name.clone();
            let id = s.follows.add(feed)?;
            Ok(json!({ "id": id, "name": name }))
        }),
        // A story from a link: a forum story downloads whole first (a network call: no lock held).
        "follows.story" => {
            let client = with_state(|s| Ok(s.catalogs.clone()))?;
            let (feed, listing, work) = ao3reader::sources::follow_story(arg_str(a, "url")?, &client)?;
            with_state(|s| {
                if let Some(w) = &work {
                    s.library.save_work(w)?;
                }
                let name = feed.name.clone();
                let id = s.follows.add(feed)?;
                let _ = s.follows.record(&id, listing);
                Ok(json!({ "id": id, "name": name, "saved": work.is_some() }))
            })
        }
        "follows.shelf" => with_state(|s| {
            let feed = Feed { kind: FeedKind::Shelf, name: arg_str(a, "name")?.to_string(), link: arg_str(a, "link")?.to_string(), source: arg_str(a, "source")?.to_string() };
            let name = feed.name.clone();
            let id = s.follows.add(feed)?;
            Ok(json!({ "id": id, "name": name }))
        }),
        // Shelves worth following that aren't followed yet.
        "follows.shelves" => with_state(|s| {
            Ok(Value::Array(
                ao3reader::sources::suggested_shelves()
                    .into_iter()
                    .filter(|f| !s.follows.is_following_link(&f.link))
                    .map(|f| json!({ "name": f.name, "link": f.link, "source": f.source }))
                    .collect(),
            ))
        }),
        // A followed shelf's book, or a followed story again, into the library.
        "follows.get" => {
            let client = with_state(|s| Ok(s.catalogs.clone()))?;
            let source = a.get("catalog").and_then(Value::as_str).unwrap_or_default();
            let catalog = with_state(|s| Ok(ao3reader::sources::builtin().into_iter().chain(s.config.catalogs.iter().cloned()).find(|c| c.url == source)))?
                .unwrap_or(Catalog { name: "the catalogue".into(), url: source.to_string(), ..Default::default() });
            let mime = a.get("mime").and_then(Value::as_str).unwrap_or_default();
            let work = client.get_work(arg_str(a, "href")?, mime, &catalog, a.get("title").and_then(Value::as_str))?;
            with_state(|s| {
                s.library.save_work(&work)?;
                Ok(json!({ "id": work.meta.id, "meta": meta_json(&work.meta) }))
            })
        }
        "follows.remove" => with_state(|s| {
            s.follows.remove(arg_str(a, "id")?)?;
            Ok(Value::Null)
        }),
        "follows.read" => with_state(|s| {
            match a.get("work").and_then(Value::as_str) {
                Some(w) => s.follows.mark_work_read(w)?,
                None => s.follows.mark_read(scope_of(a))?,
            }
            Ok(Value::Null)
        }),
        // Authors or tags from the user's own works, offered as one-tap follows (never followed
        // automatically).
        "follows.suggest" => with_state(|s| {
            let names = s.follows.suggestions(s.library.entries().values().map(|e| &e.meta), feed_kind(a), 8);
            Ok(json!(names))
        }),
        "follows.timeline" => with_state(|s| {
            Ok(Value::Array(
                s.follows
                    .timeline(scope_of(a))
                    .into_iter()
                    .map(|(b, from, mark)| {
                        let mut v = blurb_json(b, &s.library);
                        v["unread"] = json!(mark != Mark::None);
                        v["mark"] = json!(mark.label());
                        v["from"] = json!(from.iter().map(|f| f.feed.name.clone()).collect::<Vec<_>>());
                        v
                    })
                    .collect(),
            ))
        }),
        "follows.check" => {
            // Feeds are fetched one at a time with a pause between, to go easy on AO3.
            let ids: Vec<String> = if a.get("stale").and_then(Value::as_bool) == Some(true) {
                with_state(|s| Ok(s.follows.stale(STALE_MINUTES)))?
            } else {
                with_state(|s| {
                    let scope = scope_of(a);
                    Ok(s.follows
                        .list()
                        .iter()
                        .filter(|f| match scope {
                            Scope::All => true,
                            Scope::Kind(k) => f.feed.kind == k,
                            Scope::One(id) => f.id() == id,
                        })
                        .map(|f| f.id())
                        .collect())
                })?
            };
            let mut fresh = 0;
            let mut errors = Vec::new();
            for (i, id) in ids.iter().enumerate() {
                if i > 0 && errors.iter().any(|e: &String| e.contains("429")) {
                    // Told to slow down: the rest wait for the next round, marked as tried, so
                    // they aren't asked for again straight away.
                    with_state(|s| s.follows.record_error(id, "Not checked: AO3 asked to slow down (HTTP 429). It's tried again later.".into()))?;
                    continue;
                }
                if i > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                }
                // A followed story kept in the library goes along, to have its new chapters added.
                let Some((client, catalogs, list, feed, saved)) = with_state(|s| {
                    Ok(s.follows.get(id).map(|f| {
                        let saved = ao3reader::sources::story_work_id(&f.feed).and_then(|w| s.library.load_work(&w));
                        (s.client.clone(), s.catalogs.clone(), s.config.catalogs.clone(), f.feed.clone(), saved)
                    }))
                })?
                else {
                    continue;
                };
                match ao3reader::sources::check_feed(&feed, saved, &client, &catalogs, &list) {
                    Ok((listing, work)) => {
                        fresh += with_state(|s| {
                            if let Some(w) = &work {
                                s.library.save_work(w)?;
                            }
                            s.follows.record(id, listing)
                        })?
                    }
                    Err(e) => {
                        with_state(|s| s.follows.record_error(id, e.clone()))?;
                        errors.push(format!("{}: {e}", feed.name));
                    }
                }
            }
            Ok(json!({ "checked": ids.len(), "new": fresh, "errors": errors }))
        }
        "storage" => Ok(json!({ "dir": AppConfig::get_app_dir().to_string_lossy() })),
        "memory" => Ok(json!({ "live": RUST_LIVE.load(Relaxed), "peak": RUST_PEAK.load(Relaxed) })),
        "clear_cache" => with_state(|s| {
            // Only what can be downloaded again: books from files and catalogues stay.
            let n = s.library.clear_downloads();
            s.work = None;
            Ok(json!(n))
        }),
        // The story forums one can sign in to: {host, name, username (null when signed out)}.
        "forums" => with_state(|s| {
            Ok(Value::Array(
                ao3reader::sources::forum::SITES
                    .iter()
                    .map(|site| json!({ "host": site.host, "name": site.name, "username": s.config.forum_accounts.iter().find(|a| a.host == site.host).map(|a| a.username.clone()) }))
                    .collect(),
            ))
        }),
        "forum.logout" => {
            let host = arg_str(a, "host")?.to_string();
            let account = with_state(|s| {
                let account = s.config.forum_accounts.iter().find(|x| x.host == host).cloned();
                s.config.forum_accounts.retain(|x| x.host != host);
                s.config.save()?;
                Ok(account)
            })?;
            // Signs out on the forum too, so the saved cookie stops working.
            if let Some(account) = account {
                ao3reader::sources::accounts::logout(&account);
            }
            Ok(Value::Null)
        }
        // A forum story's thread from its library id ("sb-1340351"), or null.
        "story.url" => Ok(json!(ao3reader::sources::forum::url_of_id(arg_str(a, "id")?))),
        "logout" => with_state(|s| {
            s.config.session = UserSession::default();
            s.config.save()?;
            s.client = Ao3Client::new(None);
            Ok(session_json(&s.config.session))
        }),

        // ---- network: the lock is released while waiting on AO3 ----
        "fetch" => {
            let input = arg_str(a, "input")?.to_string();
            let client = with_state(|s| Ok(s.client.clone()))?;
            let work = client.fetch_work(&input)?;
            with_state(|s| {
                s.library.save_work(&work)?;
                Ok(json!({ "id": work.meta.id, "meta": meta_json(&work.meta) }))
            })
        }
        "ao3.work" => {
            // A work's details for the pasted-link card, without downloading its text.
            let input = arg_str(a, "input")?.to_string();
            let client = with_state(|s| Ok(s.client.clone()))?;
            let meta = client.work_meta(&input)?;
            with_state(|s| Ok(json!({ "meta": meta_json(&meta), "in_library": s.library.get_entry(&meta.id).is_some() })))
        }
        "import" => {
            // Recognised by content: Android's picker often hides the real file name.
            let path = std::path::PathBuf::from(arg_str(a, "path")?);
            let bytes = std::fs::read(&path).map_err(|e| format!("Couldn't read the file: {e}"))?;
            // The file's name, when known, stands in for a missing title (PDFs often have none).
            let work = ao3reader::ao3::epub::import_named(&bytes, a.get("name").and_then(Value::as_str))?;
            with_state(|s| {
                s.library.save_work(&work)?;
                Ok(json!({ "id": work.meta.id, "meta": meta_json(&work.meta) }))
            })
        }
        "pdf.open" => {
            let id = arg_str(a, "id")?;
            let opened = match a.get("password").and_then(Value::as_str) {
                Some(password) => ao3reader::pdf::unlock(id, password),
                None => ao3reader::pdf::open(id),
            };
            let engine = match opened {
                Ok(engine) => engine,
                // Locked, or the password given is wrong: the page view asks for it.
                Err(ao3reader::pdf::OpenError::NeedsPassword) => return Ok(json!({ "locked": true })),
                Err(e) => return Err(format!("Couldn't open the PDF: {e}")),
            };
            // Imported while locked: now its pages, title and author can be read.
            let title = with_state(|s| {
                let work = s.work.as_mut().ok_or("no work open")?;
                if ao3reader::pdf::is_locked(&work.meta) {
                    ao3reader::pdf::describe(&mut work.meta, engine.as_ref());
                    s.library.save_work(work)?;
                }
                Ok(work.meta.title.clone())
            })?;
            let (doc, sizes) = open_pdf(engine)?;
            Ok(json!({ "doc": doc, "sizes": sizes, "title": title }))
        }
        // Notes on a page, and comments on its highlights and drawings: where they are, in points
        // from its top-left corner, and what they say.
        "pdf.notes" => {
            let t = open_tiles(arg_u64(a, "doc")? as i32).ok_or("the PDF is closed")?;
            let page = arg_u64(a, "page")? as usize;
            if page >= t.pages {
                return Ok(json!([]));
            }
            let notes = ao3reader::pdf::page_notes(t.engine.as_ref(), page);
            Ok(Value::Array(notes.iter().map(|n| json!({ "label": ao3reader::pdf::annotation_label(&n.kind), "rect": n.rect, "text": n.contents.trim() })).collect()))
        }
        "pdf.close" => {
            close_pdf();
            Ok(Value::Null)
        }
        "catalogs" => with_state(|s| Ok(catalogs_json(&s.config))),
        "catalog.add" => with_state(|s| {
            let url = arg_str(a, "url")?.trim().to_string();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err("The address should start with http:// or https://".into());
            }
            let name = a.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
            let host = url.split("://").nth(1).unwrap_or(&url).split('/').next().unwrap_or(&url).to_string();
            s.config.catalogs.push(Catalog {
                name: name.unwrap_or(host),
                url,
                user: a.get("user").and_then(Value::as_str).unwrap_or_default().trim().to_string(),
                password: a.get("password").and_then(Value::as_str).unwrap_or_default().to_string(),
            });
            s.config.save()?;
            Ok(catalogs_json(&s.config))
        }),
        "catalog.remove" => with_state(|s| {
            let i = (arg_u64(a, "index")? as usize).checked_sub(ao3reader::sources::builtin().len()).ok_or("built-in catalogs stay")?;
            if i < s.config.catalogs.len() {
                s.config.catalogs.remove(i);
                s.config.save()?;
            }
            Ok(catalogs_json(&s.config))
        }),
        // Network calls: the state's lock is only held to read and save.
        "catalog.feed" => {
            let (catalog, client) = catalog_arg(a)?;
            Ok(catalogue_json(&client.feed(arg_str(a, "url")?, &catalog)?))
        }
        "catalog.search" => {
            let (catalog, client) = catalog_arg(a)?;
            let search = match (a.pointer("/search/kind").and_then(Value::as_str), a.pointer("/search/url").and_then(Value::as_str)) {
                (Some("template"), Some(u)) => Search::Template(u.to_string()),
                (Some(_), Some(u)) => Search::Description(u.to_string()),
                _ => return Err("This catalog can't be searched.".into()),
            };
            let feed = Catalogue { search: Some(search), ..Default::default() };
            Ok(catalogue_json(&client.search(&feed, arg_str(a, "query")?, &catalog)?))
        }
        "catalog.get" => {
            let (catalog, client) = catalog_arg(a)?;
            let work = client.get_work(arg_str(a, "url")?, a.get("mime").and_then(Value::as_str).unwrap_or_default(), &catalog, a.get("title").and_then(Value::as_str))?;
            with_state(|s| {
                s.library.save_work(&work)?;
                Ok(json!({ "id": work.meta.id, "meta": meta_json(&work.meta) }))
            })
        }
        "epub" => {
            let id = arg_str(a, "id")?.to_string();
            let path = arg_str(a, "path")?.to_string();
            let client = with_state(|s| Ok(s.client.clone()))?;
            let bytes = client.download_epub(&id)?;
            std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        // Downloads a forum story brought in by id ("sb-1340351") and saves it to the library.
        "story.get" => {
            let id = arg_str(a, "id")?.to_string();
            let url = ao3reader::sources::forum::url_of_id(&id).ok_or("That isn't a forum story.")?;
            let client = with_state(|s| Ok(s.catalogs.clone()))?;
            let site = Catalog { name: "the forum".into(), url: url.clone(), ..Default::default() };
            let work = client.get_work(&url, ao3reader::sources::opds::STORY_MIME, &site, None)?;
            with_state(|s| {
                s.library.save_work(&work)?;
                Ok(json!(work.meta.id))
            })
        }
        "forum.login" => {
            let host = arg_str(a, "host")?.to_string();
            let site = ao3reader::sources::forum::SITES.iter().find(|x| x.host == host).ok_or("Not a forum Flax Reader knows.")?;
            let account = ao3reader::sources::accounts::login(site, arg_str(a, "user")?, arg_str(a, "pass")?)?;
            with_state(|s| {
                let name = account.username.clone();
                s.config.forum_accounts.retain(|x| x.host != host);
                s.config.forum_accounts.push(account);
                s.config.save()?;
                Ok(json!(name))
            })
        }
        // Brings in the threads the reader watches on a forum: {added, total, collection}.
        "forum.watched" => {
            let host = arg_str(a, "host")?.to_string();
            let account = with_state(|s| s.config.forum_accounts.iter().find(|x| x.host == host).cloned().ok_or_else(|| "Sign in to the forum first.".to_string()))?;
            let (threads, account) = ao3reader::sources::accounts::watched_threads(&account)?;
            let site = ao3reader::sources::forum::SITES.iter().find(|x| x.host == host).map_or("the forum", |x| x.name);
            let collection = format!("Watched on {site}");
            with_state(|s| {
                if let Some(x) = s.config.forum_accounts.iter_mut().find(|x| x.host == host) {
                    *x = account;
                    s.config.save()?;
                }
                let metas: Vec<WorkMeta> = threads.into_iter().map(|t| t.meta).collect();
                let (added, total) = s.library.import_known(&metas, &collection, ShelfStatus::MarkedForLater, false);
                Ok(json!({ "added": added, "total": total, "collection": collection }))
            })
        }
        // Brings in one of the reader's AO3 lists ("bookmarks" or "later"): {added, total, collection}.
        "ao3.list" => {
            use ao3reader::ao3::client::UserList;
            let (list, collection, favorite) = match arg_str(a, "list")? {
                "later" => (UserList::MarkedForLater, "AO3 Marked for Later", false),
                _ => (UserList::Bookmarks, "AO3 bookmarks", true),
            };
            let (client, user) = with_state(|s| Ok((s.client.clone(), s.config.session.username.clone())))?;
            let user = user.ok_or("Sign in to AO3 again: it didn't say which account this is.")?;
            let metas = client.whole_user_list(&user, list, 50)?;
            with_state(|s| {
                let (added, total) = s.library.import_known(&metas, collection, ShelfStatus::MarkedForLater, favorite);
                Ok(json!({ "added": added, "total": total, "collection": collection }))
            })
        }
        // A page of AO3 comments on a chapter of the open work: {comments, page, has_next}.
        "ao3.comments" => {
            let (client, work_id, chapter_id) = open_chapter(a)?;
            let page = a.get("page").and_then(Value::as_u64).unwrap_or(1).max(1) as u32;
            Ok(serde_json::to_value(client.comments(&work_id, chapter_id.as_deref(), page)?).unwrap_or(Value::Null))
        }
        "ao3.comment" => {
            let (client, work_id, chapter_id) = open_chapter(a)?;
            client.post_comment(&work_id, chapter_id.as_deref(), arg_str(a, "text")?)?;
            Ok(Value::Null)
        }
        "kudos" => {
            let id = arg_str(a, "id")?.to_string();
            let client = with_state(|s| Ok(s.client.clone()))?;
            Ok(json!(client.leave_kudos(&id)?))
        }
        "login" => {
            let (user, pass) = (arg_str(a, "user")?.to_string(), arg_str(a, "pass")?.to_string());
            let client = with_state(|s| Ok(s.client.clone()))?;
            let session = client.login(&user, &pass)?;
            with_state(|s| {
                s.config.session = session;
                s.config.save()?;
                Ok(session_json(&s.config.session))
            })
        }
        "cookie" => {
            let value = arg_str(a, "value")?.trim().to_string();
            let client = with_state(|s| {
                s.client.set_cookie("_otw_remember_me", &value);
                Ok(s.client.clone())
            })?;
            match client.current_user() {
                Ok(Some(name)) => with_state(|s| {
                    s.config.session = UserSession { username: Some(name), session_cookie: None, remember_token: Some(value), is_logged_in: true };
                    s.config.save()?;
                    Ok(session_json(&s.config.session))
                }),
                other => {
                    with_state(|s| {
                        s.client = Ao3Client::new(Some(&s.config.session));
                        Ok(())
                    })?;
                    match other {
                        Err(e) => Err(e),
                        _ => Err("AO3 didn't recognise that cookie as signed in.".into()),
                    }
                }
            }
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

#[cfg(target_os = "android")]
mod jni_entry {
    use jni::objects::{JClass, JIntArray, JString};
    use jni::sys::{jint, jintArray, jstring};
    use jni::JNIEnv;

    /// The parts of the open PDF's pages the page view wants (see `pdf_want`).
    #[no_mangle]
    pub extern "system" fn Java_org_ao3reader_Core_pdfWant<'l>(env: JNIEnv<'l>, _class: JClass<'l>, doc: jint, look: jint, jobs: JIntArray<'l>) {
        let n = env.get_array_length(&jobs).unwrap_or(0).max(0) as usize;
        let mut buf = vec![0; n];
        if env.get_int_array_region(&jobs, 0, &mut buf).is_ok() {
            super::pdf_want(doc, look as u8, &buf);
        }
    }

    /// The next rendered part of the open PDF (see `pdf_tile`); null once it's closed.
    #[no_mangle]
    pub extern "system" fn Java_org_ao3reader_Core_pdfTile<'l>(env: JNIEnv<'l>, _class: JClass<'l>, doc: jint) -> jintArray {
        let Some(t) = super::pdf_tile(doc) else { return std::ptr::null_mut() };
        match env.new_int_array(t.len() as i32) {
            Ok(arr) if env.set_int_array_region(&arr, 0, &t).is_ok() => arr.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_org_ao3reader_Core_call<'l>(mut env: JNIEnv<'l>, _class: JClass<'l>, method: JString<'l>, arg: JString<'l>) -> jstring {
        let method: String = env.get_string(&method).map(Into::into).unwrap_or_default();
        let arg: String = env.get_string(&arg).map(Into::into).unwrap_or_default();
        // A panic must not cross into Java (it would end the app): it becomes the call's error.
        let out = std::panic::catch_unwind(|| super::handle(&method, &arg))
            .unwrap_or_else(|_| serde_json::json!({ "err": format!("Something went wrong in {method}.") }).to_string());
        env.new_string(out).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_indices_for_android_spans() {
        let r = parse_inline("日本 *語* ✨ *x*");
        let v = rich_json(&r);
        assert_eq!(v["text"], "日本 語 ✨ x");
        // 語 is one UTF-16 unit at index 3; ✨ is one unit, so x is at 7.
        assert_eq!(v["spans"][0], json!([3, 4, "i"]));
        assert_eq!(v["spans"][1], json!([7, 8, "i"]));
    }

    #[test]
    fn library_round_trip_in_a_temp_folder() {
        let dir = std::env::temp_dir().join(format!("ao3core-test-{}", std::process::id()));
        let init: Value = serde_json::from_str(&handle("init", &json!({"dir": dir.to_string_lossy()}).to_string())).unwrap();
        assert_eq!(init["ok"]["themes"].as_array().map(|t| t.len()), Some(ReaderThemeType::all().len()));
        let lib: Value = serde_json::from_str(&handle("library", "{}")).unwrap();
        assert_eq!(lib["ok"], json!([]));
        let bad: Value = serde_json::from_str(&handle("nope", "{}")).unwrap();
        assert!(bad["err"].is_string());
        let _ = std::fs::remove_dir_all(dir);
    }
}
