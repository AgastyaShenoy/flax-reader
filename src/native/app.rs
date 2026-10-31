//! Application state and the window shell (header, navigation, toasts).

use super::gfx::*;
use super::account::{AccountAction, AccountScreen, AO3};
use super::browse::{BrowseAction, FollowingScreen, SearchScreen, Target};
use super::icons;
use super::mark;
use super::library::{ExportFormat, LibraryAction, LibraryScreen};
use super::book::Book;
use super::catalog::{CatalogAction, CatalogScreen, Getting};
use super::pdf::{LockedPdf, PdfScreen};
use super::reader::{ReaderAction, ReaderScreen};
use super::settings::{SettingsAction, SettingsCtx, SettingsScreen};
use super::ui::*;
use super::worker::Worker;
use crate::ao3::client::Ao3Client;
use crate::ao3::models::{ReadingProgress, ShelfStatus, UserSession, Work, WorkMeta};
use crate::ao3::search::{Feed, FeedKind, Listing};
use crate::storage::follows::{FollowStore, Scope, STALE_MINUTES};
use crate::storage::export::WorkExporter;
use crate::reader::typography::{FontFamilyChoice, TypographySettings};
use crate::storage::config::AppConfig;
use crate::storage::library::LibraryManager;
use crate::updater::{check_for_updates, UpdateInfo, UpdateStatus, CURRENT_VERSION};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

const HEADER_H: f32 = 56.0;
const TOAST_SECS: u64 = 4;
/// Settings are written to disk this long after the last change, not on every slider step.
const CONFIG_SAVE_DELAY: Duration = Duration::from_millis(600);
/// Reading position is written at most this often while reading (and always on close).
const PROGRESS_SAVE_DELAY: Duration = Duration::from_secs(3);

pub enum AppEvent {
    UpdateChecked(Result<Option<UpdateInfo>, String>),
    Kudos(Result<String, String>),
    Fetched { id: String, result: Result<Work, String>, open: bool },
    /// A work's details for the pasted-link card.
    LookedUp { id: String, result: Result<WorkMeta, String> },
    Imported(Result<Work, String>),
    Epub { name: String, result: Result<Vec<u8>, String> },
    Login(Result<UserSession, String>),
    SessionUser(Result<Option<String>, String>),
    Searched { page: u32, result: Result<Listing, String> },
    /// First pages of followed feeds, by feed id, with any followed story brought up to date.
    FollowsChecked(Vec<(String, Result<(Listing, Option<Work>), String>)>),
    /// A story followed from a link: its feed, first listing and (for forum stories) the story.
    StoryFollowed(Result<(Feed, Listing, Option<Work>), String>),
    FeedPage(Result<Listing, String>),
    /// A page of a catalog (`url` is the page's address, or "search:…").
    CatalogPage { url: String, append: bool, result: Result<crate::sources::opds::Feed, String> },
    /// A book downloaded from a catalog, ready to save.
    CatalogBook { key: String, result: Result<Work, String> },
    /// A word's meanings, for the reader.
    Defined { word: String, result: Result<Vec<crate::sources::dictionary::Meaning>, String> },
    /// One of the reader's AO3 lists, to bring into the library.
    Ao3List { list: crate::ao3::client::UserList, result: Result<Vec<WorkMeta>, String> },
    ForumLogin { host: String, result: Result<crate::sources::accounts::ForumAccount, String> },
    /// The threads the reader watches on a forum, and the account with its cookies as last set.
    Watched { host: String, result: Result<(Vec<crate::sources::accounts::Thread>, crate::sources::accounts::ForumAccount), String> },
    /// A page of AO3 comments for the reader (`key` is the chapter's AO3 id, or "").
    Comments { key: String, result: Result<crate::ao3::comments::CommentPage, String> },
    CommentPosted(Result<(), String>),
    /// A job with nothing to report.
    Done,
    /// A job that went wrong unexpectedly (a panic), with what it said.
    Failed(String),
}

/// Work the window host runs after the frame, outside any borrow of the app
/// (modal dialogs pump their own messages).
pub enum Deferred {
    PickFontFile,
    ImportFile,
    /// Windows' colour picker for "Your colours": the text colour (true) or the page colour.
    PickColor { text: bool, current: u32 },
    SaveFile { name: String, filter: &'static str, ext: &'static str, data: Vec<u8> },
    Open(String),
    /// Put text on the clipboard.
    Copy(String),
    Fullscreen(bool),
    SetTitle(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Reader,
    Library,
    /// Finding books: AO3 and catalogs.
    Browse,
    Following,
    Account,
    Settings,
}

pub struct App {
    pub config: AppConfig,
    library: LibraryManager,
    client: Ao3Client,
    tab: Tab,
    settings: SettingsScreen,
    toast: Option<(String, Instant)>,
    update_status: UpdateStatus,
    worker: Option<Worker<AppEvent>>,
    /// Checking followed feeds takes a while; it runs here so downloads aren't held up.
    slow_worker: Option<Worker<AppEvent>>,
    /// The main window, for render threads to wake (as a raw handle value).
    hwnd: usize,
    follows: FollowStore,
    search: SearchScreen,
    catalogs: CatalogScreen,
    catalog_client: crate::sources::CatalogClient,
    following: FollowingScreen,
    config_dirty_since: Option<Instant>,
    pub deferred: Vec<Deferred>,
    font_families: Option<Vec<String>>,
    reader: Option<Book>,
    last_progress_save: Instant,
    library_screen: LibraryScreen,
    account: AccountScreen,
    /// Cookie being verified with AO3 before it is saved.
    pending_cookie: Option<String>,
    /// The file being imported was opened with the app ("Open with"), so read it once it's in.
    open_imported: bool,
    /// The system's dark mode and high-contrast setting, kept up to date by the window.
    pub system_dark: bool,
    pub system_hc: bool,
}

/// A file name that Windows accepts, from a work title.
fn file_name(title: &str, ext: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .take(120)
        .collect();
    let clean = clean.trim().trim_end_matches('.');
    format!("{}.{ext}", if clean.is_empty() { "work" } else { clean })
}

fn signed_in(s: &UserSession) -> bool {
    s.is_logged_in && (s.session_cookie.is_some() || s.remember_token.is_some())
}

/// The font family the reader uses for the given typography settings.
pub fn reader_family(t: &TypographySettings) -> Family {
    match &t.font_family {
        FontFamilyChoice::SansSerif => Family::Named { name: "Segoe UI".into(), file: None },
        FontFamilyChoice::Serif => Family::Named { name: "Georgia".into(), file: None },
        FontFamilyChoice::Monospace => Family::Mono,
        FontFamilyChoice::Custom(name) => {
            let file = if t.custom_font_name.as_deref() == Some(name.as_str()) { t.custom_font_path.clone() } else { None };
            Family::Named { name: name.clone(), file }
        }
        FontFamilyChoice::Atkinson => Family::Named { name: "Atkinson Hyperlegible".into(), file: super::fonts::atkinson() },
        FontFamilyChoice::OpenDyslexic => Family::Named { name: "OpenDyslexic".into(), file: super::fonts::open_dyslexic() },
    }
}

pub fn reader_font_label(t: &TypographySettings) -> String {
    match &t.font_family {
        FontFamilyChoice::SansSerif => "Segoe UI (default)".into(),
        FontFamilyChoice::Serif => "Georgia".into(),
        FontFamilyChoice::Monospace => "Monospace".into(),
        FontFamilyChoice::Custom(name) => name.clone(),
        FontFamilyChoice::Atkinson => "Atkinson Hyperlegible".into(),
        FontFamilyChoice::OpenDyslexic => "OpenDyslexic".into(),
    }
}

impl App {
    pub fn new(config: AppConfig) -> Self {
        let client = Ao3Client::new(Some(&config.session));
        Self {
            config,
            library: LibraryManager::new(),
            client,
            tab: Tab::Library,
            settings: SettingsScreen::default(),
            toast: None,
            update_status: UpdateStatus::Idle,
            worker: None,
            slow_worker: None,
            hwnd: 0,
            follows: FollowStore::load(),
            search: SearchScreen::default(),
            catalogs: CatalogScreen::default(),
            catalog_client: crate::sources::CatalogClient::new(),
            following: FollowingScreen::default(),
            config_dirty_since: None,
            deferred: Vec::new(),
            font_families: None,
            reader: None,
            last_progress_save: Instant::now(),
            library_screen: LibraryScreen::default(),
            account: AccountScreen::default(),
            pending_cookie: None,
            open_imported: false,
            system_dark: true,
            system_hc: false,
        }
    }

