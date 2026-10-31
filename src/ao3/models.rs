use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Represents an AO3 Work Rating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkRating {
    General,
    Teen,
    Mature,
    Explicit,
    NotRated,
}

impl WorkRating {
    pub fn from_str_loose(s: &str) -> Self {
        let lower = s.to_lowercase();
        if lower.contains("explicit") {
            WorkRating::Explicit
        } else if lower.contains("mature") {
            WorkRating::Mature
        } else if lower.contains("teen") {
            WorkRating::Teen
        } else if lower.contains("general") {
            WorkRating::General
        } else {
            WorkRating::NotRated
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            WorkRating::General => "General Audiences",
            WorkRating::Teen => "Teen And Up Audiences",
            WorkRating::Mature => "Mature",
            WorkRating::Explicit => "Explicit",
            WorkRating::NotRated => "Not Rated",
        }
    }

    pub fn badge_color(&self) -> [u8; 3] {
        match self {
            WorkRating::General => [46, 160, 67],    // AO3 Green
            WorkRating::Teen => [210, 153, 34],     // AO3 Gold/Yellow
            WorkRating::Mature => [219, 109, 40],   // AO3 Orange
            WorkRating::Explicit => [207, 34, 46],  // AO3 Red
            WorkRating::NotRated => [110, 119, 129],// Gray
        }
    }
}

/// Category / Pairing orientations in AO3
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    FF,
    FM,
    MM,
    Multi,
    Other,
    Gen,
    Unknown(String),
}

impl Category {
    pub fn from_str_loose(s: &str) -> Self {
        let trimmed = s.trim();
        match trimmed {
            "F/F" => Category::FF,
            "F/M" => Category::FM,
            "M/M" => Category::MM,
            "Multi" => Category::Multi,
            "Other" => Category::Other,
            "Gen" => Category::Gen,
            other => Category::Unknown(other.to_string()),
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Category::FF => "F/F",
            Category::FM => "F/M",
            Category::MM => "M/M",
            Category::Multi => "Multi",
            Category::Other => "Other",
            Category::Gen => "Gen",
            Category::Unknown(s) => s.as_str(),
        }
    }
}

/// Archive Warnings
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveWarning {
    pub name: String,
    pub is_major: bool,
}

/// Series information
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SeriesInfo {
    pub id: Option<String>,
    pub name: String,
    pub part: u32,
    pub previous_work_id: Option<String>,
    pub next_work_id: Option<String>,
}

/// Statistics for a work
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkStats {
    pub published_date: String,
    pub completed_date: Option<String>,
    pub words: usize,
    pub chapters_published: u32,
    pub chapters_total: Option<u32>, // None if '?'
    pub comments: u64,
    pub kudos: u64,
    pub bookmarks: u64,
    pub hits: u64,
}

/// Full metadata of an AO3 work
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkMeta {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub summary_html: String,
    pub summary_text: String,
    pub rating: WorkRating,
    pub warnings: Vec<String>,
    pub categories: Vec<Category>,
    pub fandoms: Vec<String>,
    pub relationships: Vec<String>,
    pub characters: Vec<String>,
    pub freeform_tags: Vec<String>,
    pub language: String,
    pub series: Vec<SeriesInfo>,
    pub stats: WorkStats,
    pub is_restricted: bool, // Requires AO3 login
    pub is_complete: bool,
    /// A PDF (drawn page by page from the file, see `crate::pdf`), with its page count: 0 for one
    /// locked with a password and not opened yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf_pages: Option<u32>,
}

impl WorkMeta {
    pub fn is_pdf(&self) -> bool {
        self.pdf_pages.is_some()
    }

    /// A work from AO3 (its id is AO3's work number), as opposed to a book imported from a file.
    /// Ratings, kudos and "complete" only mean something for these.
    pub fn is_ao3(&self) -> bool {
        !self.id.is_empty() && self.id.chars().all(|c| c.is_ascii_digit())
    }
}

impl Default for WorkMeta {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: "Untitled".to_string(),
            authors: vec!["Anonymous".to_string()],
            summary_html: String::new(),
            summary_text: String::new(),
            rating: WorkRating::NotRated,
            warnings: Vec::new(),
            categories: Vec::new(),
            fandoms: Vec::new(),
            relationships: Vec::new(),
            characters: Vec::new(),
            freeform_tags: Vec::new(),
            language: "English".to_string(),
            series: Vec::new(),
            stats: WorkStats::default(),
            is_restricted: false,
            is_complete: false,
            pdf_pages: None,
        }
    }
}

