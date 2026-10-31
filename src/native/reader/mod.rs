//! Reader screen: paged (single or two-page spread) and scrolling modes, contents and
//! bookmarks drawer, in-work search with highlights, appearance panel, shortcuts, and
//! position tracking that survives font, window and mode changes.

mod layout;
pub mod preface;

use crate::ao3::comments::{Comment, CommentPage};
use crate::ao3::models::{Bookmark, Highlight, ReadingProgress, Work};
use crate::sources::dictionary::Meaning;
use std::cell::RefCell;
use crate::native::app::{reader_family, reader_font_label};
use crate::native::gfx::*;
use crate::native::icons;
use crate::native::speech::{Progress, Voice};
use crate::native::ui::*;
use crate::reader::pace::PaceTracker;
use crate::reader::theme::ReaderThemeType;
use crate::reader::typography::{PageLayoutMode, ReadingMode, TextAlignment, TypographySettings};
use crate::storage::config::AppConfig;
use layout::{is_landmark, paragraph_rich, Block, ChapterLayout, Extra, Kind, CARD_PAD, INDENT};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

const TOOLBAR_H: f32 = 48.0;
const BOTTOM_H: f32 = 44.0;
const MAX_MATCHES: usize = 2000;
/// Minimum time between wheel-driven page turns (touchpads send many small deltas).
const FLIP_DEBOUNCE: Duration = Duration::from_millis(220);

const VK_SPACE: u16 = 0x20;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_F11: u16 = 0x7A;
const VK_ESCAPE: u16 = 0x1B;
const VK_ADD: u16 = 0x6B;
const VK_SUBTRACT: u16 = 0x6D;
const VK_OEM_PLUS: u16 = 0xBB;
const VK_OEM_MINUS: u16 = 0xBD;
const VK_F: u16 = 0x46;
const VK_B: u16 = 0x42;
const VK_R: u16 = 0x52;
const VK_C: u16 = 0x43;

/// Highlight colours: yellow, green, blue, pink.
pub const HIGHLIGHTS: [(u8, u8, u8, &str); 4] = [(250, 204, 21, "Yellow"), (74, 222, 128, "Green"), (96, 165, 250, "Blue"), (244, 114, 182, "Pink")];

fn highlight_color(i: u8, dark: bool) -> Color {
    let (r, g, b, _) = HIGHLIGHTS[(i as usize).min(HIGHLIGHTS.len() - 1)];
    Color::from_u8(r, g, b).alpha(if dark { 0.30 } else { 0.38 })
}

pub enum ReaderAction {
    Close,
    Kudos,
    AddBookmark(Bookmark),
    RemoveBookmark(String),
    ConfigChanged,
    ToggleFullscreen,
    OpenTypographySettings,
    /// A short message for the toast.
    Notice(String),
    /// A new highlight, or a changed one (colour, note).
    SaveHighlight(Highlight),
    RemoveHighlight(String),
    Copy(String),
    /// Look a word up.
    Define(String),
    /// A page of AO3 comments on a chapter (by AO3's chapter id), or on the whole work.
    LoadComments { chapter_id: Option<String>, page: u32 },
    PostComment { chapter_id: Option<String>, text: String },
    /// The password typed for a locked PDF.
    Unlock(String),
}

/// AO3 comments on the chapter being read, for Contents › Comments.
#[derive(Default)]
struct Comments {
    /// Whose comments these are: the chapter's AO3 id ("" for the whole work); None before any.
    key: Option<String>,
    list: Vec<Comment>,
    page: u32,
    has_next: bool,
    loading: bool,
    error: Option<String>,
    draft: String,
    posting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    None,
    Contents,
    Appearance,
    Search,
    Bookmark,
    /// Writing a highlight's note.
    Note,
    /// A word's meanings.
    Define,
}

/// One end of a selection: a block of this layout and a byte offset into its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Mark {
    block: usize,
    at: usize,
}

struct Selection {
    /// The layout it was made in (a new layout drops it).
    key: u64,
    anchor: Mark,
    head: Mark,
    dragging: bool,
}

/// Text drawn this frame, for picking it with the mouse.
struct Spot {
    block: usize,
    /// Where the text starts, and the part of it that's on screen.
    x: f32,
    y: f32,
    shown: Rect,
    text: Text,
}

#[derive(Debug, Clone, Copy)]
struct Match {
    chapter: usize,
    para: usize,
    start: usize,
    end: usize,
}

enum EndAction {
    NextChapter,
    Kudos,
}

/// Where reading aloud has got to.
#[derive(Debug, Clone, Copy)]
struct Aloud {
    chapter: usize,
    /// The block being said; None starts at the first one on screen once the chapter is laid out.
    block: Option<usize>,
    /// The word being said: a byte range of the block's text.
    word: Option<(usize, usize)>,
    /// The page last seen, to notice the reader turning pages themselves.
    page: usize,
}

/// Blocks worth reading aloud.
fn speakable(kind: Kind) -> bool {
    kind.searchable() || matches!(kind, Kind::Title | Kind::Summary | Kind::PreNotes | Kind::EndNotes)
}

pub struct ReaderScreen {
    pub work: Work,
    pub chapter: usize,
    page: usize,
    anchor: f32,
    /// Paragraph anchor to apply once the next layout exists.
    restore: Option<f32>,
    layout: Option<ChapterLayout>,
    chapter_words: Vec<usize>,
    para_words: Vec<usize>,
    panel: Panel,
    contents_tab: usize,
    focus_pending: bool,
    query: String,
    search_all: bool,
    searched: (String, bool),
    matches: Vec<Match>,
    current_match: Option<usize>,
    bookmark_note: String,
    pub fullscreen: bool,
    /// Two pages were on screen in the last paged frame.
    spread_shown: bool,
    wheel_acc: f32,
    last_flip: Option<Instant>,
    pub progress_dirty: bool,
    /// Learns the reading speed from how fast the position moves.
    pace: PaceTracker,
    opened: Instant,
    pace_words: f32,
    /// Reading ruler: the highlighted line (paged mode), the text lines on screen last frame,
    /// and where the mouse was (the ruler follows it when it moves).
    ruler: usize,
    ruler_lines: Vec<Rect>,
    ruler_mouse: (f32, f32),
    /// Blocks on screen last frame, in order (for reading aloud and screen readers).
    visible: Vec<usize>,
    /// Read aloud: the voice (made on first use) and where it has got to.
    voice: Option<Voice>,
    aloud: Option<Aloud>,
    /// Selecting and highlighting: the text on screen, the selection, this chapter's highlights,
    /// and the bar of actions shown over a selection or highlight (where, and for which).
    spots: RefCell<Vec<Spot>>,
    sel: Option<Selection>,
    highlights: Vec<Highlight>,
    popover: Option<(Rect, Option<String>)>,
    popover_rect: Option<Rect>,
    /// The highlight whose note is being written (a new one isn't saved until then).
    note_for: Option<Highlight>,
    note: String,
    /// The word being looked up, and its meanings once they arrive.
    define: Option<(String, Option<Result<Vec<Meaning>, String>>)>,
    comments: Comments,
    /// Signed in to AO3, so comments can be posted.
    pub can_comment: bool,
}

fn hash_of<T: Hash>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn layout_key(chapter: usize, t: &TypographySettings, family: &Family, width: f32, page_h: Option<f32>) -> u64 {
    hash_of(&(
        chapter,
        family,
        width.to_bits(),
        page_h.map(f32::to_bits),
        t.font_size.to_bits(),
        t.line_spacing_multiplier.to_bits(),
        t.paragraph_spacing.to_bits(),
        t.indent_paragraphs,
        t.text_alignment == TextAlignment::Justified,
        (t.letter_spacing.to_bits(), t.word_spacing.to_bits(), t.bold_word_starts),
        (t.show_chapter_summary, t.show_chapter_pre_notes, t.show_chapter_post_notes),
    ))
}

fn fmt_minutes(mins: f32) -> String {
    let m = mins.round().max(1.0) as u32;
    if m >= 60 { format!("{} h {} min", m / 60, m % 60) } else { format!("{m} min") }
}

impl ReaderScreen {
    pub fn new(mut work: Work, progress: Option<&ReadingProgress>) -> Self {
        // Only the parsed paragraphs are displayed; don't keep the source HTML in memory.
        for c in &mut work.chapters {
            c.raw_html = String::new();
        }
        let chapter_words: Vec<usize> = (0..work.chapters.len()).map(|i| layout::paragraph_words(&work, i).iter().sum()).collect();
        let n = work.chapters.len().max(1);
        let chapter = progress.map_or(0, |p| p.current_chapter.saturating_sub(1)).min(n.saturating_sub(1));
        let anchor = progress.map_or(0.0, |p| p.position);
        let para_words = layout::paragraph_words(&work, chapter);
        Self {
            work,
            chapter,
            page: 0,
            anchor,
            restore: Some(anchor),
            layout: None,
            chapter_words,
            para_words,
            panel: Panel::None,
            contents_tab: 0,
            focus_pending: false,
            query: String::new(),
            search_all: true,
            searched: (String::new(), true),
            matches: Vec::new(),
            current_match: None,
            bookmark_note: String::new(),
            fullscreen: false,
            spread_shown: false,
            wheel_acc: 0.0,
            last_flip: None,
            progress_dirty: false,
            pace: PaceTracker::default(),
            opened: Instant::now(),
            pace_words: -1.0,
            ruler: 0,
            ruler_lines: Vec::new(),
            ruler_mouse: (-1.0, -1.0),
            visible: Vec::new(),
            voice: None,
            aloud: None,
            spots: RefCell::new(Vec::new()),
            sel: None,
            highlights: Vec::new(),
            popover: None,
            popover_rect: None,
            note_for: None,
            note: String::new(),
            define: None,
            comments: Comments::default(),
            can_comment: false,
        }
    }

    pub fn work_id(&self) -> &str {
        &self.work.meta.id
    }

    fn chapter_count(&self) -> usize {
        self.work.chapters.len()
    }

    fn go_chapter(&mut self, idx: usize, anchor: f32) {
        if idx >= self.chapter_count() {
            return;
        }
        if idx != self.chapter {
            self.chapter = idx;
            self.layout = None;
            self.para_words = layout::paragraph_words(&self.work, idx);
        }
        self.page = 0;
        self.restore = Some(anchor);
        self.anchor = anchor.min(self.para_words.len() as f32);
        self.progress_dirty = true;
    }

    fn at_end(&self) -> bool {
        self.chapter + 1 >= self.chapter_count() && self.anchor >= self.para_words.len() as f32 - 0.05
    }

    /// Words read so far in (the work, the current chapter).
    fn words_read(&self) -> (f32, f32) {
        let before: usize = self.chapter_words[..self.chapter.min(self.chapter_words.len())].iter().sum();
        let idx = self.anchor.floor().max(0.0) as usize;
        let mut in_ch: f32 = self.para_words.iter().take(idx).sum::<usize>() as f32;
        if let Some(w) = self.para_words.get(idx) {
            in_ch += *w as f32 * (self.anchor - self.anchor.floor());
        }
        (before as f32 + in_ch, in_ch)
    }

    pub fn progress(&self) -> ReadingProgress {
        let total: usize = self.chapter_words.iter().sum();
        let (read, _) = self.words_read();
        let completed = self.at_end();
        let pct = if completed { 100.0 } else { (read / total.max(1) as f32 * 100.0).clamp(0.0, 100.0) };
        ReadingProgress {
            current_chapter: self.chapter + 1,
            scroll_offset: self.page as f32,
            position: self.anchor,
            percentage: pct,
            last_read: chrono::Utc::now(),
            completed,
        }
    }

    fn set_anchor(&mut self, a: f32) {
        if (a - self.anchor).abs() > 0.001 {
            self.anchor = a;
            self.progress_dirty = true;
        }
    }

    fn ensure_layout(&mut self, ui: &mut Ui, t: &TypographySettings, width: f32, page_h: Option<f32>) {
        let family = reader_family(t);
        let key = layout_key(self.chapter, t, &family, width, page_h);
        if self.layout.as_ref().is_some_and(|l| l.key == key) {
            return;
        }
        if self.restore.is_none() {
            self.restore = Some(self.anchor);
        }
        self.layout = Some(ChapterLayout::build(ui, key, &self.work, self.chapter, t, family, width, page_h));
    }

    // ---- frame ---------------------------------------------------------------------