    /// Called once the window exists, so the worker can wake it.
    pub fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd.0 as usize;
        self.worker = Some(Worker::new(hwnd, AppEvent::Failed));
        self.slow_worker = Some(Worker::new(hwnd, AppEvent::Failed));
        if self.config.check_updates_on_startup {
            self.check_updates();
        }
    }

    /// Results waiting from both background workers.
    pub fn take_events(&self) -> Vec<AppEvent> {
        self.worker.iter().chain(self.slow_worker.iter()).flat_map(|w| w.results.try_iter()).collect()
    }

    fn check_updates(&mut self) {
        if let Some(w) = &self.worker {
            self.update_status = UpdateStatus::Checking;
            w.run(|| AppEvent::UpdateChecked(check_for_updates()));
        }
    }

    pub fn on_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::UpdateChecked(res) => {
                self.update_status = match res {
                    Ok(Some(info)) => {
                        self.set_toast(format!("Version {} is available", info.latest_version));
                        UpdateStatus::UpdateAvailable(Box::new(info))
                    }
                    Ok(None) => UpdateStatus::UpToDate { checked_version: CURRENT_VERSION.to_string() },
                    Err(e) => UpdateStatus::Error(e),
                };
            }
            AppEvent::Kudos(Ok(msg)) => self.set_toast(msg),
            AppEvent::Kudos(Err(e)) => self.set_toast(format!("Couldn't leave kudos: {e}")),
            AppEvent::Fetched { id, result: Ok(work), open } => {
                if let Err(e) = self.library.save_work(&work) {
                    let e = format!("Downloaded, but couldn't save it: {e}");
                    if !self.search.link_done(&id, Err(e.clone()), false) {
                        self.set_toast(e);
                    }
                    return;
                }
                self.search.link_done(&id, Ok(work.meta.clone()), true);
                self.set_toast(format!("Saved “{}”", work.meta.title));
                if open {
                    self.open_loaded(work);
                }
            }
            AppEvent::Fetched { id, result: Err(e), .. } => {
                if !self.search.link_done(&id, Err(e.clone()), false) || !self.on_ao3() {
                    self.set_toast(format!("Couldn't download: {e}"));
                }
            }
            AppEvent::LookedUp { id, result } => {
                self.search.link_done(&id, result, false);
            }
            AppEvent::Imported(result) => {
                let imp = &mut self.settings.import;
                imp.busy = false;
                let open = std::mem::take(&mut self.open_imported);
                match result.and_then(|work| self.library.save_work(&work).map(|_| work)) {
                    Ok(mut work) => {
                        imp.error = None;
                        imp.done = Some((work.meta.id.clone(), work.meta.title.clone()));
                        // The pictures are on disk now; don't keep their bytes in memory.
                        work.resources = Vec::new();
                        if open {
                            self.open_loaded(work);
                        } else {
                            self.set_toast(format!("Imported “{}”", work.meta.title));
                        }
                    }
                    Err(e) => {
                        imp.error = Some(e);
                        imp.done = None;
                    }
                }
            }
            AppEvent::CatalogPage { url, append, result } => self.catalogs.loaded(&url, append, result),
            AppEvent::Defined { word, result } => {
                if let Some(r) = &mut self.reader {
                    r.defined(&word, result);
                }
            }
            AppEvent::CatalogBook { key, result } => match result.and_then(|work| self.library.save_work(&work).map(|_| work)) {
                Ok(work) => {
                    self.set_toast(format!("Saved “{}” to your library", work.meta.title));
                    self.catalogs.getting.insert(key, Getting::Done(work.meta.id));
                }
                Err(e) => {
                    self.set_toast(e.clone());
                    self.catalogs.getting.insert(key, Getting::Failed(e));
                }
            },
            AppEvent::Searched { page, result } => {
                self.search.busy = false;
                match result {
                    Ok(l) => {
                        if page <= 1 {
                            self.search.results.clear();
                        }
                        self.search.results.extend(l.works);
                        self.search.total = l.total;
                        self.search.page = page;
                        self.search.has_next = l.has_next;
                        self.search.error = None;
                    }
                    Err(e) => self.search.error = Some(e),
                }
            }
            AppEvent::StoryFollowed(result) => {
                let done = result.and_then(|(feed, listing, work)| {
                    if let Some(w) = &work {
                        self.library.save_work(w)?;
                    }
                    let (name, kind) = (feed.name.clone(), feed.kind);
                    let fid = self.follows.add(feed)?;
                    let _ = self.follows.record(&fid, listing);
                    self.following.show_kind(kind);
                    self.set_toast(if work.is_some() { format!("Following “{name}”: it's in your library") } else { format!("Following “{name}”") });
                    Ok(())
                });
                self.following.story_followed(done);
            }
            AppEvent::FollowsChecked(results) => {
                self.following.checking = false;
                let mut fresh = 0;
                let mut errors = Vec::new();
                for (fid, res) in results {
                    match res {
                        Ok((l, work)) => {
                            // A followed story's new chapters, into the library's copy.
                            if let Some(w) = work {
                                let _ = self.library.save_work(&w);
                            }
                            fresh += self.follows.record(&fid, l).unwrap_or(0)
                        }
                        Err(e) => {
                            let name = self.follows.get(&fid).map(|f| f.feed.name.clone()).unwrap_or_default();
                            errors.push(format!("{name}: {e}"));
                            let _ = self.follows.record_error(&fid, e);
                        }
                    }
                }
                self.following.error = errors.into_iter().next();
                if fresh > 0 {
                    self.set_toast(if fresh == 1 { "1 new work from your follows".to_string() } else { format!("{fresh} new works from your follows") });
                }
            }
            AppEvent::FeedPage(result) => {
                self.following.more_busy = false;
                match result {
                    Ok(l) => {
                        self.following.more.extend(l.works);
                        self.following.more_page = l.page;
                        self.following.more_next = l.has_next;
                    }
                    Err(e) => self.set_toast(format!("Couldn't load more: {e}")),
                }
            }
            AppEvent::Epub { name, result: Ok(data) } => {
                self.search.epub_done(None);
                self.deferred.push(Deferred::SaveFile { name, filter: "EPUB e-book", ext: "epub", data });
            }
            AppEvent::Epub { result: Err(e), .. } => {
                self.search.epub_done(Some(e.clone()));
                if !self.on_ao3() {
                    self.set_toast(format!("Couldn't download the EPUB: {e}"));
                }
            }
            AppEvent::Login(Ok(session)) => {
                let name = session.username.clone().unwrap_or_default();
                self.config.session = session;
                let _ = self.config.save();
                self.account.set_message(AO3, format!("Signed in as {name}."), false);
                self.set_toast("Signed in to AO3");
            }
            AppEvent::Login(Err(e)) => self.account.set_message(AO3, e, true),
            AppEvent::Ao3List { list, result: Ok(metas) } => {
                use crate::ao3::client::UserList;
                let (what, collection, favorite) = match list {
                    UserList::Bookmarks => ("bookmarks", "AO3 bookmarks", true),
                    UserList::MarkedForLater => ("works Marked for Later", "AO3 Marked for Later", false),
                };
                let (added, total) = self.library.import_known(&metas, collection, ShelfStatus::MarkedForLater, favorite);
                let msg = if total == 0 { format!("You have no {what} on AO3 yet.") } else { format!("Brought in {total} {what} ({added} new to your library), in the collection “{collection}”.") };
                self.account.set_message(AO3, msg.clone(), false);
                self.set_toast(msg);
            }
            AppEvent::Ao3List { result: Err(e), .. } => self.account.set_message(AO3, e, true),
            AppEvent::ForumLogin { host, result: Ok(account) } => {
                let name = account.username.clone();
                self.config.forum_accounts.retain(|a| a.host != host);
                self.config.forum_accounts.push(account);
                let _ = self.config.save();
                self.account.set_message(&host, format!("Signed in as {name}."), false);
            }
            AppEvent::ForumLogin { host, result: Err(e) } => self.account.set_message(&host, e, true),
            AppEvent::Watched { host, result: Ok((threads, account)) } => {
                if let Some(a) = self.config.forum_accounts.iter_mut().find(|a| a.host == host) {
                    *a = account;
                    let _ = self.config.save();
                }
                let site = crate::sources::forum::SITES.iter().find(|s| s.host == host).map_or("the forum", |s| s.name);
                let collection = format!("Watched on {site}");
                let metas: Vec<WorkMeta> = threads.into_iter().map(|t| t.meta).collect();
                let (added, total) = self.library.import_known(&metas, &collection, ShelfStatus::MarkedForLater, false);
                let msg = if total == 0 { format!("You don't watch any threads on {site} yet.") } else { format!("Brought in {total} watched threads ({added} new to your library), in the collection “{collection}”.") };
                self.account.set_message(&host, msg.clone(), false);
                self.set_toast(msg);
            }
            AppEvent::Watched { host, result: Err(e) } => self.account.set_message(&host, e, true),
            AppEvent::Done => {}
            AppEvent::Failed(why) => {
                // Whatever was waiting on it would spin forever: stop the spinners.
                self.account = AccountScreen::default();
                self.set_toast(if why.is_empty() { "Something went wrong. Please try again.".to_string() } else { format!("Something went wrong: {why}") });
            }
            AppEvent::Comments { key, result } => {
                if let Some(r) = &mut self.reader {
                    r.comments_loaded(&key, result);
                }
            }
            AppEvent::CommentPosted(result) => {
                if let Some(r) = &mut self.reader {
                    r.comment_posted(&result);
                }
                match result {
                    Ok(()) => self.set_toast("Comment posted"),
                    Err(e) => self.set_toast(e),
                }
            }
            AppEvent::SessionUser(result) => {
                let cookie = self.pending_cookie.take();
                match (result, cookie) {
                    (Ok(Some(name)), Some(cookie)) => {
                        self.config.session = UserSession { username: Some(name.clone()), session_cookie: None, remember_token: Some(cookie), is_logged_in: true };
                        let _ = self.config.save();
                        self.account.set_message(AO3, format!("Signed in as {name}."), false);
                        self.set_toast("Signed in to AO3");
                    }
                    (Ok(_), _) => {
                        self.client = Ao3Client::new(Some(&self.config.session));
                        self.account.set_message(AO3, "AO3 didn't recognise that cookie as signed in. Copy it again from a browser where you're signed in.", true);
                    }
                    (Err(e), _) => {
                        self.client = Ao3Client::new(Some(&self.config.session));
                        self.account.set_message(AO3, e, true);
                    }
                }
            }
        }
    }

    fn run_job(&self, job: impl FnOnce() -> AppEvent + Send + 'static) {
        if let Some(w) = &self.worker {
            w.run(job);
        }
    }

    /// A long job (a whole story from a forum, page by page), kept off the worker that loads
    /// pages.
    fn run_slow(&self, job: impl FnOnce() -> AppEvent + Send + 'static) {
        if let Some(w) = &self.slow_worker {
            w.run(job);
        }
    }

    fn start_fetch(&mut self, work_id: String, open: bool) {
        let client = self.client.clone();
        self.run_job(move || AppEvent::Fetched { result: client.fetch_work(&work_id), id: work_id, open });
    }

    /// A book opened with the app (its path on the command line, as "Open with" passes it):
    /// imported, then opened in the reader.
    pub fn open_file(&mut self, path: std::path::PathBuf) {
        self.open_imported = true;
        self.import_file(path);
    }

    /// Reads and parses an EPUB, AO3 .html download or saved .json work off the UI thread.
    pub fn import_file(&mut self, path: std::path::PathBuf) {
        self.settings.import.busy = true;
        self.settings.import.error = None;
        self.tab = Tab::Settings;
        self.settings.show_import();
        self.run_job(move || {
            // The file's name stands in for a missing title (PDFs often have none).
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
            let result = std::fs::read(&path)
                .map_err(|e| format!("Couldn't read the file: {e}"))
                .and_then(|bytes| crate::ao3::epub::import_named(&bytes, name.as_deref()));
            AppEvent::Imported(result)
        });
    }

    pub fn on_saved(&mut self, result: Result<std::path::PathBuf, String>) {
        match result {
            Ok(path) => self.set_toast(format!("Saved {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
            Err(e) => self.set_toast(format!("Couldn't save the file: {e}")),
        }
    }

    fn open_loaded(&mut self, work: Work) {
        self.close_reader();
        let id = work.meta.id.clone();
        self.library.mark_opened(&id);
        // A finished book opens at its start again ("Read again"), not on its last page.
        let progress = self.library.get_entry(&id).map(|e| e.progress.clone()).filter(|p| !p.completed);
        let title = work.meta.title.clone();
        if work.meta.is_pdf() {
            match crate::pdf::open(&id) {
                Ok(engine) => self.show_pdf(work.meta, engine, progress.as_ref()),
                // Locked: the reader asks for its password (`unlock_pdf`).
                Err(crate::pdf::OpenError::NeedsPassword) => self.reader = Some(Book::Locked(LockedPdf::new(work.meta, progress))),
                Err(e) => {
                    self.set_toast(format!("Couldn't open the PDF: {e}"));
                    return;
                }
            }
        } else {
            self.reader = Some(Book::Text(ReaderScreen::new(work, progress.as_ref())));
        }
        self.library.touch_history(&id);
        let _ = self.follows.mark_work_read(&id);
        self.tab = Tab::Reader;
        self.deferred.push(Deferred::SetTitle(format!("{title} — Flax Reader")));
    }

    /// Shows an open PDF in the reader.
    fn show_pdf(&mut self, meta: WorkMeta, engine: std::sync::Arc<dyn crate::pdf::Engine>, progress: Option<&ReadingProgress>) {
        // Render threads wake the window when pages are ready.
        let hwnd = self.hwnd;
        let wake = move || unsafe {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(HWND(hwnd as *mut _)), super::worker::WM_APP_RESULT, WPARAM(0), LPARAM(0));
        };
        self.reader = Some(Book::Pdf(PdfScreen::new(meta, engine, progress, wake)));
    }

    /// The password typed for the locked PDF in the reader: opens it, or says it's wrong.
    fn unlock_pdf(&mut self, password: &str) {
        let Some(Book::Locked(lock)) = &mut self.reader else { return };
        match crate::pdf::unlock(&lock.meta.id, password) {
            Ok(engine) => {
                let (mut meta, progress) = (lock.meta.clone(), lock.progress.clone());
                // Imported while locked: now its pages, title and author can be read.
                if crate::pdf::is_locked(&meta) {
                    crate::pdf::describe(&mut meta, engine.as_ref());
                    if let Some(mut work) = self.library.load_work(&meta.id) {
                        work.meta = meta.clone();
                        let _ = self.library.save_work(&work);
                    }
                    self.deferred.push(Deferred::SetTitle(format!("{} — Flax Reader", meta.title)));
                }
                self.show_pdf(meta, engine, progress.as_ref());
            }
            Err(crate::pdf::OpenError::NeedsPassword) => lock.wrong = true,
            Err(e) => self.set_toast(format!("Couldn't open the PDF: {e}")),
        }
    }

    pub fn open_work(&mut self, work_id: &str) {
        match self.library.load_work(work_id) {
            Some(work) => self.open_loaded(work),
            None if work_id.chars().all(|c| c.is_ascii_digit()) => {
                // The text was cleared from disk; the library entry still knows the work.
                self.set_toast("Downloading this work again…");
                self.start_fetch(work_id.to_string(), true);
            }
            None => match crate::sources::forum::url_of_id(work_id) {
                Some(url) => {
                    // A watched thread brought in from a forum: download it now.
                    self.set_toast("Downloading this story…");
                    let client = self.catalog_client.clone();
                    let id = work_id.to_string();
                    let site = crate::sources::Catalog { name: "the forum".into(), url: url.clone(), ..Default::default() };
                    self.run_slow(move || AppEvent::Fetched { result: client.get_work(&url, crate::sources::opds::STORY_MIME, &site, None), id, open: true });
                }
                None => self.set_toast("This work's text isn't on disk any more."),
            },
        }
    }

    fn save_progress(&mut self) {
        if let Some(r) = &mut self.reader {
            if r.unsaved() {
                r.mark_saved();
                let id = r.work_id().to_string();
                self.library.update_progress(&id, r.progress());
            }
        }
        self.last_progress_save = Instant::now();
    }

    fn close_reader(&mut self) {
        self.save_progress();
        if let Some(r) = self.reader.take() {
            if r.fullscreen() {
                self.deferred.push(Deferred::Fullscreen(false));
            }
            self.deferred.push(Deferred::SetTitle("Flax Reader".into()));
        }
        if self.tab == Tab::Reader {
            self.tab = Tab::Library;
        }
    }

    fn reader_frame(&mut self, ui: &mut Ui, r: Rect) {
        let Some(reader) = &mut self.reader else {
            self.tab = Tab::Library;
            return;
        };
        let entry = self.library.get_entry(reader.work_id());
        let bookmarks = entry.map(|e| e.bookmarks.clone()).unwrap_or_default();
        let highlights = entry.map(|e| e.highlights.clone()).unwrap_or_default();
        reader.set_can_comment(signed_in(&self.config.session));
        let actions = reader.frame(ui, r, &mut self.config, &bookmarks, &highlights);
        let work_id = reader.work_id().to_string();
        for a in actions {
            match a {
                ReaderAction::Close => {
                    self.close_reader();
                    return;
                }
                ReaderAction::Kudos => {
                    if let Some(w) = &self.worker {
                        let client = self.client.clone();
                        let id = work_id.clone();
                        w.run(move || AppEvent::Kudos(client.leave_kudos(&id)));
                        self.set_toast("Sending kudos…");
                    }
                }
                ReaderAction::AddBookmark(b) => {
                    self.library.add_bookmark(&work_id, b);
                    self.set_toast("Bookmark added");
                }
                ReaderAction::RemoveBookmark(bid) => self.library.remove_bookmark(&work_id, &bid),
                ReaderAction::ConfigChanged => self.mark_config_dirty(),
                ReaderAction::ToggleFullscreen => {
                    if let Some(r) = &mut self.reader {
                        let on = r.toggle_fullscreen();
                        self.deferred.push(Deferred::Fullscreen(on));
                        if on {
                            self.set_toast("Full screen: press Esc or F11 to leave");
                        }
                    }
                }
                ReaderAction::OpenTypographySettings => {
                    self.tab = Tab::Settings;
                    self.settings.show_typography();
                }
                ReaderAction::Notice(msg) => self.set_toast(msg),
                ReaderAction::Unlock(password) => self.unlock_pdf(&password),
                ReaderAction::SaveHighlight(h) => self.library.save_highlight(&work_id, h),
                ReaderAction::RemoveHighlight(hid) => self.library.remove_highlight(&work_id, &hid),
                ReaderAction::Copy(text) => self.deferred.push(Deferred::Copy(text)),
                ReaderAction::Define(word) => {
                    self.run_job(move || {
                        let result = crate::sources::dictionary::define(&word);
                        AppEvent::Defined { word, result }
                    });
                }
                ReaderAction::LoadComments { chapter_id, page } => {
                    let client = self.client.clone();
                    let wid = work_id.clone();
                    self.run_job(move || AppEvent::Comments { result: client.comments(&wid, chapter_id.as_deref(), page), key: chapter_id.unwrap_or_default() });
                }
                ReaderAction::PostComment { chapter_id, text } => {
                    let client = self.client.clone();
                    let wid = work_id.clone();
                    self.run_job(move || AppEvent::CommentPosted(client.post_comment(&wid, chapter_id.as_deref(), &text)));
                }
            }
        }
        if self.reader.as_ref().is_some_and(|r| r.unsaved()) {
            let since = self.last_progress_save.elapsed();
            if since >= PROGRESS_SAVE_DELAY {
                self.save_progress();
            } else {
                ui.wake_after((PROGRESS_SAVE_DELAY - since).as_millis() as u32 + 1);
            }
        }
    }

    pub fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn mark_config_dirty(&mut self) {
        self.config_dirty_since = Some(Instant::now());
    }

    pub fn on_close(&mut self) {
        self.save_progress();
        if self.config_dirty_since.take().is_some() {
            let _ = self.config.save();
        }
    }

    /// A font file chosen in the picker; `family` is None if DirectWrite could not load it.
    pub fn on_font_file(&mut self, path: PathBuf, family: Option<String>) {
        match family {
            Some(name) => {
                let t = &mut self.config.typography;
                t.font_family = FontFamilyChoice::Custom(name.clone());
                t.custom_font_name = Some(name.clone());
                t.custom_font_path = Some(path);
                self.mark_config_dirty();
                self.set_toast(format!("Reading font set to {name}"));
            }
            None => self.set_toast("That file could not be loaded as a font"),
        }
    }

    /// The system's look changed (or is first known): follow it if asked to.
    pub fn on_system_look(&mut self, dark: bool, high_contrast: bool) {
        self.system_dark = dark;
        self.system_hc = high_contrast;
        self.follow_system();
    }

    fn follow_system(&mut self) {
        let t = self.config.system_theme(self.system_dark, self.system_hc);
        if self.config.theme != t {
            self.config.theme = t;
            self.mark_config_dirty();
        }
    }

    /// A colour chosen for "Your colours".
    pub fn on_color(&mut self, text: bool, rgb: u32) {
        if text {
            self.config.custom_text = rgb;
        } else {
            self.config.custom_bg = rgb;
        }
        let custom = crate::reader::theme::ReaderThemeType::Custom;
        if self.config.follow_system {
            // "Your colours" becomes the theme for the system mode its page colour suits.
            if self.config.palette_of(custom).is_dark {
                self.config.dark_theme = custom;
            } else {
                self.config.light_theme = custom;
            }
            self.follow_system();
        } else {
            self.config.theme = custom;
        }
        self.mark_config_dirty();
    }

    pub fn frame(&mut self, ui: &mut Ui) {
        let (w, h) = ui.size;
        let c = ui.c;
        ui.g.clear(c.bg);

        if self.tab == Tab::Reader {
            self.reader_frame(ui, Rect::new(0.0, 0.0, w, h));
        }
        if self.tab != Tab::Following {
            self.following.left();
        }
        if self.tab != Tab::Reader {
            let body = Rect::new(0.0, HEADER_H, w, (h - HEADER_H).max(0.0));
            match self.tab {
                Tab::Settings => self.settings_frame(ui, body),
                Tab::Library => self.library_frame(ui, body),
                Tab::Browse => {
                    let (actions, ao3) = self.catalogs.frame(ui, body, &self.config.catalogs, self.search.summary(), &self.follows);
                    self.catalog_actions(actions);
                    if let Some(area) = ao3 {
                        let actions = self.search.frame(ui, area, &self.library, &self.follows, signed_in(&self.config.session));
                        self.browse_actions(actions);
                    }
                }
                Tab::Following => {
                    let actions = self.following.frame(ui, body, &self.follows, &self.library, &self.catalogs.getting);
                    self.browse_actions(actions);
                }
                Tab::Account => self.account_frame(ui, body),
                Tab::Reader => {}
            }
            // Drawn last so it sits on top of the scrolled content, but first for Tab.
            let mark = ui.node_mark();
            self.header(ui, Rect::new(0.0, 0.0, w, HEADER_H));
            ui.tab_first(mark);
        }
        self.toast_overlay(ui);

        if let Some(since) = self.config_dirty_since {
            let elapsed = since.elapsed();
            if elapsed >= CONFIG_SAVE_DELAY {
                let _ = self.config.save();
                self.config_dirty_since = None;
            } else {
                ui.wake_after((CONFIG_SAVE_DELAY - elapsed).as_millis() as u32 + 1);
            }
        }
    }

    fn header(&mut self, ui: &mut Ui, r: Rect) {
        let c = ui.c;
        ui.g.fill_rect(r, c.surface);
        ui.separator(r.x, r.bottom() - 1.0, r.w);

        // The flax flower, lightened on dark themes so it stays visible.
        ui.g.fill_path(mark::PATH, mark::SIZE, Rect::new(20.0, r.y + (r.h - 30.0) / 2.0, 30.0, 30.0), c.accent_mark);
        let name = ui.measure("Flax Reader", &TextStyle::ui(16.0).semibold(), TextOpts::default());
        ui.g.draw_text(name, 60.0, r.y + (r.h - name.h) / 2.0, c.text);
        let nav_x = (60.0 + name.w + 24.0).round();

        let account_label = if signed_in(&self.config.session) {
            self.config.session.username.clone().unwrap_or_else(|| "Accounts".into())
        } else {
            self.config.forum_accounts.first().map_or_else(|| "Accounts".into(), |a| a.username.clone())
        };
        let mut items = vec![
            (icons::LIBRARY, "Library"),
            (icons::SEARCH, "Browse"),
            (icons::BELL, "Following"),
            (icons::CONTACT, account_label.as_str()),
            (icons::SETTINGS, "Settings"),
        ];
        let mut tabs = vec![Tab::Library, Tab::Browse, Tab::Following, Tab::Account, Tab::Settings];
        // New works from follows show as a badge on Following.
        let mut badges = vec![0, 0, self.follows.unread_total(), 0, 0];
        if self.reader.is_some() {
            items.insert(0, (icons::READING, "Reading"));
            tabs.insert(0, Tab::Reader);
            badges.insert(0, 0);
        }
        let mut sel = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);

        let has_update = matches!(self.update_status, UpdateStatus::UpdateAvailable(_));
        let update_w = if has_update { if r.w > 860.0 { ui.button_width("Update available", Some(icons::UPDATE)) } else { 40.0 } } else { 0.0 };
        let tabs_w = r.w - nav_x - update_w - 16.0;
        let full: f32 = items.iter().zip(&badges).map(|((icon, label), n)| ui.button_width(label, Some(icon)) + ui.badge_width(*n) + 12.0).sum();
        if full > tabs_w {
            for (i, item) in items.iter_mut().enumerate() {
                if i != sel {
                    item.1 = "";
                }
            }
        }
        let (_, changed) = ui.tab_strip(id("nav"), nav_x, 11.0, tabs_w, &items, &badges, &mut sel);
        if changed {
            self.tab = tabs[sel];
        }

        if has_update {
            let label = if r.w > 860.0 { "Update available" } else { "" };
            let br = Rect::new(r.right() - 16.0 - update_w, 12.0, update_w, 32.0);
            if ui.button_ex(id("nav-update"), br, label, Some(icons::UPDATE), ButtonKind::Primary, true) {
                self.tab = Tab::Settings;
                self.settings.show_updates();
            }
            if label.is_empty() {
                ui.tip(id("nav-update"), br, "Update available");
            }
        }
    }

    fn library_frame(&mut self, ui: &mut Ui, body: Rect) {
        for a in self.library_screen.frame(ui, body, &self.library) {
            match a {
                LibraryAction::Open(id) => self.open_work(&id),
                LibraryAction::ToggleFavorite(id) => self.library.toggle_favorite(&id),
                LibraryAction::SetShelf(id, shelf) => self.library.set_shelf(&id, shelf),
                LibraryAction::GoSearch => {
                    self.tab = Tab::Browse;
                    self.catalogs.show_ao3();
                }
                LibraryAction::GoImport => {
                    self.tab = Tab::Settings;
                    self.settings.show_import();
                }
                LibraryAction::NewCollection { name, with } => {
                    if let Some(cid) = self.library.create_collection(&name) {
                        match with {
                            Some(wid) => {
                                self.library.set_in_collection(&cid, &wid, true);
                                self.set_toast(format!("Added to “{name}”"));
                            }
                            None => self.library_screen.show_collection(&cid),
                        }
                    }
                }
                LibraryAction::RenameCollection(cid, name) => self.library.rename_collection(&cid, &name),
                LibraryAction::DeleteCollection(cid) => {
                    let name = self.library.collection(&cid).map(|c| c.name.clone()).unwrap_or_default();
                    self.library.delete_collection(&cid);
                    self.set_toast(format!("Deleted the collection “{name}”. Its books are still in your library."));
                }
                LibraryAction::SetInCollection(cid, wid, on) => {
                    self.library.set_in_collection(&cid, &wid, on);
                    let name = self.library.collection(&cid).map(|c| c.name.clone()).unwrap_or_default();
                    self.set_toast(if on { format!("Added to “{name}”") } else { format!("Removed from “{name}”") });
                }
                LibraryAction::Delete(id) => {
                    if self.reader.as_ref().is_some_and(|r| r.work_id() == id) {
                        self.reader = None;
                        self.deferred.push(Deferred::SetTitle("Flax Reader".into()));
                    }
                    let title = self.library.get_entry(&id).map(|e| e.meta.title.clone()).unwrap_or_default();
                    self.library.delete_work(&id);
                    self.set_toast(format!("Removed “{title}”"));
                }
                LibraryAction::SaveAo3Epub(id) => {
                    let client = self.client.clone();
                    let title = self.library.get_entry(&id).map(|e| e.meta.title.clone()).unwrap_or_else(|| format!("ao3_{id}"));
                    let name = file_name(&title, "epub");
                    self.set_toast("Getting AO3's EPUB…");
                    self.run_job(move || AppEvent::Epub { name, result: client.download_epub(&id) });
                }
                LibraryAction::Export(id, format) => match self.library.load_work(&id) {
                    Some(work) => {
                        let (text, filter, ext) = match format {
                            ExportFormat::Html => (WorkExporter::to_html(&work), "Web page", "html"),
                            ExportFormat::Markdown => (WorkExporter::to_markdown(&work), "Markdown", "md"),
                            ExportFormat::Text => (WorkExporter::to_txt(&work), "Plain text", "txt"),
                        };
                        let name = file_name(&work.meta.title, ext);
                        self.deferred.push(Deferred::SaveFile { name, filter, ext, data: text.into_bytes() });
                    }
                    None => self.set_toast("This work's text isn't on disk. Open it to download it again first."),
                },
            }
        }
    }

    fn check_follows(&mut self, target: Target) {
        let ids: Vec<String> = match target {
            Target::One(id) => vec![id],
            Target::Kind(k) => self.follows.list().iter().filter(|f| f.feed.kind == k).map(|f| f.id()).collect(),
            Target::Stale => self.follows.stale(STALE_MINUTES),
        };
        if ids.is_empty() {
            self.following.checking = false;
            return;
        }
        // A followed story kept in the library goes along, to have its new chapters added.
        let feeds: Vec<(String, Feed, Option<Work>)> = ids
            .into_iter()
            .filter_map(|id| self.follows.get(&id).map(|f| (id, f.feed.clone())))
            .map(|(id, feed)| {
                let saved = crate::sources::story_work_id(&feed).and_then(|w| self.library.load_work(&w));
                (id, feed, saved)
            })
            .collect();
        let Some(w) = &self.slow_worker else { return };
        self.following.checking = true;
        let (client, catalog_client, catalogs) = (self.client.clone(), self.catalog_client.clone(), self.config.catalogs.clone());
        w.run(move || {
            // One feed at a time with a pause between, to go easy on the sites.
            let mut out = Vec::new();
            let mut slow_down = false;
            for (i, (id, feed, saved)) in feeds.into_iter().enumerate() {
                // Told to slow down: the rest wait for the next round, marked as tried, so they
                // aren't asked for again straight away.
                if slow_down {
                    out.push((id, Err("Not checked: AO3 asked to slow down (HTTP 429). It's tried again later.".to_string())));
                    continue;
                }
                if i > 0 {
                    std::thread::sleep(Duration::from_millis(1200));
                }
                let res = crate::sources::check_feed(&feed, saved, &client, &catalog_client, &catalogs);
                slow_down = res.as_ref().is_err_and(|e| e.contains("429"));
                out.push((id, res));
            }
            AppEvent::FollowsChecked(out)
        });
    }

    /// AO3's search is on screen.
    fn on_ao3(&self) -> bool {
        self.tab == Tab::Browse && self.catalogs.on_ao3()
    }

    fn catalog_actions(&mut self, actions: Vec<CatalogAction>) {
        for a in actions {
            match a {
                CatalogAction::Load { catalog, url, append } => {
                    let client = self.catalog_client.clone();
                    self.run_job(move || {
                        let result = client.feed(&url, &catalog);
                        AppEvent::CatalogPage { url, append, result }
                    });
                }
                CatalogAction::Search { catalog, feed, query } => {
                    let client = self.catalog_client.clone();
                    self.run_job(move || {
                        let result = client.search(&feed, &query, &catalog);
                        AppEvent::CatalogPage { url: format!("search:{query}"), append: false, result }
                    });
                }
                CatalogAction::Get { catalog, url, key, title } => {
                    let client = self.catalog_client.clone();
                    // A forum story is read page by page: the slow worker, so Browse stays quick.
                    let story = crate::sources::forum::thread_url(&url).is_some();
                    let job = move || AppEvent::CatalogBook { key, result: client.get_work(&url, "", &catalog, Some(&title)) };
                    if story {
                        self.run_slow(job);
                    } else {
                        self.run_job(job);
                    }
                }
                CatalogAction::FollowShelf { catalog, url, title } => {
                    let feed = Feed { kind: FeedKind::Shelf, name: format!("{title} · {}", catalog.name), link: url, source: catalog.url };
                    self.browse_actions(vec![BrowseAction::Follow(feed)]);
                }
                CatalogAction::FollowStory(url) => {
                    self.following.story_busy = true;
                    self.set_toast("Following the story: saving it to your library first…");
                    self.browse_actions(vec![BrowseAction::FollowStory(url)]);
                }
                CatalogAction::Read(id) => self.open_work(&id),
                CatalogAction::GoImport => {
                    self.tab = Tab::Settings;
                    self.settings.show_import();
                }
                CatalogAction::Save(list) => {
                    self.config.catalogs = list;
                    self.mark_config_dirty();
                }
            }
        }
    }

    fn browse_actions(&mut self, actions: Vec<BrowseAction>) {
        for a in actions {
            match a {
                BrowseAction::Search(q, page) => {
                    let client = self.client.clone();
                    self.search.busy = true;
                    self.run_job(move || AppEvent::Searched { page, result: client.search(&q, page) });
                }
                BrowseAction::Read(id) => {
                    if self.library.get_entry(&id).is_some() {
                        // In the library (downloaded or not): opening fetches what's missing.
                        self.open_work(&id);
                    } else if id.chars().all(|c| c.is_ascii_digit()) {
                        self.set_toast("Downloading the work…");
                        self.start_fetch(id, true);
                    } else {
                        self.set_toast("That book isn't in your library any more. Get it again from its catalogue.");
                    }
                }
                BrowseAction::Save(id) => {
                    self.set_toast("Saving the work to your library…");
                    self.start_fetch(id, false);
                }
                BrowseAction::Follow(feed) => {
                    let (name, kind) = (feed.name.clone(), feed.kind);
                    match self.follows.add(feed) {
                        Ok(fid) => {
                            self.set_toast(format!("Following {name}"));
                            self.following.show_kind(kind);
                            self.check_follows(Target::One(fid));
                        }
                        Err(e) => self.set_toast(e),
                    }
                }
                BrowseAction::Unfollow(fid) => {
                    let _ = self.follows.remove(&fid);
                    self.set_toast("Unfollowed");
                }
                BrowseAction::Check(target) => self.check_follows(target),
                BrowseAction::MarkRead(target) => {
                    let _ = match &target {
                        Target::One(id) => self.follows.mark_read(Scope::One(id)),
                        Target::Kind(k) => self.follows.mark_read(Scope::Kind(*k)),
                        Target::Stale => Ok(()),
                    };
                }
                BrowseAction::FeedPage(feed, page) => {
                    let client = self.client.clone();
                    self.run_job(move || AppEvent::FeedPage(client.feed(&feed, page)));
                }
                BrowseAction::OpenUrl(url) => self.deferred.push(Deferred::Open(url)),
                BrowseAction::LookUp(id) => {
                    let client = self.client.clone();
                    self.run_job(move || AppEvent::LookedUp { result: client.work_meta(&id), id });
                }
                BrowseAction::Epub(work_id) => {
                    let client = self.client.clone();
                    let known = self.search.link.as_ref().and_then(|l| l.meta.as_ref()).map(|m| m.title.clone());
                    let title = known.or_else(|| self.library.get_entry(&work_id).map(|e| e.meta.title.clone())).unwrap_or_else(|| format!("ao3_{work_id}"));
                    let name = file_name(&title, "epub");
                    self.run_job(move || AppEvent::Epub { name, result: client.download_epub(&work_id) });
                }
                BrowseAction::GoAccount => self.tab = Tab::Account,
                BrowseAction::FollowStory(url) => {
                    let client = self.catalog_client.clone();
                    self.run_slow(move || AppEvent::StoryFollowed(crate::sources::follow_story(&url, &client)));
                }
                BrowseAction::GetBook { key, href, mime, catalog, title } => {
                    let client = self.catalog_client.clone();
                    let catalog = crate::sources::builtin()
                        .into_iter()
                        .chain(self.config.catalogs.iter().cloned())
                        .find(|c| c.url == catalog)
                        .unwrap_or(crate::sources::Catalog { name: title.clone(), url: catalog, ..Default::default() });
                    self.catalogs.getting.insert(key.clone(), Getting::Busy);
                    self.run_job(move || AppEvent::CatalogBook { key, result: client.get_work(&href, &mime, &catalog, Some(&title)) });
                }
                BrowseAction::GetStory { key, url, title } => {
                    let client = self.catalog_client.clone();
                    self.catalogs.getting.insert(key.clone(), Getting::Busy);
                    let catalog = crate::sources::Catalog { name: title, url: url.clone(), ..Default::default() };
                    self.run_slow(move || AppEvent::CatalogBook { key, result: client.get_work(&url, crate::sources::opds::STORY_MIME, &catalog, None) });
                }
                BrowseAction::OpenItem { url, id } => {
                    let _ = self.follows.mark_work_read(&id);
                    self.deferred.push(Deferred::Open(url));
                }
            }
        }
    }

    fn account_frame(&mut self, ui: &mut Ui, body: Rect) {
        let session = self.config.session.clone();
        let forums = self.config.forum_accounts.clone();
        for a in self.account.frame(ui, body, &session, &forums) {
            match a {
                AccountAction::Login { username, password } => {
                    let client = self.client.clone();
                    self.run_job(move || AppEvent::Login(client.login(&username, &password)));
                }
                AccountAction::ApplyCookie(cookie) => {
                    self.client.set_cookie("_otw_remember_me", &cookie);
                    self.pending_cookie = Some(cookie);
                    let client = self.client.clone();
                    self.run_job(move || AppEvent::SessionUser(client.current_user()));
                }
                AccountAction::Logout => {
                    self.config.session = UserSession::default();
                    let _ = self.config.save();
                    self.client = Ao3Client::new(None);
                    self.account.set_message(AO3, "Signed out of AO3.", false);
                    self.set_toast("Signed out of AO3");
                }
                AccountAction::ImportAo3(list) => match session.username.clone() {
                    Some(user) => {
                        let client = self.client.clone();
                        self.run_slow(move || AppEvent::Ao3List { list, result: client.whole_user_list(&user, list, 50) });
                    }
                    None => self.account.set_message(AO3, "Sign in again to bring in your lists: AO3 didn't say which account this is.", true),
                },
                AccountAction::ForumLogin { host, username, password } => match crate::sources::forum::SITES.iter().find(|s| s.host == host) {
                    Some(site) => self.run_job(move || AppEvent::ForumLogin { result: crate::sources::accounts::login(site, &username, &password), host }),
                    None => self.account.set_message(&host, "Not a forum Flax Reader knows.", true),
                },
                AccountAction::ForumLogout(host) => {
                    if let Some(account) = self.config.forum_accounts.iter().find(|a| a.host == host).cloned() {
                        // Sign out on the forum too, so the saved cookie stops working; nothing to wait for.
                        self.run_slow(move || {
                            crate::sources::accounts::logout(&account);
                            AppEvent::Done
                        });
                    }
                    self.config.forum_accounts.retain(|a| a.host != host);
                    let _ = self.config.save();
                    self.account.set_message(&host, "Signed out.", false);
                }
                AccountAction::ImportWatched(host) => {
                    if let Some(account) = self.config.forum_accounts.iter().find(|a| a.host == host).cloned() {
                        self.run_slow(move || AppEvent::Watched { result: crate::sources::accounts::watched_threads(&account), host });
                    }
                }
                AccountAction::GoSources => {
                    self.tab = Tab::Settings;
                    self.settings.show_sources();
                }
            }
        }
    }

    fn settings_frame(&mut self, ui: &mut Ui, body: Rect) {
        if self.font_families.is_none() && self.settings.needs_fonts() {
            self.font_families = Some(ui.g.system_font_families());
        }
        let storage_dir = AppConfig::get_app_dir();
        let mut ctx = SettingsCtx {
            system_dark: self.system_dark,
            config: &mut self.config,
            update: &self.update_status,
            fonts: self.font_families.as_deref().unwrap_or(&[]),
            storage_dir: storage_dir.to_string_lossy().into_owned(),
        };
        let actions = self.settings.frame(ui, body, &mut ctx);
        for a in actions {
            match a {
                SettingsAction::ConfigChanged => {
                    self.follow_system();
                    self.mark_config_dirty();
                }
                SettingsAction::PickColor(text) => {
                    let current = if text { self.config.custom_text } else { self.config.custom_bg };
                    self.deferred.push(Deferred::PickColor { text, current });
                }
                SettingsAction::CheckUpdates => self.check_updates(),
                SettingsAction::PickFontFile => self.deferred.push(Deferred::PickFontFile),
                SettingsAction::Open(target) => self.deferred.push(Deferred::Open(target)),
                SettingsAction::Import => self.deferred.push(Deferred::ImportFile),
                SettingsAction::OpenWork(id) => self.open_work(&id),
                SettingsAction::AddCatalog => {
                    self.tab = Tab::Browse;
                    self.catalogs.start_adding();
                }
                SettingsAction::CatalogsChanged => self.catalogs.reset(),
                SettingsAction::Account => self.tab = Tab::Account,
                SettingsAction::ClearCache => {
                    let n = self.library.clear_downloads();
                    self.set_toast(format!("Cleared {n} downloaded works. They download again when you open them."));
                }
            }
        }
    }

    fn toast_overlay(&mut self, ui: &mut Ui) {
        let Some((msg, at)) = &self.toast else { return };
        let left = Duration::from_secs(TOAST_SECS).saturating_sub(at.elapsed());
        if left.is_zero() {
            self.toast = None;
            ui.request_repaint();
            return;
        }
        ui.wake_after(left.as_millis() as u32 + 1);
        let c = ui.c;
        let (w, h) = ui.size;
        let style = TextStyle::ui(13.5);
        let t = ui.measure(msg, &style, TextOpts::wrap((w - 120.0).min(520.0)));
        let r = Rect::new((w - t.w - 56.0) / 2.0, h - t.h - 48.0, t.w + 56.0, t.h + 22.0);
        ui.g.fill_round(Rect::new(r.x, r.y + 2.0, r.w, r.h), 8.0, Color::from_u8(0, 0, 0).alpha(0.16));
        ui.g.fill_round(r, 8.0, c.surface);
        ui.g.stroke_round(r, 8.0, 1.0, c.border);
        let it = ui.measure(icons::INFO, &TextStyle::icons(14.0), TextOpts::default());
        ui.g.draw_text(it, r.x + 16.0, r.y + (r.h - it.h) / 2.0, c.accent_text);
        ui.g.draw_text(t, r.x + 40.0, r.y + 11.0, c.text);
    }
}