/// Single chapter in an AO3 work
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub id: Option<String>,
    pub index: usize, // 1-based index
    pub title: String,
    pub summary: Option<String>,
    pub pre_notes: Option<String>,
    pub post_notes: Option<String>,
    pub paragraphs: Vec<String>, // Parsed text paragraphs
    /// Source HTML of the chapter body. No longer filled in; always written (empty) because
    /// v1.x requires the field when it reads a saved work.
    #[serde(default)]
    pub raw_html: String,
    pub word_count: usize,
}

impl Chapter {
    pub fn new(index: usize, title: String) -> Self {
        Self {
            id: None,
            index,
            title,
            summary: None,
            pre_notes: None,
            post_notes: None,
            paragraphs: Vec::new(),
            raw_html: String::new(),
            word_count: 0,
        }
    }
}

/// Complete Work including all chapters
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    pub meta: WorkMeta,
    pub chapters: Vec<Chapter>,
    pub fetched_at: DateTime<Utc>,
    /// Pictures found while importing, as (file name, bytes). Saved next to the work by the
    /// library, never inside its JSON.
    #[serde(skip)]
    pub resources: Vec<(String, Vec<u8>)>,
}

impl Work {
    pub fn total_words(&self) -> usize {
        if self.meta.stats.words > 0 {
            self.meta.stats.words
        } else {
            self.chapters.iter().map(|c| c.word_count).sum()
        }
    }

    pub fn chapter_count(&self) -> usize {
        self.chapters.len()
    }
}

/// Reading status for library organization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ShelfStatus {
    #[default]
    Reading,
    Completed,
    MarkedForLater,
    Favorites,
}

impl ShelfStatus {
    pub fn display_name(&self) -> &'static str {
        match self {
            ShelfStatus::Reading => "Currently Reading",
            ShelfStatus::Completed => "Completed",
            ShelfStatus::MarkedForLater => "Marked for Later",
            ShelfStatus::Favorites => "Favorites",
        }
    }
}

/// A highlighted passage, with a note if the reader wrote one. Its ends are (paragraph index,
/// byte offset into that paragraph's text as shown), so it stays put through any change of font,
/// size, spacing or layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Highlight {
    pub id: String,
    /// 0-based chapter.
    pub chapter: usize,
    pub start: (usize, usize),
    pub end: (usize, usize),
    /// Which of the highlight colours: 0 yellow, 1 green, 2 blue, 3 pink.
    #[serde(default)]
    pub color: u8,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// The passage itself, for the list of highlights.
    pub text: String,
    pub created_at: DateTime<Utc>,
}

impl Highlight {
    /// The part of paragraph `para` (whose text is `len` bytes) this covers, if any.
    pub fn span_in(&self, para: usize, len: usize) -> Option<(usize, usize)> {
        if para < self.start.0 || para > self.end.0 {
            return None;
        }
        let s = if para == self.start.0 { self.start.1.min(len) } else { 0 };
        let e = if para == self.end.0 { self.end.1.min(len) } else { len };
        (s < e).then_some((s, e))
    }
}

/// A reader bookmark placed in a work
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub chapter_index: usize, // 1-based
    pub paragraph_index: usize,
    pub note: String,
    pub excerpt: String,
    pub created_at: DateTime<Utc>,
}

/// Saved reading progress for a work
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingProgress {
    pub current_chapter: usize, // 1-based index
    pub scroll_offset: f32,
    /// Position inside the current chapter as paragraph index + fraction through that
    /// paragraph (e.g. 12.4). Independent of font size, window size and reading mode.
    #[serde(default)]
    pub position: f32,
    pub percentage: f32, // 0.0 - 100.0
    pub last_read: DateTime<Utc>,
    pub completed: bool,
}

impl Default for ReadingProgress {
    fn default() -> Self {
        Self {
            current_chapter: 1,
            scroll_offset: 0.0,
            position: 0.0,
            percentage: 0.0,
            last_read: Utc::now(),
            completed: false,
        }
    }
}

/// Stored entry in local user library
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub meta: WorkMeta,
    pub shelf: ShelfStatus,
    pub is_favorite: bool,
    pub progress: ReadingProgress,
    pub bookmarks: Vec<Bookmark>,
    #[serde(default)]
    pub highlights: Vec<Highlight>,
    pub downloaded_at: DateTime<Utc>,
    pub local_file_exists: bool,
    pub personal_notes: String,
}

/// AO3 User session info
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UserSession {
    pub username: Option<String>,
    pub session_cookie: Option<String>,
    pub remember_token: Option<String>,
    pub is_logged_in: bool,
}