    pub fn frame(&mut self, ui: &mut Ui, r: Rect, config: &mut AppConfig, bookmarks: &[Bookmark], highlights: &[Highlight]) -> Vec<ReaderAction> {
        let mut out = Vec::new();
        self.highlights = highlights.iter().filter(|h| h.chapter == self.chapter).cloned().collect();
        self.selection_keys(ui, &mut out);
        self.global_keys(ui, config, &mut out);
        self.spots.borrow_mut().clear();
        let t = config.typography.clone();
        let c = ui.c;
        // Focus mode: only the text, like full screen but in the window.
        let chrome = !self.fullscreen && !t.focus_mode;
        let top = if chrome { TOOLBAR_H } else { 0.0 };
        let bottom = if chrome && t.show_reading_progress_bar { BOTTOM_H } else { 0.0 };
        let area = Rect::new(r.x, r.y + top, r.w, (r.h - top - bottom).max(100.0));

        let panel_rect = self.panel_rect(r, area);
        if self.panel != Panel::None {
            ui.block(area);
            if ui.input.pressed && area.contains(ui.input.mouse) && !panel_rect.is_some_and(|p| p.contains(ui.input.mouse)) {
                self.close_panel();
                ui.request_repaint();
            }
        }

        ui.g.fill_rect(area, c.bg);
        let end = match t.reading_mode {
            ReadingMode::Paged => self.paged(ui, area, &t),
            ReadingMode::Scroll => self.scrolled(ui, area, &t, config.auto_next_chapter),
        };
        match end {
            Some(EndAction::NextChapter) => self.go_chapter(self.chapter + 1, 0.0),
            Some(EndAction::Kudos) => out.push(ReaderAction::Kudos),
            None => {}
        }
        // What's on the page, for screen readers (Tab reaches it).
        if let Some(layout) = &self.layout {
            let text: Vec<&str> = self.visible.iter().map(|&b| layout.blocks[b].rich.text.trim()).filter(|s| !s.is_empty()).collect();
            ui.text_node(id("rd-page"), area, &text.join("\n"));
        }
        self.read_aloud(ui, area, &t, config.speech_rate);
        if self.panel == Panel::None {
            self.select(ui, area);
        }
        // A soft colour over the page, then the reading ruler on top.
        if let Some(tint) = t.tint {
            ui.g.fill_rect(area, Color::from_u8((tint >> 16) as u8, (tint >> 8) as u8, tint as u8).alpha(t.tint_strength.clamp(0.0, 0.5)));
        }
        if t.reading_ruler && self.panel == Panel::None {
            self.draw_ruler(ui, area, t.reading_mode == ReadingMode::Scroll);
        }

        // Each move through the work teaches the reading speed (jumps and pauses are ignored).
        let (read, _) = self.words_read();
        if read != self.pace_words {
            self.pace_words = read;
            if self.pace.observe(self.opened.elapsed().as_secs_f64(), read as f64, &mut config.reading_speed) {
                out.push(ReaderAction::ConfigChanged);
            }
        }

        if chrome {
            let mark = ui.node_mark();
            self.toolbar(ui, Rect::new(r.x, r.y, r.w, TOOLBAR_H), config, &mut out);
            ui.tab_first(mark);
            if bottom > 0.0 {
                self.bottom_bar(ui, Rect::new(r.x, area.bottom(), r.w, bottom), &t, config.wpm());
            }
        }

        if self.panel == Panel::None {
            self.popover(ui, area, &mut out);
        } else {
            self.popover = None;
        }
        if let Some(pr) = panel_rect {
            ui.g.fill_rect(area, Color { a: 0.25, ..Color::from_u8(0, 0, 0) });
            ui.overlay(|ui| match self.panel {
                Panel::Contents => self.contents_panel(ui, pr, bookmarks, highlights, &mut out),
                Panel::Appearance => self.appearance_panel(ui, pr, config, &mut out),
                Panel::Search => self.search_panel(ui, pr),
                Panel::Bookmark => self.bookmark_panel(ui, pr, &mut out),
                Panel::Note => self.note_panel(ui, pr, &mut out),
                Panel::Define => self.define_panel(ui, pr),
                Panel::None => {}
            });
        }
        out
    }

    fn panel_rect(&self, r: Rect, area: Rect) -> Option<Rect> {
        let w = |want: f32| want.min(r.w - 24.0);
        match self.panel {
            Panel::None => None,
            Panel::Contents => Some(Rect::new(r.x, area.y, w(380.0), area.h)),
            Panel::Appearance => Some(Rect::new(r.right() - w(360.0), area.y, w(360.0), area.h)),
            Panel::Search => Some(Rect::new(r.right() - w(440.0) - 12.0, area.y + 10.0, w(440.0), 108.0)),
            Panel::Bookmark => Some(Rect::new(r.right() - w(420.0) - 12.0, area.y + 10.0, w(420.0), 210.0)),
            Panel::Note => Some(Rect::new(r.right() - w(440.0) - 12.0, area.y + 10.0, w(440.0), 226.0)),
            Panel::Define => Some(Rect::new(r.right() - w(400.0), area.y, w(400.0), area.h)),
        }
    }

    /// Closing search also clears its highlights.
    fn close_panel(&mut self) {
        if self.panel == Panel::Search {
            self.matches.clear();
            self.current_match = None;
            self.query.clear();
            self.searched = (String::new(), self.search_all);
        }
        self.panel = Panel::None;
    }

    fn open_panel(&mut self, p: Panel) {
        if self.panel == p {
            self.close_panel();
            return;
        }
        if self.panel == Panel::Search {
            self.close_panel();
        }
        self.panel = p;
        self.focus_pending = matches!(self.panel, Panel::Search | Panel::Bookmark);
        if self.panel == Panel::Bookmark {
            self.bookmark_note.clear();
        }
    }

    fn global_keys(&mut self, ui: &mut Ui, config: &mut AppConfig, out: &mut Vec<ReaderAction>) {
        if self.panel == Panel::None && ui.key_combo(VK_R, false, false) {
            self.toggle_aloud(out);
        }
        if ui.key_combo(VK_ESCAPE, false, false) {
            if self.aloud.is_some() {
                self.stop_aloud();
            } else if self.panel == Panel::None && (self.sel.is_some() || self.popover.is_some()) {
                self.sel = None;
                self.popover = None;
                ui.request_repaint();
            } else if self.panel != Panel::None {
                self.close_panel();
            } else if config.typography.focus_mode {
                config.typography.focus_mode = false;
                out.push(ReaderAction::ConfigChanged);
            } else if self.fullscreen {
                out.push(ReaderAction::ToggleFullscreen);
            }
        }
        if self.panel == Panel::None && ui.key_combo(VK_F, false, false) {
            self.toggle_focus(config, out);
        }
        if ui.key_combo(VK_F11, false, false) {
            out.push(ReaderAction::ToggleFullscreen);
        }
        if ui.key_combo(VK_F, true, false) {
            self.open_panel(Panel::Search);
        }
        if ui.key_combo(VK_B, true, false) {
            self.open_panel(Panel::Bookmark);
        }
        let t = &mut config.typography;
        if ui.key_combo(VK_OEM_PLUS, true, false) || ui.key_combo(VK_ADD, true, false) {
            t.font_size = (t.font_size + 1.0).min(36.0);
            out.push(ReaderAction::ConfigChanged);
        }
        if ui.key_combo(VK_OEM_MINUS, true, false) || ui.key_combo(VK_SUBTRACT, true, false) {
            t.font_size = (t.font_size - 1.0).max(12.0);
            out.push(ReaderAction::ConfigChanged);
        }
        if ui.key_combo(VK_RIGHT, true, false) && self.chapter + 1 < self.chapter_count() {
            self.go_chapter(self.chapter + 1, 0.0);
        }
        if ui.key_combo(VK_LEFT, true, false) && self.chapter > 0 {
            self.go_chapter(self.chapter - 1, 0.0);
        }
    }

    fn toggle_focus(&mut self, config: &mut AppConfig, out: &mut Vec<ReaderAction>) {
        let t = &mut config.typography;
        t.focus_mode = !t.focus_mode;
        out.push(ReaderAction::ConfigChanged);
        if t.focus_mode {
            out.push(ReaderAction::Notice("Focus mode: press F or Esc to bring back the menus".into()));
        }
    }

    // ---- read aloud --------------------------------------------------------------------

    fn toggle_aloud(&mut self, out: &mut Vec<ReaderAction>) {
        if self.aloud.is_some() {
            self.stop_aloud();
            return;
        }
        if self.voice.is_none() {
            self.voice = Voice::new();
        }
        if self.voice.is_none() {
            out.push(ReaderAction::Notice("Read aloud needs a Windows voice (Settings → Time & language → Speech)".into()));
            return;
        }
        self.aloud = Some(Aloud { chapter: self.chapter, block: None, word: None, page: self.page });
    }

    fn stop_aloud(&mut self) {
        self.aloud = None;
        if let Some(v) = &mut self.voice {
            v.stop();
        }
    }

    /// Starts saying block `b`.
    fn say(&mut self, b: usize, rate: f32) {
        let (Some(layout), Some(voice), Some(a)) = (&self.layout, &mut self.voice, &mut self.aloud) else { return };
        voice.speak(&layout.blocks[b].rich.text, rate);
        a.block = Some(b);
        a.word = None;
    }

    /// Keeps reading aloud going: follows the voice word by word, moves to the next block when
    /// one is done (turning pages and chapters as needed), and starts over from what's on
    /// screen when the reader moves somewhere else.
    fn read_aloud(&mut self, ui: &mut Ui, area: Rect, t: &TypographySettings, rate: f32) {
        let Some(mut a) = self.aloud else { return };
        let Some(layout) = &self.layout else { return };
        if layout.chapter != self.chapter {
            return;
        }
        let paged = t.reading_mode == ReadingMode::Paged;
        let first_on_screen = self.visible.iter().copied().find(|&b| speakable(layout.blocks[b].kind));
        let moved = a.chapter != self.chapter || (paged && a.page != self.page) || a.block.is_some_and(|b| !self.visible.contains(&b));
        if a.block.is_none() || moved {
            a = Aloud { chapter: self.chapter, block: None, word: None, page: self.page };
            self.aloud = Some(a);
            match first_on_screen {
                Some(b) => self.say(b, rate),
                None => {
                    // Nothing to read here (a picture page): go on.
                    self.aloud_next(ui, area, t, rate, None);
                }
            }
            ui.wake_after(60);
            return;
        }
        let Some(progress) = self.voice.as_ref().map(|v| v.progress()) else { return };
        match progress {
            Progress::Done => self.aloud_next(ui, area, t, rate, a.block),
            Progress::Starting => {}
            Progress::Word(s, e) => {
                if a.word != Some((s, e)) {
                    if let Some(x) = &mut self.aloud {
                        x.word = Some((s, e));
                    }
                    self.follow_word(ui, area, t);
                    ui.request_repaint();
                }
            }
        }
        if let Some(x) = &mut self.aloud {
            x.page = self.page;
        }
        ui.wake_after(60);
    }

    /// Goes on to the next block after `after` worth reading, or into the next chapter.
    fn aloud_next(&mut self, ui: &mut Ui, area: Rect, t: &TypographySettings, rate: f32, after: Option<usize>) {
        let Some(layout) = &self.layout else { return };
        let from = after.map_or(self.visible.last().map_or(0, |b| b + 1), |b| b + 1);
        match (from..layout.blocks.len()).find(|&b| speakable(layout.blocks[b].kind)) {
            Some(b) => {
                self.say(b, rate);
                self.follow_word(ui, area, t);
            }
            None if self.chapter + 1 < self.chapter_count() => {
                self.go_chapter(self.chapter + 1, 0.0);
                if let Some(a) = &mut self.aloud {
                    *a = Aloud { chapter: self.chapter, block: None, word: None, page: 0 };
                }
            }
            None => self.stop_aloud(),
        }
        ui.request_repaint();
    }

    /// Keeps the word being said on screen: turns the page, or scrolls.
    fn follow_word(&mut self, ui: &mut Ui, area: Rect, t: &TypographySettings) {
        let (Some(layout), Some(a)) = (&self.layout, self.aloud) else { return };
        let Some(bi) = a.block else { return };
        let b = &layout.blocks[bi];
        // Where the word (or the block's start) is in the chapter's column.
        let mut y = b.y + b.text_y;
        if let Some((s, e)) = a.word {
            let (style, opts) = layout.styles.get(b.style);
            let opts = TextOpts { wrap: Some(b.text_w), ..opts };
            let th = ui.g.layout_rich(&b.rich.text, &b.rich.spans, style, opts);
            if let Some(r) = ui.g.range_rects(th, &b.rich.text, s, e).first() {
                y += r.y + r.h / 2.0;
            }
        }
        if t.reading_mode == ReadingMode::Paged {
            let n = layout.pages.len();
            let dual = self.spread_shown;
            let last = (self.page + if dual { 1 } else { 0 }).min(n - 1);
            if last + 1 < n && y >= layout.pages[last + 1].y {
                self.turn(true, dual);
                if let Some(a) = &mut self.aloud {
                    a.page = self.page;
                }
            }
        } else {
            let sid = id2(id("reader-scroll"), self.chapter);
            if let Some(s) = ui.mem.scroll.get_mut(&sid) {
                let pad_top = 36.0;
                let at = y + pad_top;
                if at < s.offset + area.h * 0.15 || at > s.offset + area.h * 0.75 {
                    s.offset = (at - area.h * 0.35).max(0.0);
                }
            }
        }
    }

    // ---- reading ruler -----------------------------------------------------------------

    /// Screen rectangles of block `b`'s text lines when its top is drawn at `y`, within `clip`
    /// (top, bottom).
    fn block_lines(b: &Block, x: f32, y: f32, clip: (f32, f32), out: &mut Vec<Rect>) {
        let mut top = b.text_y;
        for &bottom in &b.lines {
            if y + bottom > clip.0 + 1.0 && y + top < clip.1 - 1.0 {
                out.push(Rect::new(x + b.text_x - 8.0, y + top, b.text_w + 16.0, bottom - top));
            }
            top = bottom;
        }
    }

    /// Moves the ruler a line (paged mode), turning the page at either end.
    fn ruler_step(&mut self, down: bool, dual: bool) {
        let n = self.ruler_lines.len();
        if down {
            if self.ruler + 1 < n {
                self.ruler += 1;
            } else {
                self.turn(true, dual);
                self.ruler = 0;
            }
        } else if self.ruler > 0 {
            self.ruler = self.ruler.min(n.saturating_sub(1)) - 1;
        } else {
            self.turn(false, dual);
            self.ruler = usize::MAX;
        }
    }

    /// Highlights one line and dims the rest of the page. Scrolling keeps the band still at a
    /// comfortable height and moves the text under it.
    fn draw_ruler(&mut self, ui: &mut Ui, area: Rect, scroll: bool) {
        let lines = &self.ruler_lines;
        if lines.is_empty() {
            return;
        }
        let c = ui.c;
        let idx = if scroll {
            let band_y = area.y + area.h * 0.38;
            lines
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1.y + a.1.h / 2.0 - band_y).abs().total_cmp(&(b.1.y + b.1.h / 2.0 - band_y).abs()))
                .map_or(0, |(i, _)| i)
        } else {
            self.ruler.min(lines.len() - 1)
        };
        self.ruler = idx;
        let l = lines[idx];
        let band = Rect::new(l.x, l.y - 3.0, l.w, l.h + 6.0);
        let dim = c.bg.alpha(0.62);
        ui.g.fill_rect(Rect::new(area.x, area.y, area.w, (band.y - area.y).max(0.0)), dim);
        ui.g.fill_rect(Rect::new(area.x, band.bottom(), area.w, (area.bottom() - band.bottom()).max(0.0)), dim);
        ui.g.fill_rect(Rect::new(area.x, band.y, (band.x - area.x).max(0.0), band.h), dim);
        ui.g.fill_rect(Rect::new(band.right(), band.y, (area.right() - band.right()).max(0.0), band.h), dim);
        ui.g.fill_round(Rect::new(band.x - 2.0, band.y + 2.0, 3.0, band.h - 4.0), 1.5, c.accent_mark);
    }

    // ---- paged mode --------------------------------------------------------------------

    fn turn(&mut self, forward: bool, dual: bool) {
        let step = if dual { 2 } else { 1 };
        let n = self.layout.as_ref().map_or(1, |l| l.pages.len());
        if forward {
            if self.page + step < n {
                self.page += step;
            } else if self.chapter + 1 < self.chapter_count() {
                self.go_chapter(self.chapter + 1, 0.0);
            }
        } else if self.page > 0 {
            self.page = self.page.saturating_sub(step);
        } else if self.chapter > 0 {
            self.go_chapter(self.chapter - 1, f32::MAX);
        }
        self.progress_dirty = true;
    }

    fn paged(&mut self, ui: &mut Ui, area: Rect, t: &TypographySettings) -> Option<EndAction> {
        let c = ui.c;
        let dual = t.page_layout == PageLayoutMode::Dual || (t.page_layout == PageLayoutMode::Auto && area.w >= 1100.0);
        let (margin, gutter) = (56.0, 80.0);
        let col_w = if dual { t.max_column_width.min((area.w - 2.0 * margin - gutter) / 2.0) } else { t.max_column_width.min(area.w - 2.0 * margin) }.max(240.0);
        let (head, foot) = (40.0, 44.0);
        let page_top = area.y + head;
        let page_h = (area.h - head - foot).max(120.0);

        // Navigation keys and the wheel act on the layout from the previous frame.
        if self.panel == Panel::None && self.layout.is_some() {
            // With the ruler on, up and down move it a line (and turn the page at the ends),
            // and it follows the mouse.
            let ruler = t.reading_ruler;
            if ruler {
                if ui.key_combo(VK_DOWN, false, false) {
                    self.ruler_step(true, dual);
                } else if ui.key_combo(VK_UP, false, false) {
                    self.ruler_step(false, dual);
                }
                let m = ui.input.mouse;
                if m != self.ruler_mouse && ui.hit(area) {
                    self.ruler_mouse = m;
                    if let Some(i) = self.ruler_lines.iter().position(|l| m.1 >= l.y - 4.0 && m.1 < l.bottom() + 4.0 && m.0 >= l.x && m.0 <= l.right()) {
                        self.ruler = i;
                    }
                }
            }
            let fwd = ui.key_combo(VK_RIGHT, false, false) || ui.key_combo(VK_NEXT, false, false) || ui.key_combo(VK_SPACE, false, false) || (!ruler && ui.key_combo(VK_DOWN, false, false));
            let back = ui.key_combo(VK_LEFT, false, false) || ui.key_combo(VK_PRIOR, false, false) || ui.key_combo(VK_SPACE, false, true) || (!ruler && ui.key_combo(VK_UP, false, false));
            if fwd {
                self.turn(true, dual);
            } else if back {
                self.turn(false, dual);
            }
            if ui.key_combo(VK_HOME, false, false) {
                self.page = 0;
                self.progress_dirty = true;
            }
            if ui.key_combo(VK_END, false, false) {
                self.page = usize::MAX;
                self.progress_dirty = true;
            }
            if t.page_flip_mouse_wheel && ui.input.wheel != 0.0 && ui.hit(area) {
                let ready = self.last_flip.is_none_or(|at| at.elapsed() >= FLIP_DEBOUNCE);
                self.wheel_acc = if ready { self.wheel_acc + ui.input.wheel } else { 0.0 };
                if self.wheel_acc.abs() >= 0.5 {
                    self.turn(self.wheel_acc < 0.0, dual);
                    self.wheel_acc = 0.0;
                    self.last_flip = Some(Instant::now());
                }
                ui.request_repaint();
            }
        }

        self.ensure_layout(ui, t, col_w, Some(page_h));
        let layout = self.layout.as_ref().unwrap();
        let n = layout.pages.len();
        if let Some(a) = self.restore.take() {
            self.page = layout.page_for_y(layout.y_for_anchor(a));
        }
        self.page = self.page.min(n - 1);
        if dual {
            self.page -= self.page % 2;
        }

        let total_w = if dual { col_w * 2.0 + gutter } else { col_w };
        let x0 = area.x + (area.w - total_w) / 2.0;
        let small = TextStyle::ui(12.0);
        let mut end = None;
        let mut lines = Vec::new();
        let mut visible = Vec::new();
        for k in 0..if dual { 2 } else { 1 } {
            let p = self.page + k;
            if p >= n {
                break;
            }
            let px = x0 + k as f32 * (col_w + gutter);
            // Running heads and page numbers stay hidden in focus mode.
            if !t.focus_mode {
                let head_text = if dual && k == 0 { self.work.meta.title.clone() } else { self.work.chapters[self.chapter].title.clone() };
                ui.text_in(&head_text, &small, Rect::new(px, area.y + 12.0, col_w, 20.0), Align::Center, c.muted.alpha(0.8));
            }
            let mut y = page_top;
            for s in &layout.pages[p].slices {
                let h = s.to - s.from;
                ui.g.push_clip(Rect::new(px - 30.0, y, col_w + 60.0, h));
                if let Some(e) = self.draw_block(ui, layout, s.block, px, y - s.from) {
                    end = Some(e);
                }
                ui.g.pop_clip();
                Self::block_lines(&layout.blocks[s.block], px, y - s.from, (y, y + h), &mut lines);
                if visible.last() != Some(&s.block) {
                    visible.push(s.block);
                }
                y += h;
            }
            if !t.focus_mode {
                ui.text_in(&format!("{}", p + 1), &small, Rect::new(px, area.bottom() - foot + 12.0, col_w, 20.0), Align::Center, c.muted);
            }
        }
        self.ruler_lines = lines;
        self.visible = visible;
        if dual {
            let mx = x0 + col_w + gutter / 2.0;
            ui.g.line(mx, page_top, mx, page_top + page_h, 1.0, c.border);
        }
        let pages_y = layout.pages[self.page].y;
        let last_visible = (self.page + if dual { 1 } else { 0 }).min(n - 1);
        self.spread_shown = last_visible > self.page;

        if t.show_page_turn_indicators && !t.focus_mode {
            let size = 40.0;
            let cy = area.y + area.h / 2.0 - size / 2.0;
            let can_back = self.page > 0 || self.chapter > 0;
            let can_fwd = last_visible + 1 < n || self.chapter + 1 < self.chapter_count();
            let left = Rect::new(area.x + 12.0, cy, size, size);
            let right = Rect::new(area.right() - 12.0 - size, cy, size, size);
            if can_back {
                let back = ui.button_ex(id("page-back"), left, "", Some("\u{E76B}"), ButtonKind::Ghost, true);
                ui.tip(id("page-back"), left, "Previous page");
                if back {
                    self.turn(false, dual);
                }
            }
            if can_fwd {
                let fwd = ui.button_ex(id("page-fwd"), right, "", Some("\u{E76C}"), ButtonKind::Ghost, true);
                ui.tip(id("page-fwd"), right, "Next page");
                if fwd {
                    self.turn(true, dual);
                }
            }
        }

        let anchor = if last_visible + 1 >= n {
            // Showing the chapter's last page counts as having read it.
            self.layout.as_ref().map_or(0.0, |l| l.para_count as f32)
        } else {
            self.layout.as_ref().map_or(0.0, |l| l.anchor_at(pages_y))
        };
        self.set_anchor(anchor);
        end
    }

    // ---- scroll mode -------------------------------------------------------------------

    fn scrolled(&mut self, ui: &mut Ui, area: Rect, t: &TypographySettings, auto_next: bool) -> Option<EndAction> {
        let col_w = t.max_column_width.min(area.w - 72.0).max(240.0);
        self.ensure_layout(ui, t, col_w, None);
        let sid = id2(id("reader-scroll"), self.chapter);
        let pad_top = 36.0;
        let total = self.layout.as_ref().map_or(0.0, |l| l.total_h) + pad_top + 24.0;

        if let Some(a) = self.restore.take() {
            let y = self.layout.as_ref().map_or(0.0, |l| l.y_for_anchor(a));
            let s = ui.mem.scroll.entry(sid).or_default();
            s.content_h = total;
            s.offset = if a >= f32::MAX / 2.0 { total } else { y };
        }
        let max = (total - area.h).max(0.0);
        if self.panel == Panel::None {
            let page = (area.h - 64.0).max(40.0);
            let delta = if ui.key_combo(VK_DOWN, false, false) {
                Some(56.0)
            } else if ui.key_combo(VK_UP, false, false) {
                Some(-56.0)
            } else if ui.key_combo(VK_NEXT, false, false) || ui.key_combo(VK_SPACE, false, false) {
                Some(page)
            } else if ui.key_combo(VK_PRIOR, false, false) || ui.key_combo(VK_SPACE, false, true) {
                Some(-page)
            } else if ui.key_combo(VK_HOME, false, false) {
                Some(-f32::MAX / 4.0)
            } else if ui.key_combo(VK_END, false, false) {
                Some(f32::MAX / 4.0)
            } else {
                None
            };
            if let Some(d) = delta {
                let s = ui.mem.scroll.entry(sid).or_default();
                s.offset = (s.offset + d).clamp(0.0, max);
            }
            // Wheeling down past the end continues into the next chapter.
            let at_end = ui.mem.scroll.get(&sid).is_some_and(|s| s.offset >= max - 1.0);
            if auto_next && at_end && ui.input.wheel < 0.0 && ui.hit(area) && self.chapter + 1 < self.chapter_count() {
                self.wheel_acc += ui.input.wheel;
                if self.wheel_acc <= -4.0 {
                    self.wheel_acc = 0.0;
                    self.go_chapter(self.chapter + 1, 0.0);
                    return None;
                }
            } else if ui.input.wheel > 0.0 {
                self.wheel_acc = 0.0;
            }
        }

        let layout = self.layout.as_ref().unwrap();
        let x = area.x + (area.w - col_w) / 2.0;
        let mut end = None;
        let mut lines = Vec::new();
        let mut visible = Vec::new();
        ui.scroll_area(sid, area, |ui, view, y0| {
            let top = y0 + pad_top;
            for (i, b) in layout.blocks.iter().enumerate() {
                if top + b.y + b.h < view.y || top + b.y > view.bottom() {
                    continue;
                }
                visible.push(i);
                if let Some(e) = self.draw_block(ui, layout, i, x, top + b.y) {
                    end = Some(e);
                }
                Self::block_lines(b, x, top + b.y, (view.y, view.bottom()), &mut lines);
            }
            total
        });
        self.ruler_lines = lines;
        self.visible = visible;
        let offset = ui.mem.scroll.get(&sid).map_or(0.0, |s| s.offset);
        let anchor = if offset >= max - 1.0 && max > 0.0 { layout.para_count as f32 } else { layout.anchor_at(offset) };
        self.set_anchor(anchor);
        end
    }

    // ---- blocks ------------------------------------------------------------------------

    fn draw_block(&self, ui: &mut Ui, layout: &ChapterLayout, bi: usize, x: f32, y: f32) -> Option<EndAction> {
        let c = ui.c;
        let b = &layout.blocks[bi];
        let w = layout.width;
        let card_label = |kind: Kind| match kind {
            Kind::Summary => "Chapter summary",
            Kind::PreNotes => "Notes",
            _ => "End notes",
        };
        match b.kind {
            Kind::Preface => {
                preface::draw(ui, &self.work.meta, x, y, w, layout.page_h);
                None
            }
            Kind::Divider => {
                let t = ui.measure("\u{2042}", &TextStyle::ui(18.0), TextOpts::default());
                ui.g.draw_text(t, x + (w - t.w) / 2.0, y + (b.text_h - t.h) / 2.0, c.muted);
                None
            }
            Kind::Image if matches!(b.extra, Extra::Image(Some(_))) => {
                let r = Rect::new(x + b.text_x, y + b.text_y, b.text_w, b.text_h);
                if let Extra::Image(Some(path)) = &b.extra {
                    if !ui.g.draw_image(path, r) {
                        ui.g.stroke_round(r, 6.0, 1.0, c.border);
                        let label = if b.rich.text.is_empty() { "Picture" } else { b.rich.text.as_str() };
                        ui.text_in(label, &TextStyle::ui(13.0), r, Align::Center, c.muted);
                    }
                }
                None
            }
            Kind::TableRow => {
                if let Extra::Row(cells, col_w, header) = &b.extra {
                    let row = Rect::new(x, y, w, b.text_h + 16.0);
                    if *header {
                        ui.g.fill_rect(row, c.text.alpha(0.07));
                    }
                    // Top edge only on a table's first row; every row draws its bottom edge.
                    let first = bi == 0 || layout.blocks[bi - 1].kind != Kind::TableRow;
                    if first {
                        ui.g.fill_rect(Rect::new(x, y, w, 1.0), c.border);
                    }
                    ui.g.fill_rect(Rect::new(x, row.bottom() - 1.0, w, 1.0), c.border);
                    let (style, opts) = layout.styles.get(b.style);
                    let style = if *header { style.clone().semibold() } else { style.clone() };
                    let opts = TextOpts { wrap: Some(b.text_w), ..opts };
                    for (ci, cell) in cells.iter().enumerate() {
                        let cx = x + ci as f32 * col_w;
                        if ci > 0 {
                            ui.g.fill_rect(Rect::new(cx, y, 1.0, row.h), c.border);
                        }
                        let th = ui.g.layout_rich(&cell.text, &cell.spans, &style, opts);
                        ui.g.draw_text(th, cx + b.text_x, y + b.text_y, c.text);
                    }
                    ui.g.fill_rect(Rect::new(x, y, 1.0, row.h), c.border);
                    ui.g.fill_rect(Rect::new(x + w - 1.0, y, 1.0, row.h), c.border);
                }
                None
            }
            Kind::End => {
                let last = self.chapter + 1 >= self.chapter_count();
                let label = if last { "The end" } else { "End of chapter" };
                ui.text_in(label, &TextStyle::ui(13.0), Rect::new(x, y + 24.0, w, 20.0), Align::Center, c.muted);
                // Kudos are an AO3 thing; other books just end.
                if last && !self.work.meta.is_ao3() {
                    return None;
                }
                let (text, icon) = if last { ("Leave kudos", "\u{EB51}") } else { ("Next chapter", "\u{E76C}") };
                let bw = ui.button_width(text, Some(icon));
                let r = Rect::new(x + (w - bw) / 2.0, y + 56.0, bw, 36.0);
                if ui.button_ex(id2(id("end-action"), self.chapter), r, text, Some(icon), ButtonKind::Primary, true) {
                    return Some(if last { EndAction::Kudos } else { EndAction::NextChapter });
                }
                None
            }
            kind => {
                if matches!(kind, Kind::Summary | Kind::PreNotes | Kind::EndNotes) {
                    let top = if kind == Kind::EndNotes { 16.0 } else { 0.0 };
                    let card = Rect::new(x, y + top, w, b.text_y + b.text_h + CARD_PAD - top);
                    ui.g.fill_round(card, 8.0, c.notes_bg);
                    ui.g.stroke_round(card, 8.0, 1.0, c.border);
                    ui.text(card_label(kind), &TextStyle::ui(12.5).semibold(), x + CARD_PAD, y + top + CARD_PAD - 2.0, c.accent_text);
                }
                if kind == Kind::Quote {
                    let r = Rect::new(x + 6.0, y, w - 6.0, b.text_y + b.text_h + 12.0);
                    ui.g.fill_round(r, 6.0, c.quote_bg);
                    ui.g.fill_round(Rect::new(r.x, r.y, 3.0, r.h), 1.5, c.text.alpha(0.22));
                }
                if kind == Kind::Pre {
                    let r = Rect::new(x, y, w, b.text_y + b.text_h + 12.0);
                    ui.g.fill_round(r, 6.0, c.quote_bg);
                    ui.g.stroke_round(r, 6.0, 1.0, c.border);
                }
                if let Extra::Marker(marker) = &b.extra {
                    let (style, _) = layout.styles.get(b.style);
                    let m = ui.g.layout(marker, style, TextOpts::default());
                    ui.g.draw_text(m, x + b.text_x - 10.0 - m.w, y + b.text_y, c.muted);
                }
                let (style, opts) = layout.styles.get(b.style);
                let opts = TextOpts { wrap: Some(b.text_w), ..opts };
                let th = ui.g.layout_rich(&b.rich.text, &b.rich.spans, style, opts);
                let (tx, ty) = (x + b.text_x, y + b.text_y);
                if kind.searchable() {
                    let shift = Self::shift(b);
                    let len = b.rich.text.len().saturating_sub(shift);
                    for h in &self.highlights {
                        if let Some((s, e)) = h.span_in(b.para, len) {
                            for rr in ui.g.range_rects(th, &b.rich.text, s + shift, e + shift) {
                                ui.g.fill_rect(Rect::new(tx + rr.x, ty + rr.y, rr.w, rr.h), highlight_color(h.color, c.is_dark));
                            }
                        }
                    }
                    if let Some((s, e)) = self.sel_in(bi, b.rich.text.len()) {
                        for rr in ui.g.range_rects(th, &b.rich.text, s, e) {
                            ui.g.fill_rect(Rect::new(tx + rr.x, ty + rr.y, rr.w, rr.h), c.accent_mark.alpha(0.3));
                        }
                    }
                    let whole = Rect::new(tx, ty, b.text_w, b.text_h);
                    let shown = ui.g.clip().map_or(whole, |cl| whole.intersect(&cl));
                    if shown.w > 0.0 && shown.h > 0.0 {
                        self.spots.borrow_mut().push(Spot { block: bi, x: tx, y: ty, shown, text: th });
                    }
                }
                if let Some(Aloud { chapter, block: Some(ab), word: Some((s, e)), .. }) = self.aloud {
                    if chapter == layout.chapter && ab == bi {
                        for rr in ui.g.range_rects(th, &b.rich.text, s, e) {
                            ui.g.fill_round(Rect::new(tx + rr.x - 2.0, ty + rr.y, rr.w + 4.0, rr.h), 4.0, c.accent_mark.alpha(0.3));
                        }
                    }
                }
                if kind.searchable() && !self.matches.is_empty() {
                    let shift = if kind == Kind::Para && b.rich.text.starts_with(INDENT) { INDENT.len() } else { 0 };
                    for (mi, m) in self.matches.iter().enumerate() {
                        if m.chapter != layout.chapter || m.para != b.para {
                            continue;
                        }
                        let current = self.current_match == Some(mi);
                        let fill = c.accent.alpha(if current { 0.55 } else { 0.22 });
                        for rr in ui.g.range_rects(th, &b.rich.text, m.start + shift, m.end + shift) {
                            ui.g.fill_round(Rect::new(tx + rr.x - 1.0, ty + rr.y, rr.w + 2.0, rr.h), 3.0, fill);
                        }
                    }
                }
                let color = match kind {
                    Kind::Summary | Kind::PreNotes | Kind::EndNotes => c.text.alpha(0.9),
                    // A picture that couldn't be shown: its description, quietly.
                    Kind::Image => c.muted,
                    _ => c.text,
                };
                ui.g.draw_text(th, tx, ty, color);
                None
            }
        }
    }

    // ---- chrome ------------------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut Ui, r: Rect, config: &mut AppConfig, out: &mut Vec<ReaderAction>) {
        let c = ui.c;
        ui.g.fill_rect(r, c.surface);
        ui.separator(r.x, r.bottom() - 1.0, r.w);
        let b = |x: f32| Rect::new(x, r.y + 8.0, 34.0, 32.0);
        if ui.button_ex(id("rd-back"), Rect::new(r.x + 8.0, r.y + 8.0, 96.0, 32.0), "Library", Some("\u{E72B}"), ButtonKind::Ghost, true) {
            out.push(ReaderAction::Close);
        }
        if ui.icon_button(id("rd-contents"), b(r.x + 108.0), "\u{E8FD}", "Contents and bookmarks", self.panel == Panel::Contents) {
            self.open_panel(Panel::Contents);
        }

        let mut x = r.right() - 8.0 - 34.0;
        let paged = config.typography.reading_mode == ReadingMode::Paged;
        let fs_icon = if self.fullscreen { "\u{E73F}" } else { "\u{E740}" };
        let fs_tip = if self.fullscreen { "Leave full screen (F11)" } else { "Full screen (F11)" };
        if ui.icon_button(id("rd-full"), b(x), fs_icon, fs_tip, false) {
            out.push(ReaderAction::ToggleFullscreen);
        }
        x -= 38.0;
        if ui.icon_button(id("rd-focus"), b(x), "\u{E890}", "Focus mode: just the text (F)", false) {
            self.toggle_focus(config, out);
        }
        x -= 38.0;
        let reading = self.aloud.is_some();
        let tip = if reading { "Stop reading aloud (R)" } else { "Read aloud (R)" };
        if ui.icon_button(id("rd-aloud"), b(x), "\u{E767}", tip, reading) {
            self.toggle_aloud(out);
        }
        x -= 38.0;
        let mode_tip = if paged { "Switch to scrolling" } else { "Switch to pages" };
        if ui.icon_button(id("rd-mode"), b(x), if paged { "\u{E8A1}" } else { "\u{E8A9}" }, mode_tip, false) {
            config.typography.reading_mode = if paged { ReadingMode::Scroll } else { ReadingMode::Paged };
            out.push(ReaderAction::ConfigChanged);
        }
        x -= 38.0;
        if ui.icon_button(id("rd-appearance"), b(x), icons::FONT, "Text and theme", self.panel == Panel::Appearance) {
            self.open_panel(Panel::Appearance);
        }
        if self.work.meta.is_ao3() {
            x -= 38.0;
            if ui.icon_button(id("rd-kudos"), b(x), "\u{EB51}", "Leave kudos on AO3", false) {
                out.push(ReaderAction::Kudos);
            }
        }
        x -= 38.0;
        if ui.icon_button(id("rd-bookmark"), b(x), "\u{E734}", "Bookmark this page (Ctrl+B)", self.panel == Panel::Bookmark) {
            self.open_panel(Panel::Bookmark);
        }
        x -= 38.0;
        if ui.icon_button(id("rd-search"), b(x), icons::SEARCH, "Search in this work (Ctrl+F)", self.panel == Panel::Search) {
            self.open_panel(Panel::Search);
        }

        let title_x = r.x + 156.0;
        let title_w = (x - 12.0 - title_x).max(0.0);
        let ch = &self.work.chapters[self.chapter];
        let title = if self.chapter_count() > 1 { format!("{}  ·  {}", self.work.meta.title, ch.title) } else { self.work.meta.title.clone() };
        ui.text_in(&title, &TextStyle::ui(14.0).semibold(), Rect::new(title_x, r.y, title_w, r.h), Align::Center, c.text);
    }

    fn bottom_bar(&mut self, ui: &mut Ui, r: Rect, t: &TypographySettings, wpm: u32) {
        let c = ui.c;
        ui.g.fill_rect(r, c.surface);
        let p = self.progress();
        ui.g.fill_rect(Rect::new(r.x, r.y, r.w, 3.0), c.border);
        ui.g.fill_rect(Rect::new(r.x, r.y, r.w * p.percentage / 100.0, 3.0), c.accent_mark);

        let n = self.chapter_count();
        let has_prev = self.chapter > 0;
        let has_next = self.chapter + 1 < n;
        let prev_r = Rect::new(r.x + 8.0, r.y + 7.0, 34.0, 32.0);
        if ui.button_ex(id("ch-prev"), prev_r, "", Some("\u{E76B}"), ButtonKind::Ghost, has_prev) {
            self.go_chapter(self.chapter - 1, 0.0);
        }
        ui.tip(id("ch-prev"), prev_r, "Previous chapter (Ctrl+←)");
        let next_r = Rect::new(r.right() - 42.0, r.y + 7.0, 34.0, 32.0);
        if ui.button_ex(id("ch-next"), next_r, "", Some("\u{E76C}"), ButtonKind::Ghost, has_next) {
            self.go_chapter(self.chapter + 1, 0.0);
        }
        ui.tip(id("ch-next"), next_r, "Next chapter (Ctrl+→)");

        let mut left = format!("Chapter {} of {}", self.chapter + 1, n);
        if t.reading_mode == ReadingMode::Paged {
            if let Some(l) = &self.layout {
                let n = l.pages.len();
                if self.spread_shown {
                    left.push_str(&format!("  ·  Pages {}–{} of {}", self.page + 1, self.page + 2, n));
                } else {
                    left.push_str(&format!("  ·  Page {} of {}", self.page + 1, n));
                }
            }
        }
        let style = TextStyle::ui(12.5);
        ui.text_in(&left, &style, Rect::new(r.x + 50.0, r.y + 3.0, r.w / 2.0 - 50.0, r.h - 3.0), Align::Left, c.muted);

        let total: usize = self.chapter_words.iter().sum();
        let (read, in_ch) = self.words_read();
        let wpm = wpm.max(60) as f32;
        let left_work = (total as f32 - read).max(0.0) / wpm;
        let left_ch = (self.chapter_words.get(self.chapter).copied().unwrap_or(0) as f32 - in_ch).max(0.0) / wpm;
        let right = if p.completed {
            "Finished".to_string()
        } else if n > 1 {
            format!("{:.0}%  ·  {} left  ·  {} in chapter", p.percentage, fmt_minutes(left_work), fmt_minutes(left_ch))
        } else {
            format!("{:.0}%  ·  {} left", p.percentage, fmt_minutes(left_work))
        };
        ui.text_in(&right, &style, Rect::new(r.x + r.w / 2.0, r.y + 3.0, r.w / 2.0 - 50.0, r.h - 3.0), Align::Right, c.muted);
    }

    // ---- panels ------------------------------------------------------------------------

    fn panel_frame(ui: &mut Ui, r: Rect) {
        let c = ui.c;
        ui.g.fill_round(r, 8.0, c.surface);
        ui.g.stroke_round(r, 8.0, 1.0, c.border);
    }

    fn contents_panel(&mut self, ui: &mut Ui, r: Rect, bookmarks: &[Bookmark], highlights: &[Highlight], out: &mut Vec<ReaderAction>) {
        let c = ui.c;
        Self::panel_frame(ui, r);
        // AO3 works have their comments too.
        let mut tabs = vec![("Chapters", 0), ("Bookmarks", bookmarks.len()), ("Highlights", highlights.len())];
        if self.work.meta.is_ao3() {
            tabs.push(("Comments", 0));
        } else if self.contents_tab == 3 {
            self.contents_tab = 0;
        }
        ui.segmented_counts(id("contents-tab"), Rect::new(r.x + 14.0, r.y + 14.0, r.w - 28.0, 34.0), &tabs, &mut self.contents_tab, Badge::Count);
        let list = Rect::new(r.x + 4.0, r.y + 60.0, r.w - 8.0, r.h - 64.0);
        let mut jump: Option<(usize, f32)> = None;

        if self.contents_tab == 3 {
            self.comments_list(ui, list, out);
        } else if self.contents_tab == 2 {
            if let Some(j) = self.highlights_list(ui, list, highlights, out) {
                jump = Some(j);
            }
        } else if self.contents_tab == 0 {
            let row_h = 44.0;
            let chapters = &self.work.chapters;
            let current = self.chapter;
            let words = &self.chapter_words;
            ui.scroll_area(id("contents-list"), list, |ui, view, y0| {
                for (i, ch) in chapters.iter().enumerate() {
                    let rr = Rect::new(view.x + 6.0, y0 + i as f32 * row_h, view.w - 22.0, row_h - 2.0);
                    if rr.bottom() < view.y || rr.y > view.bottom() {
                        continue;
                    }
                    let resp = ui.interact_as(id2(id("toc-row"), i), rr, Role::Choice(i == current), &ch.title);
                    if i == current {
                        ui.g.fill_round(rr, 6.0, c.selected());
                    } else if resp.hovered {
                        ui.g.fill_round(rr, 6.0, c.hover());
                    }
                    let wtxt = format!("{} words", preface::thousands(words.get(i).copied().unwrap_or(0) as u64));
                    let wt = ui.text_in(&wtxt, &TextStyle::ui(11.5), Rect::new(rr.right() - 100.0, rr.y, 92.0, rr.h), Align::Right, c.muted);
                    let style = if i == current { TextStyle::ui(13.5).semibold() } else { TextStyle::ui(13.5) };
                    let label = if ch.title.trim().is_empty() { format!("Chapter {}", i + 1) } else { ch.title.trim().to_string() };
                    ui.text_in(&label, &style, Rect::new(rr.x + 12.0, rr.y, rr.w - wt.w - 32.0, rr.h), Align::Left, c.text);
                    if resp.clicked {
                        jump = Some((i, 0.0));
                    }
                }
                chapters.len() as f32 * row_h + 8.0
            });
        } else {
            let mut sorted: Vec<&Bookmark> = bookmarks.iter().collect();
            sorted.sort_by(|a, b| (a.chapter_index, a.paragraph_index).cmp(&(b.chapter_index, b.paragraph_index)));
            if sorted.is_empty() {
                let msg = "No bookmarks yet. Press Ctrl+B or the bookmark button to add one.";
                let t = ui.measure(msg, &TextStyle::ui(13.0), TextOpts::wrap(list.w - 48.0).align(Align::Center));
                ui.g.draw_text(t, list.x + 24.0, list.y + 32.0, c.muted);
            }
            let row_h = 78.0;
            let mut remove = None;
            ui.scroll_area(id("bookmark-list"), list, |ui, view, y0| {
                for (i, bm) in sorted.iter().enumerate() {
                    let rr = Rect::new(view.x + 6.0, y0 + i as f32 * row_h, view.w - 22.0, row_h - 4.0);
                    if rr.bottom() < view.y || rr.y > view.bottom() {
                        continue;
                    }
                    let del = Rect::new(rr.right() - 38.0, rr.y + 8.0, 32.0, 30.0);
                    let deleted = ui.button_ex(id2(id("bm-del"), &bm.id), del, "", Some(icons::DELETE), ButtonKind::Ghost, true);
                    ui.tip(id2(id("bm-del"), &bm.id), del, "Delete bookmark");
                    if deleted {
                        remove = Some(bm.id.clone());
                    }
                    let resp = ui.interact(id2(id("bm-row"), &bm.id), Rect::new(rr.x, rr.y, rr.w - 44.0, rr.h));
                    ui.g.stroke_round(rr, 6.0, 1.0, c.border);
                    if resp.hovered {
                        ui.g.fill_round(rr, 6.0, c.hover());
                    }
                    ui.text(&format!("Chapter {}", bm.chapter_index), &TextStyle::ui(12.5).semibold(), rr.x + 12.0, rr.y + 9.0, c.accent_text);
                    let body = if bm.note.trim().is_empty() { bm.excerpt.as_str() } else { bm.note.as_str() };
                    let t = ui.measure(body, &TextStyle::ui(13.0), TextOpts::wrap(rr.w - 60.0).max_lines(2));
                    ui.g.draw_text(t, rr.x + 12.0, rr.y + 30.0, c.text);
                    if resp.clicked {
                        jump = Some((bm.chapter_index.saturating_sub(1), bm.paragraph_index as f32));
                    }
                }
                sorted.len() as f32 * row_h + 8.0
            });
            if let Some(bid) = remove {
                out.push(ReaderAction::RemoveBookmark(bid));
            }
        }
        if let Some((ch, anchor)) = jump {
            self.go_chapter(ch, anchor);
            self.panel = Panel::None;
        }
    }

    /// The chapter's AO3 comments, a page at a time, and a box to write one.
    fn comments_list(&mut self, ui: &mut Ui, list: Rect, out: &mut Vec<ReaderAction>) {
        let c = ui.c;
        let key = self.work.chapters.get(self.chapter).and_then(|ch| ch.id.clone()).unwrap_or_default();
        let chapter_id = (!key.is_empty()).then(|| key.clone());
        if self.comments.key.as_deref() != Some(key.as_str()) {
            let draft = std::mem::take(&mut self.comments.draft);
            self.comments = Comments { key: Some(key.clone()), loading: true, draft, ..Default::default() };
            out.push(ReaderAction::LoadComments { chapter_id: chapter_id.clone(), page: 1 });
        }
        let foot_h = 64.0;
        let area = Rect::new(list.x, list.y, list.w, (list.h - foot_h).max(0.0));
        let whose = if self.work.chapters.len() > 1 { format!("Comments on chapter {}", self.chapter + 1) } else { "Comments on this work".to_string() };
        ui.text(&whose, &TextStyle::ui(12.5).semibold(), area.x + 14.0, area.y + 2.0, c.muted);
        let body = Rect::new(area.x, area.y + 24.0, area.w, (area.h - 24.0).max(0.0));
        let cm = &self.comments;
        let mut more = false;
        if cm.list.is_empty() {
            let msg = if cm.loading {
                "Loading comments from AO3…".to_string()
            } else if let Some(e) = &cm.error {
                e.clone()
            } else {
                "No comments yet.".to_string()
            };
            let t = ui.measure(&msg, &TextStyle::ui(13.0), TextOpts::wrap(body.w - 48.0).align(Align::Center));
            let th = t.h;
            ui.g.draw_text(t, body.x + 24.0, body.y + 24.0, c.muted);
            if cm.loading {
                ui.spinner(body.x + body.w / 2.0, body.y + 24.0 + th + 20.0, 7.0);
            }
        } else {
            let (has_next, loading) = (cm.has_next, cm.loading);
            ui.scroll_area(id2(id("comments-list"), &key), body, |ui, view, y0| {
                let mut y = y0 + 4.0;
                for (i, m) in cm.list.iter().enumerate() {
                    let indent = m.depth.min(4) as f32 * 18.0;
                    let x = view.x + 14.0 + indent;
                    let w = (view.w - 36.0 - indent).max(80.0);
                    let name = if m.author.is_empty() { "Someone" } else { m.author.as_str() };
                    let head = ui.text(name, &TextStyle::ui(13.0).semibold(), x, y, c.text);
                    let when = [m.chapter.as_str(), m.date.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("  ·  ");
                    ui.text_opts(&when, &TextStyle::ui(11.5), TextOpts::ellipsize((w - head.w - 12.0).max(0.0)), x + head.w + 10.0, y + 1.5, c.muted);
                    let top = y;
                    y += head.h + 6.0;
                    for para in &m.paragraphs {
                        let t = ui.measure(para, &TextStyle::ui(13.0), TextOpts::wrap(w).line_height(1.4));
                        let th = t.h;
                        if y + th > view.y && y < view.bottom() {
                            ui.g.draw_text(t, x, y, c.text);
                        }
                        y += th + 6.0;
                    }
                    // Replies hang off a line under what they answer.
                    if indent > 0.0 {
                        ui.g.fill_rect(Rect::new(x - 10.0, top, 2.0, (y - top - 6.0).max(0.0)), c.border);
                    }
                    y += 10.0;
                    if i + 1 < cm.list.len() {
                        ui.separator(view.x + 14.0, y - 6.0, view.w - 36.0);
                        y += 6.0;
                    }
                }
                if has_next {
                    let bw = ui.button_width("Load more", None);
                    if ui.button_ex(id("comments-more"), Rect::new(view.x + (view.w - bw) / 2.0, y + 4.0, bw, 32.0), "Load more", None, ButtonKind::Secondary, !loading) {
                        more = true;
                    }
                    y += 44.0;
                }
                y - y0 + 8.0
            });
        }
        if more {
            self.comments.loading = true;
            out.push(ReaderAction::LoadComments { chapter_id: chapter_id.clone(), page: self.comments.page + 1 });
        }

        // Writing one: AO3 only takes comments here from signed-in readers.
        let fy = list.bottom() - foot_h + 14.0;
        ui.separator(list.x + 10.0, fy - 8.0, list.w - 20.0);
        if !self.can_comment {
            let t = ui.measure("Sign in to AO3 under Accounts to leave a comment.", &TextStyle::ui(12.5), TextOpts::wrap(list.w - 28.0));
            ui.g.draw_text(t, list.x + 14.0, fy + 8.0, c.muted);
            return;
        }
        let pw = 80.0;
        let input = Rect::new(list.x + 10.0, fy, list.w - 28.0 - pw, 34.0);
        let resp = ui.text_input(id("comment-draft"), input, &mut self.comments.draft, "Write a comment…", false);
        let can = !self.comments.posting && !self.comments.draft.trim().is_empty();
        let label = if self.comments.posting { "Posting…" } else { "Post" };
        if ui.button_ex(id("comment-post"), Rect::new(input.right() + 8.0, fy, pw, 34.0), label, None, ButtonKind::Primary, can) || (can && resp.submitted) {
            self.comments.posting = true;
            out.push(ReaderAction::PostComment { chapter_id, text: self.comments.draft.trim().to_string() });
        }
    }

    /// A page of comments arrived for `key` (a chapter's AO3 id, or "" for the whole work).
    pub fn comments_loaded(&mut self, key: &str, result: Result<CommentPage, String>) {
        if self.comments.key.as_deref() != Some(key) {
            return;
        }
        self.comments.loading = false;
        match result {
            Ok(p) => {
                if p.page <= 1 {
                    self.comments.list = p.comments;
                } else {
                    self.comments.list.extend(p.comments);
                }
                self.comments.page = p.page;
                self.comments.has_next = p.has_next;
                self.comments.error = None;
            }
            Err(e) => self.comments.error = Some(e),
        }
    }

    /// A comment was posted (or wasn't): on success, the list loads again with it.
    pub fn comment_posted(&mut self, result: &Result<(), String>) {
        self.comments.posting = false;
        if result.is_ok() {
            self.comments.draft.clear();
            self.comments.key = None;
        }
    }

    fn appearance_panel(&mut self, ui: &mut Ui, r: Rect, config: &mut AppConfig, out: &mut Vec<ReaderAction>) {
        let c = ui.c;
        Self::panel_frame(ui, r);
        let x = r.x + 18.0;
        let w = r.w - 36.0;
        let mut y = r.y + 16.0;
        let mut changed = false;
        let label = |ui: &mut Ui, y: &mut f32, s: &str| {
            let t = ui.text(s, &TextStyle::ui(12.5).semibold(), x, *y, c.muted);
            *y += t.h + 8.0;
        };

        label(ui, &mut y, "Theme");
        let themes = ReaderThemeType::all();
        let sw = (w / themes.len() as f32).min(48.0);
        for (i, th) in themes.iter().enumerate() {
            let p = config.palette_of(*th);
            let cx = x + sw * i as f32 + sw / 2.0;
            let rr = Rect::new(cx - 17.0, y, 34.0, 34.0);
            let resp = ui.interact_as(id2(id("ap-theme"), i), rr, Role::Choice(config.theme == *th), th.display_name());
            if config.theme == *th {
                ui.g.fill_circle(cx, y + 17.0, 17.0, c.text);
            } else if resp.hovered {
                ui.g.fill_circle(cx, y + 17.0, 17.0, c.text.alpha(0.3));
            }
            ui.g.fill_circle(cx, y + 17.0, 14.0, Color::rgb(p.bg));
            ui.g.fill_circle(cx + 5.0, y + 22.0, 4.0, Color::rgb(p.accent));
            if resp.clicked {
                // While following Windows' light or dark mode, the pick is for the mode in use;
                // otherwise the next change of mode would undo it.
                if config.follow_system {
                    if config.palette_of(config.theme).is_dark {
                        config.dark_theme = *th;
                    } else {
                        config.light_theme = *th;
                    }
                }
                config.theme = *th;
                changed = true;
            }
        }
        y += 48.0;

        let t = &mut config.typography;
        label(ui, &mut y, "Text size");
        let row = Rect::new(x, y, w, 34.0);
        if ui.button(id("ap-smaller"), Rect::new(row.x, row.y, 56.0, row.h), "A−") && t.font_size > 12.0 {
            t.font_size -= 1.0;
            changed = true;
        }
        if ui.button(id("ap-larger"), Rect::new(row.right() - 56.0, row.y, 56.0, row.h), "A+") && t.font_size < 36.0 {
            t.font_size += 1.0;
            changed = true;
        }
        ui.text_in(&format!("{:.0} px", t.font_size), &TextStyle::ui(14.0), row, Align::Center, c.text);
        y += 46.0;

        label(ui, &mut y, &format!("Line height  {:.2}×", t.line_spacing_multiplier));
        changed |= ui.slider(id("ap-line"), Rect::new(x - 6.0, y, w + 12.0, 26.0), &mut t.line_spacing_multiplier, 1.2, 2.6, 0.05);
        y += 34.0;
        label(ui, &mut y, &format!("Column width  {:.0} px", t.max_column_width));
        changed |= ui.slider(id("ap-width"), Rect::new(x - 6.0, y, w + 12.0, 26.0), &mut t.max_column_width, 450.0, 1400.0, 10.0);
        y += 34.0;
        label(ui, &mut y, &format!("Paragraph spacing  {:.0} px", t.paragraph_spacing));
        changed |= ui.slider(id("ap-para"), Rect::new(x - 6.0, y, w + 12.0, 26.0), &mut t.paragraph_spacing, 4.0, 32.0, 1.0);
        y += 38.0;

        label(ui, &mut y, "Alignment");
        let mut align = if t.text_alignment == TextAlignment::Justified { 1 } else { 0 };
        if ui.segmented(id("ap-align"), Rect::new(x, y, w, 34.0), &["Left", "Justified"], &mut align) {
            t.text_alignment = if align == 1 { TextAlignment::Justified } else { TextAlignment::Left };
            changed = true;
        }
        y += 46.0;
        label(ui, &mut y, "Layout");
        let mut mode = if t.reading_mode == ReadingMode::Paged { 0 } else { 1 };
        if ui.segmented(id("ap-mode"), Rect::new(x, y, w, 34.0), &["Pages", "Scroll"], &mut mode) {
            t.reading_mode = if mode == 0 { ReadingMode::Paged } else { ReadingMode::Scroll };
            changed = true;
        }
        y += 42.0;
        if t.reading_mode == ReadingMode::Paged {
            let mut spread = match t.page_layout {
                PageLayoutMode::Auto => 0,
                PageLayoutMode::Single => 1,
                PageLayoutMode::Dual => 2,
            };
            if ui.segmented(id("ap-spread"), Rect::new(x, y, w, 34.0), &["Auto", "Single", "Two pages"], &mut spread) {
                t.page_layout = [PageLayoutMode::Auto, PageLayoutMode::Single, PageLayoutMode::Dual][spread];
                changed = true;
            }
            y += 46.0;
        }

        label(ui, &mut y, "Font");
        let font = reader_font_label(t);
        let fr = Rect::new(x, y, w, 34.0);
        ui.text_in(&font, &TextStyle::ui(14.0), Rect::new(fr.x, fr.y, fr.w - 100.0, fr.h), Align::Left, c.text);
        if ui.button(id("ap-fonts"), Rect::new(fr.right() - 92.0, fr.y, 92.0, fr.h), "Change…") {
            out.push(ReaderAction::OpenTypographySettings);
        }
        if changed {
            out.push(ReaderAction::ConfigChanged);
        }
    }

    fn run_search(&mut self) {
        self.searched = (self.query.clone(), self.search_all);
        self.matches.clear();
        self.current_match = None;
        let q = self.query.trim();
        if q.chars().count() < 2 {
            return;
        }
        let Ok(re) = regex::RegexBuilder::new(&regex::escape(q)).case_insensitive(true).build() else { return };
        // Search from the current chapter onwards and wrap around, so the matches nearest the
        // reader are found even when a common word hits the match cap.
        let n = self.chapter_count();
        let order: Vec<usize> = if self.search_all { (self.chapter..n).chain(0..self.chapter).collect() } else { vec![self.chapter] };
        'outer: for ci in order {
            for (pi, p) in self.work.chapters[ci].paragraphs.iter().enumerate() {
                if is_landmark(pi, p) {
                    continue;
                }
                // Only text the page highlights counts (not a picture's description, say), so
                // "3 of 12" always lands on something shown.
                let Some((kind, rich)) = paragraph_rich(p) else { continue };
                if !kind.searchable() {
                    continue;
                }
                for m in re.find_iter(&rich.text) {
                    self.matches.push(Match { chapter: ci, para: pi, start: m.start(), end: m.end() });
                    if self.matches.len() >= MAX_MATCHES {
                        break 'outer;
                    }
                }
            }
        }
        // Start from the first match at or after the reading position.
        let para = self.anchor.floor() as usize;
        let first = self.matches.iter().position(|m| m.chapter != self.chapter || m.para >= para).or(if self.matches.is_empty() { None } else { Some(0) });
        if let Some(k) = first {
            self.goto_match(k);
        }
    }

    fn goto_match(&mut self, k: usize) {
        let Some(m) = self.matches.get(k).copied() else { return };
        self.current_match = Some(k);
        let len = paragraph_rich(&self.work.chapters[m.chapter].paragraphs[m.para]).map_or(1, |(_, r)| r.text.len().max(1));
        let anchor = m.para as f32 + (m.start as f32 / len as f32 * 0.9).min(0.9);
        self.go_chapter(m.chapter, anchor);
    }

    fn search_panel(&mut self, ui: &mut Ui, r: Rect) {
        let c = ui.c;
        Self::panel_frame(ui, r);
        let input = Rect::new(r.x + 14.0, r.y + 14.0, r.w - 28.0, 36.0);
        let sid = id("rd-search-input");
        let resp = ui.text_input(sid, input, &mut self.query, "Find in this work", false);
        if self.focus_pending {
            ui.focus_input(sid);
            self.focus_pending = false;
        }
        if resp.escaped {
            self.close_panel();
            return;
        }
        let y = r.y + 62.0;
        let mut all = self.search_all;
        ui.toggle(id("rd-search-all"), r.x + 16.0, y + 8.0, &mut all);
        self.search_all = all;
        ui.text("All chapters", &TextStyle::ui(13.0), r.x + 64.0, y + 8.0, c.text);
        if self.searched != (self.query.clone(), self.search_all) {
            self.run_search();
        }

        let n = self.matches.len();
        let count = match (self.current_match, n) {
            (_, 0) if self.query.trim().chars().count() >= 2 => "No matches".to_string(),
            (_, 0) => String::new(),
            (Some(k), _) => format!("{} of {}{}", k + 1, n, if n >= MAX_MATCHES { "+" } else { "" }),
            (None, _) => format!("{n} matches"),
        };
        let nav_x = r.right() - 14.0 - 34.0 * 3.0 - 8.0;
        ui.text_in(&count, &TextStyle::ui(13.0), Rect::new(r.x + 170.0, y, nav_x - r.x - 178.0, 34.0), Align::Right, if n == 0 { c.danger } else { c.muted });
        let has = n > 0;
        let next_r = Rect::new(nav_x + 38.0, y, 34.0, 34.0);
        let prev_r = Rect::new(nav_x, y, 34.0, 34.0);
        let next_hit = ui.button_ex(id("rd-search-next"), next_r, "", Some("\u{E70D}"), ButtonKind::Ghost, has);
        ui.tip(id("rd-search-next"), next_r, "Next match (Enter)");
        let next = resp.submitted || next_hit;
        let prev = ui.button_ex(id("rd-search-prev"), prev_r, "", Some("\u{E70E}"), ButtonKind::Ghost, has);
        ui.tip(id("rd-search-prev"), prev_r, "Previous match");
        if has && next {
            let k = self.current_match.map_or(0, |k| (k + 1) % n);
            self.goto_match(k);
        } else if has && prev {
            let k = self.current_match.map_or(0, |k| (k + n - 1) % n);
            self.goto_match(k);
        }
        let close_r = Rect::new(nav_x + 76.0, y, 34.0, 34.0);
        let close_hit = ui.button_ex(id("rd-search-close"), close_r, "", Some(icons::CLOSE), ButtonKind::Ghost, true);
        ui.tip(id("rd-search-close"), close_r, "Close search (Esc)");
        if close_hit {
            self.close_panel();
        }
    }

    // ---- selecting, highlights, notes and definitions -----------------------------------

    /// Where a block's text starts counting for positions (after a first-line indent).
    fn shift(b: &Block) -> usize {
        if b.kind == Kind::Para && b.rich.text.starts_with(INDENT) { INDENT.len() } else { 0 }
    }

    /// The selection, start first, if it's in the current layout.
    fn sel_range(&self) -> Option<(Mark, Mark)> {
        let s = self.sel.as_ref()?;
        if self.layout.as_ref().is_none_or(|l| l.key != s.key) || s.anchor == s.head {
            return None;
        }
        Some(if s.anchor <= s.head { (s.anchor, s.head) } else { (s.head, s.anchor) })
    }

    /// The selected part of block `bi` (whose text is `len` bytes).
    fn sel_in(&self, bi: usize, len: usize) -> Option<(usize, usize)> {
        let (a, b) = self.sel_range()?;
        if bi < a.block || bi > b.block {
            return None;
        }
        let s = if bi == a.block { a.at } else { 0 };
        let e = if bi == b.block { b.at } else { len };
        (s < e).then_some((s, e.min(len)))
    }

    fn sel_text(&self) -> String {
        let (Some(layout), Some((a, b))) = (&self.layout, self.sel_range()) else { return String::new() };
        let mut parts = Vec::new();
        for bi in a.block..=b.block {
            let blk = &layout.blocks[bi];
            if !blk.kind.searchable() {
                continue;
            }
            if let Some((s, e)) = self.sel_in(bi, blk.rich.text.len()) {
                parts.push(blk.rich.text.get(s..e).unwrap_or_default().trim().to_string());
            }
        }
        parts.join("\n")
    }

    /// The selection as a highlight (not saved yet).
    fn sel_highlight(&self, color: u8) -> Option<Highlight> {
        let layout = self.layout.as_ref()?;
        let (a, b) = self.sel_range()?;
        let at = |m: Mark| {
            let blk = &layout.blocks[m.block];
            (blk.para, m.at.saturating_sub(Self::shift(blk)))
        };
        Some(Highlight {
            id: format!("h{}", chrono::Utc::now().timestamp_millis()),
            chapter: self.chapter,
            start: at(a),
            end: at(b),
            color,
            note: String::new(),
            text: self.sel_text(),
            created_at: chrono::Utc::now(),
        })
    }

    /// The text position under `m`: on text exactly (`exact`), or the nearest line otherwise.
    fn mark_at(&self, ui: &Ui, m: (f32, f32), exact: bool) -> Option<Mark> {
        let spots = self.spots.borrow();
        let spot = spots.iter().find(|s| s.shown.contains(m)).or_else(|| {
            if exact {
                return None;
            }
            spots.iter().min_by(|a, b| {
                let d = |s: &&Spot| (m.1 - m.1.min(s.shown.bottom()).max(s.shown.y)).abs() + (m.0 - m.0.min(s.shown.right()).max(s.shown.x)).abs() * 0.25;
                d(a).total_cmp(&d(b))
            })
        })?;
        let blk = &self.layout.as_ref()?.blocks[spot.block];
        // Into the text's visible part (max then min: never panics on a sliver of a line).
        let inside = |v: f32, lo: f32, hi: f32| v.min(hi).max(lo);
        let p = (inside(m.0, spot.shown.x, spot.shown.right()) - spot.x, inside(m.1, spot.shown.y, spot.shown.bottom() - 1.0) - spot.y);
        Some(Mark { block: spot.block, at: ui.g.hit_point(spot.text, &blk.rich.text, p.0, p.1) })
    }

    /// `m` moved to the start (`end`: the end) of the word it's inside, so a selection never stops
    /// halfway through one.
    fn snap(&self, m: Mark, end: bool) -> Mark {
        let Some(text) = self.layout.as_ref().map(|l| &l.blocks[m.block].rich.text) else { return m };
        let mut at = m.at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        // Spaces at the ends of a selection drop off.
        if end {
            at = text[..at].trim_end().len();
        } else {
            at = text.len() - text[at..].trim_start().len();
        }
        let in_word = |c: char| c.is_alphanumeric() || matches!(c, '\'' | '’' | '-');
        let inside = if end { text[..at].chars().next_back() } else { text[at..].chars().next() }.is_some_and(in_word);
        if !inside {
            return Mark { at, ..m };
        }
        let at = if end {
            text[at..].char_indices().find(|&(_, c)| !in_word(c)).map_or(text.len(), |(i, _)| at + i)
        } else {
            text[..at].char_indices().rev().find(|&(_, c)| !in_word(c)).map_or(0, |(i, c)| i + c.len_utf8())
        };
        Mark { at, ..m }
    }

    /// The highlight under `mark`, if any.
    fn highlight_at(&self, mark: Mark) -> Option<&Highlight> {
        let blk = &self.layout.as_ref()?.blocks[mark.block];
        let at = mark.at.saturating_sub(Self::shift(blk));
        self.highlights.iter().find(|h| h.span_in(blk.para, blk.rich.text.len()).is_some_and(|(s, e)| at >= s && at <= e))
    }

    /// Dragging over text selects it, in whole words; letting go shows what can be done with it.
    /// A double-click selects a word, and a click on a highlight shows what can be done with that.
    fn select(&mut self, ui: &mut Ui, area: Rect) {
        let m = ui.input.mouse;
        let on_popover = self.popover_rect.is_some_and(|p| p.contains(m));
        if ui.input.pressed && ui.input.double && ui.hit(area) && !on_popover {
            if let Some(mark) = self.mark_at(ui, m, true) {
                let (a, b) = (self.snap(mark, false), self.snap(mark, true));
                if a != b {
                    self.sel = Some(Selection { key: self.layout.as_ref().map_or(0, |l| l.key), anchor: a, head: b, dragging: false });
                    self.popover = self.place(ui, a, b).map(|r| (r, None));
                    ui.request_repaint();
                    return;
                }
            }
        }
        if ui.input.pressed && ui.hit(area) && !on_popover {
            self.popover = None;
            self.sel = match self.mark_at(ui, m, true) {
                Some(mark) => Some(Selection { key: self.layout.as_ref().map_or(0, |l| l.key), anchor: mark, head: mark, dragging: true }),
                None => None,
            };
        }
        let dragging = self.sel.as_ref().is_some_and(|s| s.dragging);
        if dragging && ui.input.down {
            if let Some(mark) = self.mark_at(ui, m, false) {
                if let Some(s) = &mut self.sel {
                    if s.head != mark {
                        s.head = mark;
                        ui.request_repaint();
                    }
                }
            }
        }
        if dragging && ui.input.released {
            if let Some((a, b)) = self.sel_range() {
                let (a, b) = (self.snap(a, false), self.snap(b, true));
                if let Some(s) = &mut self.sel {
                    (s.anchor, s.head) = (a, b);
                }
            }
            if let Some(s) = &mut self.sel {
                s.dragging = false;
            }
            match self.sel_range() {
                Some((a, b)) => self.popover = self.place(ui, a, b).map(|r| (r, None)),
                None => {
                    // A click: on a highlight, its actions.
                    let anchor = self.sel.as_ref().map(|s| s.anchor);
                    self.sel = None;
                    if let Some(h) = anchor.and_then(|a| self.highlight_at(a)).map(|h| h.id.clone()) {
                        self.popover = anchor.and_then(|a| self.place(ui, a, a)).map(|r| (r, Some(h)));
                    }
                }
            }
            ui.request_repaint();
        }
    }

    /// Where the selection is on screen: from its first line's top to its last line's bottom,
    /// across the first line (so the bar can sit above it, or below it all).
    fn place(&self, ui: &Ui, a: Mark, b: Mark) -> Option<Rect> {
        let spots = self.spots.borrow();
        let layout = self.layout.as_ref()?;
        // A paragraph running from one page (or column) onto the next is drawn in two places:
        // the line wanted is in the one that shows it.
        let line = |m: Mark, from: usize, to: usize, last: bool| -> Option<Rect> {
            let text = &layout.blocks[m.block].rich.text;
            let mut found = None;
            for spot in spots.iter().filter(|s| s.block == m.block) {
                let rects = ui.g.range_rects(spot.text, text, from.min(text.len()), to.min(text.len()));
                let Some(r) = (if last { rects.last() } else { rects.first() }) else { continue };
                let rr = Rect::new(spot.x + r.x, spot.y + r.y, r.w.max(8.0), r.h);
                if spot.shown.contains((rr.x + 1.0, rr.y + rr.h / 2.0)) {
                    return Some(rr);
                }
                found.get_or_insert(rr);
            }
            found
        };
        let a_len = layout.blocks[a.block].rich.text.len();
        let first = line(a, a.at, if b.block == a.block { b.at.max(a.at + 1) } else { a_len }, false)?;
        let last = line(b, if b.block == a.block { a.at } else { 0 }, b.at.max(1), true).unwrap_or(first);
        Some(Rect::new(first.x, first.y, first.w, (last.bottom() - first.y).max(first.h)))
    }

    fn selection_keys(&mut self, ui: &mut Ui, out: &mut Vec<ReaderAction>) {
        if ui.key_combo(VK_C, true, false) && self.sel_range().is_some() {
            out.push(ReaderAction::Copy(self.sel_text()));
            out.push(ReaderAction::Notice("Copied".into()));
        }
    }

    /// The bar over a selection (highlight colours, note, copy, define) or over a highlight
    /// (colours, note, copy, remove).
    fn popover(&mut self, ui: &mut Ui, area: Rect, out: &mut Vec<ReaderAction>) {
        let Some((at, hid)) = self.popover.clone() else {
            self.popover_rect = None;
            return;
        };
        let c = ui.c;
        let existing = hid.as_ref().and_then(|id| self.highlights.iter().find(|h| &h.id == id).cloned());
        if hid.is_some() && existing.is_none() {
            self.popover = None;
            return;
        }
        let text = existing.as_ref().map_or_else(|| self.sel_text(), |h| h.text.clone());
        let word = crate::sources::dictionary::word_of(&text);
        let can_define = existing.is_none() && !word.is_empty() && word.split_whitespace().count() <= 3;
        let mut items: Vec<(&str, &str)> = vec![("\u{E70F}", "Note"), ("\u{E8C8}", "Copy")];
        if can_define {
            items.push(("\u{E82D}", "Define"));
        }
        if existing.is_some() {
            items.push((icons::DELETE, "Remove"));
        }
        let dot = 30.0;
        let w = 12.0 + 4.0 * dot + 10.0 + items.len() as f32 * 40.0 + 8.0;
        let h = 44.0;
        let x = (at.x + at.w / 2.0 - w / 2.0).clamp(area.x + 8.0, area.right() - w - 8.0);
        let y = if at.y - h - 8.0 > area.y { at.y - h - 8.0 } else { at.bottom() + 8.0 };
        let r = Rect::new(x, y, w, h);
        self.popover_rect = Some(r);
        ui.block(r);
        ui.overlay(|ui| {
            ui.g.fill_round(Rect::new(r.x + 1.0, r.y + 3.0, r.w, r.h), 10.0, Color::from_u8(0, 0, 0).alpha(0.18));
            ui.g.fill_round(r, 10.0, c.surface);
            ui.g.stroke_round(r, 10.0, 1.0, c.border);
            let mut bx = r.x + 12.0;
            for (i, (rr, gg, bb, name)) in HIGHLIGHTS.iter().enumerate() {
                let dr = Rect::new(bx, r.y + 7.0, dot, dot);
                let current = existing.as_ref().is_some_and(|h| h.color as usize == i);
                let resp = ui.interact_as(id2(id("hl-color"), i), dr, Role::Choice(current), &format!("Highlight {}", name.to_lowercase()));
                ui.g.fill_circle(dr.x + dot / 2.0, dr.y + dot / 2.0, 10.0, Color::from_u8(*rr, *gg, *bb));
                if current || resp.hovered {
                    ui.g.stroke_round(dr.shrink(1.0), dot / 2.0 - 1.0, 2.0, c.text.alpha(if current { 0.9 } else { 0.4 }));
                }
                ui.tip(id2(id("hl-color"), i), dr, &format!("Highlight in {}", name.to_lowercase()));
                if resp.clicked {
                    match &existing {
                        Some(h) => out.push(ReaderAction::SaveHighlight(Highlight { color: i as u8, ..h.clone() })),
                        None => {
                            if let Some(h) = self.sel_highlight(i as u8) {
                                out.push(ReaderAction::SaveHighlight(h));
                            }
                        }
                    }
                    self.sel = None;
                    self.popover = None;
                }
                bx += dot;
            }
            bx += 10.0;
            ui.g.fill_rect(Rect::new(bx - 6.0, r.y + 10.0, 1.0, r.h - 20.0), c.border);
            for (icon, label) in items {
                let br = Rect::new(bx, r.y + 6.0, 36.0, 32.0);
                if ui.icon_button(id2(id("hl-act"), label), br, icon, label, false) {
                    match label {
                        "Note" => {
                            let h = existing.clone().or_else(|| self.sel_highlight(0));
                            if let Some(h) = h {
                                self.note = h.note.clone();
                                self.note_for = Some(h);
                                self.panel = Panel::Note;
                                self.focus_pending = true;
                            }
                        }
                        "Copy" => {
                            out.push(ReaderAction::Copy(text.clone()));
                            out.push(ReaderAction::Notice("Copied".into()));
                        }
                        "Define" => {
                            self.define = Some((word.clone(), None));
                            self.panel = Panel::Define;
                            out.push(ReaderAction::Define(word.clone()));
                        }
                        _ => {
                            if let Some(h) = &existing {
                                out.push(ReaderAction::RemoveHighlight(h.id.clone()));
                            }
                        }
                    }
                    self.popover = None;
                    if label != "Note" && label != "Define" {
                        self.sel = None;
                    }
                }
                bx += 40.0;
            }
        });
    }

    /// Meanings arrived for the word being looked up.
    pub fn defined(&mut self, word: &str, result: Result<Vec<Meaning>, String>) {
        if let Some((w, r)) = &mut self.define {
            if w == word {
                *r = Some(result);
            }
        }
    }

    fn note_panel(&mut self, ui: &mut Ui, r: Rect, out: &mut Vec<ReaderAction>) {
        let c = ui.c;
        Self::panel_frame(ui, r);
        let Some(h) = self.note_for.clone() else {
            self.panel = Panel::None;
            return;
        };
        let (x, w) = (r.x + 16.0, r.w - 32.0);
        ui.text(if h.note.is_empty() { "Add a note" } else { "Edit the note" }, &TextStyle::ui(15.0).semibold(), x, r.y + 14.0, c.text);
        ui.g.fill_round(Rect::new(x, r.y + 44.0, 4.0, 38.0), 2.0, highlight_color(h.color, false).alpha(1.0));
        let t = ui.measure(&format!("“{}”", h.text), &TextStyle::ui(12.5).italic(), TextOpts::wrap(w - 14.0).max_lines(2));
        ui.g.draw_text(t, x + 14.0, r.y + 44.0, c.muted);
        let nid = id("rd-note");
        let resp = ui.text_input(nid, Rect::new(x, r.y + 100.0, w, 36.0), &mut self.note, "Your note", false);
        if self.focus_pending {
            ui.focus_input(nid);
            self.focus_pending = false;
        }
        if resp.escaped || ui.button(id("rd-note-cancel"), Rect::new(r.right() - 16.0 - 230.0, r.bottom() - 52.0, 100.0, 36.0), "Cancel") {
            self.panel = Panel::None;
            self.note_for = None;
            return;
        }
        let save = ui.button_ex(id("rd-note-save"), Rect::new(r.right() - 16.0 - 122.0, r.bottom() - 52.0, 122.0, 36.0), "Save", Some(icons::CHECK), ButtonKind::Primary, true);
        if save || resp.submitted {
            out.push(ReaderAction::SaveHighlight(Highlight { note: self.note.trim().to_string(), ..h }));
            out.push(ReaderAction::Notice("Note saved".into()));
            self.panel = Panel::None;
            self.note_for = None;
            self.sel = None;
        }
    }

    fn define_panel(&mut self, ui: &mut Ui, r: Rect) {
        let c = ui.c;
        Self::panel_frame(ui, r);
        let Some((word, result)) = self.define.clone() else {
            self.panel = Panel::None;
            return;
        };
        let (x, w) = (r.x + 18.0, r.w - 36.0);
        let t = ui.text(&word, &TextStyle::ui(22.0).semibold(), x, r.y + 16.0, c.text);
        let close = Rect::new(r.right() - 46.0, r.y + 12.0, 34.0, 32.0);
        if ui.icon_button(id("rd-define-close"), close, icons::CLOSE, "Close", false) {
            self.panel = Panel::None;
            return;
        }
        let top = r.y + 22.0 + t.h + 10.0;
        let list = Rect::new(r.x + 2.0, top, r.w - 4.0, r.bottom() - top - 36.0);
        match result {
            None => ui.spinner(r.x + r.w / 2.0, top + 40.0, 12.0),
            Some(Err(e)) => {
                let t = ui.measure(&e, &TextStyle::ui(13.5), TextOpts::wrap(w));
                ui.g.draw_text(t, x, top + 8.0, c.muted);
            }
            Some(Ok(meanings)) => {
                ui.scroll_area(id("rd-define-list"), list, |ui, _view, y0| {
                    let mut y = y0 + 4.0;
                    for m in &meanings {
                        let pt = ui.text(&m.part, &TextStyle::ui(13.0).semibold().italic(), x, y, c.accent_text);
                        y += pt.h + 6.0;
                        for (i, d) in m.definitions.iter().take(6).enumerate() {
                            let n = ui.text(&format!("{}.", i + 1), &TextStyle::ui(13.5), x, y, c.muted);
                            let t = ui.measure(d, &TextStyle::ui(13.5), TextOpts::wrap(w - 24.0).line_height(1.35));
                            ui.g.draw_text(t, x + 22.0, y, c.text);
                            y += t.h.max(n.h) + 8.0;
                        }
                        y += 8.0;
                    }
                    y - y0
                });
            }
        }
        ui.text_in("From Wiktionary", &TextStyle::ui(11.5), Rect::new(x, r.bottom() - 30.0, w, 20.0), Align::Left, c.muted);
    }

    /// The Highlights tab of Contents: every highlight and note, to jump to. Returns where to go.
    fn highlights_list(&mut self, ui: &mut Ui, list: Rect, highlights: &[Highlight], out: &mut Vec<ReaderAction>) -> Option<(usize, f32)> {
        let c = ui.c;
        let mut sorted: Vec<&Highlight> = highlights.iter().collect();
        sorted.sort_by_key(|h| (h.chapter, h.start));
        if sorted.is_empty() {
            let msg = "No highlights yet. Select text with the mouse to highlight it, add a note, copy it or look a word up.";
            let t = ui.measure(msg, &TextStyle::ui(13.0), TextOpts::wrap(list.w - 48.0).align(Align::Center));
            ui.g.draw_text(t, list.x + 24.0, list.y + 32.0, c.muted);
            return None;
        }
        let row_h = 96.0;
        let mut jump = None;
        let mut remove = None;
        let titles: Vec<String> = self.work.chapters.iter().map(|ch| ch.title.clone()).collect();
        ui.scroll_area(id("highlight-list"), list, |ui, view, y0| {
            for (i, h) in sorted.iter().enumerate() {
                let rr = Rect::new(view.x + 6.0, y0 + i as f32 * row_h, view.w - 22.0, row_h - 6.0);
                if rr.bottom() < view.y || rr.y > view.bottom() {
                    continue;
                }
                let del = Rect::new(rr.right() - 38.0, rr.y + 8.0, 32.0, 30.0);
                let deleted = ui.button_ex(id2(id("hl-del"), &h.id), del, "", Some(icons::DELETE), ButtonKind::Ghost, true);
                ui.tip(id2(id("hl-del"), &h.id), del, "Remove highlight");
                if deleted {
                    remove = Some(h.id.clone());
                }
                let resp = ui.interact_as(id2(id("hl-row"), &h.id), Rect::new(rr.x, rr.y, rr.w - 44.0, rr.h), Role::Button, &h.text);
                ui.g.stroke_round(rr, 6.0, 1.0, c.border);
                if resp.hovered {
                    ui.g.fill_round(rr, 6.0, c.hover());
                }
                ui.g.fill_round(Rect::new(rr.x + 10.0, rr.y + 10.0, 4.0, rr.h - 20.0), 2.0, highlight_color(h.color, false).alpha(1.0));
                let ch = titles.get(h.chapter).map(|t| t.trim()).filter(|t| !t.is_empty()).map_or_else(|| format!("Chapter {}", h.chapter + 1), str::to_string);
                ui.text_opts(&ch, &TextStyle::ui(12.0).semibold(), TextOpts::ellipsize(rr.w - 80.0), rr.x + 22.0, rr.y + 9.0, c.accent_text);
                let t = ui.measure(&h.text, &TextStyle::ui(13.0), TextOpts::wrap(rr.w - 72.0).max_lines(2));
                ui.g.draw_text(t, rr.x + 22.0, rr.y + 28.0, c.text);
                if !h.note.is_empty() {
                    ui.text_opts(&h.note, &TextStyle::ui(12.5).italic(), TextOpts::ellipsize(rr.w - 72.0), rr.x + 22.0, rr.y + 30.0 + t.h + 2.0, c.muted);
                }
                if resp.clicked {
                    jump = Some((h.chapter, h.start.0 as f32));
                }
            }
            sorted.len() as f32 * row_h + 8.0
        });
        if let Some(id) = remove {
            out.push(ReaderAction::RemoveHighlight(id));
        }
        jump
    }

    fn bookmark_panel(&mut self, ui: &mut Ui, r: Rect, out: &mut Vec<ReaderAction>) {
        let c = ui.c;
        Self::panel_frame(ui, r);
        let x = r.x + 16.0;
        let w = r.w - 32.0;
        ui.text("Bookmark this page", &TextStyle::ui(15.0).semibold(), x, r.y + 14.0, c.text);
        let para = (self.anchor.floor() as usize).min(self.work.chapters[self.chapter].paragraphs.len().saturating_sub(1));
        // First real paragraph at or after the reading position.
        let excerpt: String = self.work.chapters[self.chapter]
            .paragraphs
            .iter()
            .enumerate()
            .skip(para)
            .filter(|(i, p)| !is_landmark(*i, p))
            .find_map(|(_, p)| paragraph_rich(p))
            .map(|(_, r)| r.text.chars().take(160).collect())
            .unwrap_or_default();
        let t = ui.measure(&format!("Chapter {} — “{}”", self.chapter + 1, excerpt), &TextStyle::ui(12.5).italic(), TextOpts::wrap(w).max_lines(2));
        ui.g.draw_text(t, x, r.y + 42.0, c.muted);
        let nid = id("rd-bookmark-note");
        let resp = ui.text_input(nid, Rect::new(x, r.y + 92.0, w, 36.0), &mut self.bookmark_note, "Add a note (optional)", false);
        if self.focus_pending {
            ui.focus_input(nid);
            self.focus_pending = false;
        }
        if resp.escaped || ui.button(id("rd-bm-cancel"), Rect::new(r.right() - 16.0 - 230.0, r.bottom() - 52.0, 100.0, 36.0), "Cancel") {
            self.panel = Panel::None;
            return;
        }
        let add = ui.button_ex(id("rd-bm-add"), Rect::new(r.right() - 16.0 - 122.0, r.bottom() - 52.0, 122.0, 36.0), "Add", Some("\u{E734}"), ButtonKind::Primary, true);
        if add || resp.submitted {
            let now = chrono::Utc::now();
            out.push(ReaderAction::AddBookmark(Bookmark {
                id: format!("bm_{}_{}", self.chapter + 1, now.timestamp_millis()),
                chapter_index: self.chapter + 1,
                paragraph_index: para,
                note: self.bookmark_note.trim().to_string(),
                excerpt,
                created_at: now,
            }));
            self.panel = Panel::None;
        }
    }
}
