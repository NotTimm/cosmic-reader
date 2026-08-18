use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use cosmic::app::{Core, Task};
use cosmic::dialog::file_chooser::{self, FileFilter};
use cosmic::iced::{
    event,
    keyboard::{self, key::Named, Key},
    mouse, touch, window, Alignment, Background, Color, ContentFit, Length, Point, Subscription,
};
use cosmic::iced::widget::scrollable as scroll_mod;
use cosmic::iced::widget::mouse_area;
use cosmic::widget::{self, icon};
use cosmic::{executor, Application, ApplicationExt, Element};
use url::Url;

use crate::comic::{self, ChapterInfo, PageSource};
use crate::comicvine::{self, ComicVineMatch};
use crate::library::{self, SeriesEntry};
use crate::metadata::{self, ParsedFilename, SeriesMetadata};

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 6.0;
const ZOOM_STEP: f32 = 1.2;

pub const APP_ID: &str = "com.tsingel.CosmicComic";

const PRELOAD_RADIUS: usize = 3;
const CACHE_RADIUS: usize = 10;
const PAN_STEP: f32 = 80.0;
const THUMB_W: u32 = 120;
const THUMB_H: u32 = 170;

// ── View modes ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum AppView { Library, Reader }

#[derive(Clone, Debug, PartialEq)]
pub enum Layout { Single, Dual }

// ── Messages ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Message {
    // Navigation
    GoToLibrary,
    GoToReader,
    OpenFile,
    OpenFolder,
    PathSelected(Url),
    OpenFromLibrary { path: PathBuf, page: usize, chapter: usize },
    // Loading
    Loaded(PathBuf, Vec<PageSource>, Vec<ChapterInfo>),
    PageDecoded(usize, Result<Arc<comic::Page>, String>),
    LoadFailed(String),
    Cancelled,
    // Reading
    NextPage,
    PrevPage,
    CloseError,
    // View toggles
    ToggleFullscreen,
    ToggleTheater,
    ToggleLayout,
    ToggleZoom,
    ToggleInfo,
    ToggleChapterSelect,
    SelectChapter(usize),
    // Copy
    CopyPage,
    CopiedPage(Result<(), String>),
    // Metadata
    MetadataLoaded(Result<SeriesMetadata, String>),
    ComicVineLoaded(Result<ComicVineMatch, String>),
    // Chapter covers
    ChapterCoverReady { chapter_idx: usize, result: Result<(u32, u32, Vec<u8>), String> },
    // Library
    LibrarySearchChanged(String),
    LibraryCoverReady { series_id: i64, path: PathBuf },
    // Keys / pointer / touch
    Key(Key),
    ModifiersChanged(keyboard::Modifiers),
    WheelScrolled(mouse::ScrollDelta),
    TrackpadZoom(f32),
    Touch(touch::Event),
    // Drag and drop
    FilesDropped(Vec<PathBuf>),
    DndDataReceived(String, Vec<u8>),
}

// ── State ─────────────────────────────────────────────────────────────────────

pub struct App {
    core: Core,
    // view
    app_view: AppView,
    // reader
    title: String,
    open_path: Option<PathBuf>,
    sources: Vec<PageSource>,
    chapters: Vec<ChapterInfo>,
    cache: HashMap<usize, (widget::image::Handle, Arc<comic::Page>)>,
    pending: HashSet<usize>,
    current_page: usize,
    loading: bool,
    error: Option<String>,
    fullscreen: bool,
    theater_mode: bool,
    layout: Layout,
    zoom_active: bool,
    zoom: f32,
    scroll_id: widget::Id,
    show_info: bool,
    show_chapter_select: bool,
    metadata: Option<SeriesMetadata>,
    metadata_loading: bool,
    parsed_filename: Option<ParsedFilename>,
    comicvine: Option<Result<ComicVineMatch, String>>,
    comicvine_loading: bool,
    // pointer / touch state
    modifiers: keyboard::Modifiers,
    touches: HashMap<touch::Finger, Point>,
    pinch_last_dist: Option<f32>,
    // chapter covers (decoded for the currently open series)
    chapter_covers: HashMap<usize, widget::image::Handle>,
    covers_pending: HashSet<usize>,
    // library
    db: Option<rusqlite::Connection>,
    library_entries: Vec<SeriesEntry>,
    library_search: String,
    library_covers: HashMap<i64, widget::image::Handle>,
}

// ── Task helpers ──────────────────────────────────────────────────────────────

fn open_task(path: PathBuf) -> Task<Message> {
    cosmic::task::future(async move {
        let list_path = path.clone();
        match tokio::task::spawn_blocking(move || comic::collect_sources(&list_path)).await {
            Ok(Ok((sources, chapters))) => Message::Loaded(path, sources, chapters),
            Ok(Err(e)) => Message::LoadFailed(e),
            Err(e) => Message::LoadFailed(format!("listing task panicked: {e}")),
        }
    })
}

fn decode_task(index: usize, source: PageSource) -> Task<Message> {
    cosmic::task::future(async move {
        let result = tokio::task::spawn_blocking(move || comic::decode_source(&source)).await;
        Message::PageDecoded(
            index,
            match result {
                Ok(r) => r.map(Arc::new),
                Err(e) => Err(format!("decode panicked: {e}")),
            },
        )
    })
}

fn copy_task(page: Arc<comic::Page>) -> Task<Message> {
    cosmic::task::future(async move {
        let result = tokio::task::spawn_blocking(move || {
            let mut cb = arboard::Clipboard::new()
                .map_err(|e| format!("clipboard: {e}"))?;
            cb.set_image(arboard::ImageData {
                width: page.width as usize,
                height: page.height as usize,
                bytes: Cow::Borrowed(&page.rgba),
            })
            .map_err(|e| format!("copy: {e}"))
        })
        .await;
        Message::CopiedPage(match result {
            Ok(r) => r,
            Err(e) => Err(format!("copy panicked: {e}")),
        })
    })
}

fn metadata_task(raw_name: String) -> Task<Message> {
    cosmic::task::future(async move {
        let title = metadata::extract_title(&raw_name);
        Message::MetadataLoaded(metadata::fetch_series_metadata(&title).await)
    })
}

fn comicvine_task(parsed: ParsedFilename) -> Task<Message> {
    cosmic::task::future(async move {
        let query = match parsed.issue {
            Some(n) => format!("{} #{n}", parsed.series),
            None => parsed.series.clone(),
        };
        Message::ComicVineLoaded(comicvine::search_issue(&query).await)
    })
}

/// Decode the first page of `chapter_idx`, thumbnail it, save to disk, return rgba.
fn cover_task(chapter_idx: usize, source: PageSource, series_path: PathBuf) -> Task<Message> {
    cosmic::task::future(async move {
        let result = tokio::task::spawn_blocking(move || {
            let page = comic::decode_source(&source)?;
            let img = image::RgbaImage::from_raw(page.width, page.height, page.rgba)
                .ok_or_else(|| "bad rgba".to_string())?;
            let thumb = image::imageops::resize(
                &img, THUMB_W, THUMB_H, image::imageops::FilterType::Triangle,
            );
            // Save to disk cache
            let cover_path = library::chapter_cover_path(
                &series_path.to_string_lossy(),
                chapter_idx,
            );
            if let Some(parent) = cover_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir: {e}"))?;
            }
            // Save as PNG
            thumb.save(&cover_path)
                .map_err(|e| format!("save cover: {e}"))?;
            let (w, h) = thumb.dimensions();
            Ok::<_, String>((w, h, thumb.into_raw()))
        })
        .await;
        Message::ChapterCoverReady {
            chapter_idx,
            result: match result {
                Ok(r) => r,
                Err(e) => Err(format!("cover panicked: {e}")),
            },
        }
    })
}

/// Load a chapter cover thumbnail from disk (non-blocking since it's just a path handle).
fn load_cover_from_disk(series_id: i64, path: PathBuf) -> Task<Message> {
    // Handle::from_path is lazy; we just emit the message immediately.
    cosmic::task::future(async move {
        Message::LibraryCoverReady { series_id, path }
    })
}

// ── App helpers ───────────────────────────────────────────────────────────────

impl App {
    fn refresh_window(&mut self) -> Task<Message> {
        let len = self.sources.len();
        if len == 0 {
            return Task::none();
        }
        let keep_start = self.current_page.saturating_sub(CACHE_RADIUS);
        let keep_end = (self.current_page + CACHE_RADIUS).min(len - 1);
        self.cache.retain(|idx, _| (keep_start..=keep_end).contains(idx));

        let mut tasks = Vec::new();
        for offset in 0..=PRELOAD_RADIUS {
            for idx in [
                self.current_page.checked_sub(offset),
                self.current_page.checked_add(offset),
            ] {
                let Some(idx) = idx else { continue };
                if idx >= len || self.cache.contains_key(&idx) || self.pending.contains(&idx) {
                    continue;
                }
                self.pending.insert(idx);
                tasks.push(decode_task(idx, self.sources[idx].clone()));
            }
        }
        Task::batch(tasks)
    }

    fn page_step(&self) -> usize {
        if self.layout == Layout::Dual { 2 } else { 1 }
    }

    /// Multiplies the current zoom level by `factor`, clamped, and switches
    /// into zoom/pan mode if not already active.
    fn apply_zoom(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.zoom_active = true;
    }

    /// Tracks active touch points and turns a two-finger pinch into a zoom
    /// change (touchscreen pinch-to-zoom).
    fn handle_touch(&mut self, event: touch::Event) {
        match event {
            touch::Event::FingerPressed { id, position } => {
                self.touches.insert(id, position);
                if self.touches.len() != 2 {
                    self.pinch_last_dist = None;
                }
            }
            touch::Event::FingerMoved { id, position } => {
                self.touches.insert(id, position);
                if self.app_view == AppView::Reader && self.touches.len() == 2 {
                    let mut pts = self.touches.values().copied();
                    let (a, b) = (pts.next().unwrap(), pts.next().unwrap());
                    let dist = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                    if let Some(prev) = self.pinch_last_dist {
                        if prev > 1.0 {
                            self.apply_zoom(dist / prev);
                        }
                    }
                    self.pinch_last_dist = Some(dist);
                }
            }
            touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. } => {
                self.touches.remove(&id);
                if self.touches.len() < 2 {
                    self.pinch_last_dist = None;
                }
            }
        }
    }

    fn handle(&self, idx: usize) -> Option<&widget::image::Handle> {
        self.cache.get(&idx).map(|(h, _)| h)
    }

    fn page_data(&self, idx: usize) -> Option<Arc<comic::Page>> {
        self.cache.get(&idx).map(|(_, p)| p.clone())
    }

    fn current_chapter(&self) -> Option<(usize, &ChapterInfo, usize)> {
        self.chapters.iter().enumerate().find_map(|(i, ch)| {
            if self.current_page >= ch.start && self.current_page < ch.start + ch.page_count {
                Some((i, ch, self.current_page - ch.start))
            } else {
                None
            }
        })
    }

    /// Queue cover generation for chapters that don't have covers yet.
    fn queue_missing_covers(&mut self) -> Task<Message> {
        let Some(series_path) = &self.open_path else { return Task::none() };
        let series_path = series_path.clone();
        let path_str = series_path.to_string_lossy().to_string();

        let mut tasks = Vec::new();
        for (ch_idx, ch) in self.chapters.iter().enumerate() {
            if self.chapter_covers.contains_key(&ch_idx)
                || self.covers_pending.contains(&ch_idx)
            {
                continue;
            }
            // Check disk cache first
            let cover_path = library::chapter_cover_path(&path_str, ch_idx);
            if cover_path.exists() {
                let handle = widget::image::Handle::from_path(&cover_path);
                self.chapter_covers.insert(ch_idx, handle);
                continue;
            }
            // Need to generate
            if ch.page_count > 0 {
                self.covers_pending.insert(ch_idx);
                let source = self.sources[ch.start].clone();
                tasks.push(cover_task(ch_idx, source, series_path.clone()));
            }
        }
        Task::batch(tasks)
    }

    /// Save reading progress to DB (cheap, called on page change).
    fn persist_progress(&self) {
        let Some(db) = &self.db else { return };
        let Some(path) = &self.open_path else { return };
        let ch_idx = self.current_chapter().map(|(i, _, _)| i).unwrap_or(0);
        let _ = library::save_progress(db, &path.to_string_lossy(), self.current_page, ch_idx);
    }

    fn reload_library(&mut self) -> Task<Message> {
        let Some(db) = &self.db else { return Task::none() };

        let entries = if self.library_search.is_empty() {
            library::all_series(db)
        } else {
            library::search_series(db, &self.library_search)
        };

        // Load series covers from disk (ch0 of each series)
        let mut cover_tasks = Vec::new();
        for entry in &entries {
            if self.library_covers.contains_key(&entry.id) {
                continue;
            }
            let cover_path = library::chapter_cover_path(&entry.path, 0);
            if cover_path.exists() {
                cover_tasks.push(load_cover_from_disk(entry.id, cover_path));
            }
        }

        self.library_entries = entries;
        Task::batch(cover_tasks)
    }
}

// ── Application ───────────────────────────────────────────────────────────────

impl Application for App {
    type Executor = executor::Default;
    type Flags = Option<PathBuf>;
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core { &self.core }
    fn core_mut(&mut self) -> &mut Core { &mut self.core }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let db = library::open_db().ok();

        let mut app = App {
            core,
            app_view: if flags.is_none() { AppView::Library } else { AppView::Reader },
            title: "Cosmic Comic".to_string(),
            open_path: None,
            sources: Vec::new(),
            chapters: Vec::new(),
            cache: HashMap::new(),
            pending: HashSet::new(),
            current_page: 0,
            loading: flags.is_some(),
            error: None,
            fullscreen: false,
            theater_mode: false,
            layout: Layout::Single,
            zoom_active: false,
            zoom: 1.0,
            scroll_id: widget::Id::new("viewer"),
            show_info: false,
            show_chapter_select: false,
            metadata: None,
            metadata_loading: false,
            parsed_filename: None,
            comicvine: None,
            comicvine_loading: false,
            modifiers: keyboard::Modifiers::default(),
            touches: HashMap::new(),
            pinch_last_dist: None,
            chapter_covers: HashMap::new(),
            covers_pending: HashSet::new(),
            db,
            library_entries: Vec::new(),
            library_search: String::new(),
            library_covers: HashMap::new(),
        };
        app.set_header_title("Cosmic Comic".into());

        let mut tasks = vec![match app.core.main_window_id() {
            Some(id) => app.set_window_title("Cosmic Comic".into(), id),
            None => Task::none(),
        }];

        if let Some(path) = flags {
            tasks.push(open_task(path));
        } else {
            // Start on library view — load entries
            tasks.push(app.reload_library());
        }

        (app, Task::batch(tasks))
    }

    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        if self.app_view == AppView::Reader {
            vec![widget::button::icon(icon::from_name("go-home-symbolic"))
                .on_press(Message::GoToLibrary)
                .into()]
        } else {
            vec![]
        }
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        let mut els: Vec<Element<'_, Message>> = Vec::new();

        match self.app_view {
            AppView::Library => {
                els.push(
                    widget::text_input("Search library…", &self.library_search)
                        .on_input(Message::LibrarySearchChanged)
                        .width(Length::Fixed(220.0))
                        .into(),
                );
                els.push(
                    widget::button::standard("Open File")
                        .on_press(Message::OpenFile)
                        .into(),
                );
                els.push(
                    widget::button::suggested("Add Series")
                        .on_press(Message::OpenFolder)
                        .into(),
                );
            }
            AppView::Reader => {
                if !self.sources.is_empty() {
                    // Chapter-aware counter
                    let counter = if let Some((ch_idx, ch, pg)) = self.current_chapter() {
                        if self.chapters.len() == 1 {
                            format!("{} / {}", pg + 1, ch.page_count)
                        } else {
                            format!("Ch.{}  ·  {} / {}", ch_idx + 1, pg + 1, ch.page_count)
                        }
                    } else {
                        format!("{} / {}", self.current_page + 1, self.sources.len())
                    };

                    els.push(
                        widget::button::icon(icon::from_name("go-previous-symbolic"))
                            .on_press(Message::PrevPage)
                            .into(),
                    );
                    els.push(widget::text(counter).into());
                    els.push(
                        widget::button::icon(icon::from_name("go-next-symbolic"))
                            .on_press(Message::NextPage)
                            .into(),
                    );
                    els.push(widget::divider::vertical::light().into());

                    // Chapter select (only for multi-chapter series)
                    if self.chapters.len() > 1 {
                        let ch_label = if self.show_chapter_select { "Chapters ▾" } else { "Chapters" };
                        els.push(
                            widget::button::standard(ch_label)
                                .on_press(Message::ToggleChapterSelect)
                                .into(),
                        );
                    }

                    let layout_label = match self.layout {
                        Layout::Single => "1 Page",
                        Layout::Dual => "2 Pages",
                    };
                    els.push(
                        widget::button::standard(layout_label)
                            .on_press(Message::ToggleLayout)
                            .into(),
                    );

                    let zoom_label = if self.zoom_active {
                        format!("Zoom {:.0}%", self.zoom * 100.0)
                    } else {
                        "Zoom: Off".to_string()
                    };
                    els.push(
                        widget::button::standard(zoom_label)
                            .on_press(Message::ToggleZoom)
                            .into(),
                    );

                    els.push(
                        widget::button::icon(icon::from_name("edit-copy-symbolic"))
                            .on_press(Message::CopyPage)
                            .into(),
                    );
                    els.push(
                        widget::button::icon(icon::from_name("dialog-information-symbolic"))
                            .on_press(Message::ToggleInfo)
                            .into(),
                    );
                    els.push(widget::divider::vertical::light().into());
                }

                let theater_label = if self.theater_mode { "Theater: On" } else { "Theater: Off" };
                els.push(
                    widget::button::standard(theater_label)
                        .on_press(Message::ToggleTheater)
                        .into(),
                );
                let fs_icon = if self.fullscreen {
                    icon::from_name("view-restore-symbolic")
                } else {
                    icon::from_name("view-fullscreen-symbolic")
                };
                els.push(
                    widget::button::icon(fs_icon)
                        .on_press(Message::ToggleFullscreen)
                        .into(),
                );
                els.push(widget::divider::vertical::light().into());
                els.push(
                    widget::button::standard("Open")
                        .on_press(Message::OpenFile)
                        .into(),
                );
                els.push(
                    widget::button::standard("Open Series")
                        .on_press(Message::OpenFolder)
                        .into(),
                );
            }
        }
        els
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        event::listen_with(|event, status, _| match event {
            event::Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => match status {
                event::Status::Ignored => Some(Message::Key(key)),
                event::Status::Captured => None,
            },
            event::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                Some(Message::ModifiersChanged(m))
            }
            event::Event::Mouse(mouse::Event::WheelScrolled { delta }) => match status {
                event::Status::Ignored => Some(Message::WheelScrolled(delta)),
                event::Status::Captured => None,
            },
            event::Event::Mouse(mouse::Event::WheelZoomed { delta }) => match status {
                event::Status::Ignored => Some(Message::TrackpadZoom(delta)),
                event::Status::Captured => None,
            },
            event::Event::Touch(t) => Some(Message::Touch(t)),
            event::Event::Window(window::Event::FileDropped(paths)) => {
                Some(Message::FilesDropped(paths))
            }
            _ => None,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::GoToLibrary => {
                self.app_view = AppView::Library;
                self.show_chapter_select = false;
                self.show_info = false;
                self.set_header_title("Cosmic Comic — Library".into());
                return self.reload_library();
            }
            Message::GoToReader => {
                self.app_view = AppView::Reader;
                self.set_header_title(self.title.clone());
            }
            Message::OpenFile => {
                return cosmic::task::future(async move {
                    let filter = FileFilter::new("Comic Archives")
                        .glob("*.cbz").glob("*.cbr").glob("*.zip").glob("*.rar");
                    let dialog = file_chooser::open::Dialog::new()
                        .title("Open Comic").filter(filter);
                    match dialog.open_file().await {
                        Ok(r) => Message::PathSelected(r.url().to_owned()),
                        Err(file_chooser::Error::Cancelled) => Message::Cancelled,
                        Err(e) => Message::LoadFailed(e.to_string()),
                    }
                });
            }
            Message::OpenFolder => {
                return cosmic::task::future(async move {
                    let dialog = file_chooser::open::Dialog::new().title("Open Comic Series");
                    match dialog.open_folder().await {
                        Ok(r) => Message::PathSelected(r.url().to_owned()),
                        Err(file_chooser::Error::Cancelled) => Message::Cancelled,
                        Err(e) => Message::LoadFailed(e.to_string()),
                    }
                });
            }
            Message::PathSelected(url) => {
                let path = match url.scheme() {
                    "file" => match url.to_file_path() {
                        Ok(p) => p,
                        Err(()) => {
                            self.error = Some(format!("invalid file path: {url}"));
                            return Task::none();
                        }
                    },
                    other => {
                        self.error = Some(format!("unsupported scheme: {other}"));
                        return Task::none();
                    }
                };
                self.loading = true;
                self.error = None;
                self.app_view = AppView::Reader;
                return open_task(path);
            }
            Message::OpenFromLibrary { path, page, chapter: _ } => {
                self.loading = true;
                self.error = None;
                self.app_view = AppView::Reader;
                // We'll resume at the saved page after Loaded
                // Store page temporarily in current_page; reset properly in Loaded
                self.current_page = page;
                return open_task(path);
            }
            Message::Loaded(path, sources, chapters) => {
                let path_str = path.to_string_lossy().to_string();
                let resume_page = self.current_page; // may have been set by OpenFromLibrary
                let total = sources.len();

                self.loading = false;
                self.sources = sources;
                self.chapters = chapters;
                self.cache.clear();
                self.pending.clear();
                self.chapter_covers.clear();
                self.covers_pending.clear();
                self.zoom_active = false;
                self.zoom = 1.0;
                self.metadata = None;
                self.metadata_loading = false;
                self.comicvine = None;
                self.comicvine_loading = false;
                self.show_chapter_select = false;
                self.open_path = Some(path.clone());

                self.title = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Comic".to_string());
                self.set_header_title(self.title.clone());

                // Restore saved progress (but only if we have a saved position and
                // OpenFromLibrary set resume_page, OR check DB)
                let saved_page = if resume_page > 0 {
                    resume_page.min(total.saturating_sub(1))
                } else if let Some(db) = &self.db {
                    library::get_progress(db, &path_str)
                        .map(|(pg, _)| pg)
                        .unwrap_or(0)
                        .min(total.saturating_sub(1))
                } else {
                    0
                };
                self.current_page = saved_page;

                // Update library DB
                if let Some(db) = &self.db {
                    let title = metadata::extract_title(&self.title);
                    let _ = library::upsert_series(db, &path_str, &title, total);
                }

                let raw_name = self.title.clone();
                self.metadata_loading = true;
                let parsed = metadata::parse_filename(&raw_name);
                self.comicvine_loading = true;
                self.parsed_filename = Some(parsed.clone());

                let mut tasks =
                    vec![self.refresh_window(), metadata_task(raw_name), comicvine_task(parsed)];
                if let Some(id) = self.core.main_window_id() {
                    tasks.push(self.set_window_title(self.title.clone(), id));
                }
                return Task::batch(tasks);
            }
            Message::PageDecoded(index, result) => {
                self.pending.remove(&index);
                if let Ok(page) = result {
                    let handle = widget::image::Handle::from_rgba(
                        page.width, page.height, page.rgba.clone(),
                    );
                    self.cache.insert(index, (handle, page));
                }
            }
            Message::LoadFailed(why) => {
                self.loading = false;
                self.error = Some(why);
            }
            Message::Cancelled => { self.loading = false; }
            Message::MetadataLoaded(result) => {
                self.metadata_loading = false;
                if let Ok(meta) = result {
                    if let Some(db) = &self.db {
                        if let Some(path) = &self.open_path {
                            let _ = library::update_metadata(db, &path.to_string_lossy(), &meta);
                        }
                    }
                    self.metadata = Some(meta);
                }
            }
            Message::ComicVineLoaded(result) => {
                self.comicvine_loading = false;
                self.comicvine = Some(result);
            }
            Message::ChapterCoverReady { chapter_idx, result } => {
                self.covers_pending.remove(&chapter_idx);
                if let Ok((w, h, rgba)) = result {
                    let handle = widget::image::Handle::from_rgba(w, h, rgba);
                    self.chapter_covers.insert(chapter_idx, handle);
                    // If this is ch0, update library covers for the current series
                    if chapter_idx == 0 {
                        if let Some(db) = &self.db {
                            if let Some(path) = &self.open_path {
                                // Look up the series id and update library_covers
                                let path_str = path.to_string_lossy().to_string();
                                let entries = library::search_series(db, &self.title);
                                if let Some(e) = entries.iter().find(|e| e.path == path_str) {
                                    let id = e.id;
                                    if let Some(h) = self.chapter_covers.get(&0) {
                                        self.library_covers.insert(id, h.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Message::LibraryCoverReady { series_id, path } => {
                let handle = widget::image::Handle::from_path(path);
                self.library_covers.insert(series_id, handle);
            }
            Message::NextPage => {
                let step = self.page_step();
                let new = (self.current_page + step).min(self.sources.len().saturating_sub(1));
                if new != self.current_page {
                    self.current_page = new;
                    self.persist_progress();
                    return self.refresh_window();
                }
            }
            Message::PrevPage => {
                let step = self.page_step();
                let new = self.current_page.saturating_sub(step);
                if new != self.current_page {
                    self.current_page = new;
                    self.persist_progress();
                    return self.refresh_window();
                }
            }
            Message::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                let mode = if self.fullscreen { window::Mode::Fullscreen } else { window::Mode::Windowed };
                if let Some(id) = self.core.main_window_id() {
                    return window::set_mode(id, mode);
                }
            }
            Message::ToggleTheater => { self.theater_mode = !self.theater_mode; }
            Message::ToggleLayout => {
                self.layout = match self.layout { Layout::Single => Layout::Dual, Layout::Dual => Layout::Single };
                if self.layout == Layout::Dual && self.current_page % 2 != 0 {
                    self.current_page = self.current_page.saturating_sub(1);
                }
                return self.refresh_window();
            }
            Message::ToggleZoom => {
                self.zoom_active = !self.zoom_active;
                self.zoom = 1.0;
            }
            Message::ToggleInfo => {
                self.show_info = !self.show_info;
                if self.show_info { self.show_chapter_select = false; }
            }
            Message::ToggleChapterSelect => {
                self.show_chapter_select = !self.show_chapter_select;
                if self.show_chapter_select {
                    self.show_info = false;
                    return self.queue_missing_covers();
                }
            }
            Message::SelectChapter(ch_idx) => {
                if let Some(ch) = self.chapters.get(ch_idx) {
                    self.current_page = ch.start;
                    self.show_chapter_select = false;
                    self.persist_progress();
                    return self.refresh_window();
                }
            }
            Message::CopyPage => {
                if let Some(page) = self.page_data(self.current_page) {
                    return copy_task(page);
                }
            }
            Message::CopiedPage(r) => { if let Err(e) = r { self.error = Some(e); } }
            Message::CloseError => { self.error = None; }
            Message::LibrarySearchChanged(q) => {
                self.library_search = q;
                return self.reload_library();
            }
            Message::Key(key) => {
                return self.handle_key(key);
            }
            Message::ModifiersChanged(m) => {
                self.modifiers = m;
            }
            Message::WheelScrolled(delta) => {
                if self.app_view == AppView::Reader
                    && self.modifiers.control()
                    && !self.sources.is_empty()
                {
                    let dy = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => y,
                        mouse::ScrollDelta::Pixels { y, .. } => y / 40.0,
                    };
                    if dy != 0.0 {
                        self.apply_zoom(ZOOM_STEP.powf(dy.signum()));
                    }
                }
            }
            Message::TrackpadZoom(delta) => {
                if self.app_view == AppView::Reader && !self.sources.is_empty() {
                    self.apply_zoom(1.0 + delta);
                }
            }
            Message::Touch(event) => self.handle_touch(event),
            Message::FilesDropped(paths) => {
                if let Some(path) = paths.into_iter().next() {
                    self.loading = true;
                    self.error = None;
                    self.app_view = AppView::Reader;
                    return open_task(path);
                }
            }
            Message::DndDataReceived(mime, data) => {
                if mime.contains("uri-list") {
                    if let Some(path) = first_path_from_uri_list(&data) {
                        self.loading = true;
                        self.error = None;
                        self.app_view = AppView::Reader;
                        return open_task(path);
                    }
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let content = match self.app_view {
            AppView::Library => self.view_library(),
            AppView::Reader => self.view_reader(),
        };

        widget::dnd_destination(content, vec![Cow::Borrowed("text/uri-list")])
            .on_finish(|mime, data, _action, _x, _y| Message::DndDataReceived(mime, data))
            .on_data_received(Message::DndDataReceived)
            .into()
    }
}

// ── Key handling ──────────────────────────────────────────────────────────────

impl App {
    fn handle_key(&mut self, key: Key) -> Task<Message> {
        match &key {
            Key::Named(Named::ArrowRight) | Key::Named(Named::PageDown) => {
                if self.app_view != AppView::Reader { return Task::none(); }
                if self.zoom_active {
                    return scroll_mod::scroll_by(self.scroll_id.clone(),
                        scroll_mod::AbsoluteOffset { x: PAN_STEP, y: 0.0 });
                }
                let step = self.page_step();
                let new = (self.current_page + step).min(self.sources.len().saturating_sub(1));
                if new != self.current_page {
                    self.current_page = new;
                    self.persist_progress();
                    return self.refresh_window();
                }
            }
            Key::Named(Named::ArrowLeft) | Key::Named(Named::PageUp) => {
                if self.app_view != AppView::Reader { return Task::none(); }
                if self.zoom_active {
                    return scroll_mod::scroll_by(self.scroll_id.clone(),
                        scroll_mod::AbsoluteOffset { x: -PAN_STEP, y: 0.0 });
                }
                let step = self.page_step();
                let new = self.current_page.saturating_sub(step);
                if new != self.current_page {
                    self.current_page = new;
                    self.persist_progress();
                    return self.refresh_window();
                }
            }
            Key::Named(Named::ArrowDown) if self.zoom_active => {
                return scroll_mod::scroll_by(self.scroll_id.clone(),
                    scroll_mod::AbsoluteOffset { x: 0.0, y: PAN_STEP });
            }
            Key::Named(Named::ArrowUp) if self.zoom_active => {
                return scroll_mod::scroll_by(self.scroll_id.clone(),
                    scroll_mod::AbsoluteOffset { x: 0.0, y: -PAN_STEP });
            }
            Key::Named(Named::Escape) => {
                if self.zoom_active { self.zoom_active = false; self.zoom = 1.0; }
                else if self.show_chapter_select { self.show_chapter_select = false; }
            }
            Key::Character(c)
                if self.app_view == AppView::Reader && self.modifiers.control() =>
            {
                match c.as_str() {
                    "+" | "=" => self.apply_zoom(ZOOM_STEP),
                    "-" | "_" => self.apply_zoom(1.0 / ZOOM_STEP),
                    "0" => { self.zoom_active = false; self.zoom = 1.0; }
                    _ => {}
                }
            }
            Key::Character(c) if self.app_view == AppView::Reader => match c.as_str() {
                "f" | "F" => {
                    self.fullscreen = !self.fullscreen;
                    let mode = if self.fullscreen { window::Mode::Fullscreen } else { window::Mode::Windowed };
                    if let Some(id) = self.core.main_window_id() {
                        return window::set_mode(id, mode);
                    }
                }
                "t" | "T" => self.theater_mode = !self.theater_mode,
                "l" | "L" => {
                    self.layout = match self.layout { Layout::Single => Layout::Dual, Layout::Dual => Layout::Single };
                    if self.layout == Layout::Dual && self.current_page % 2 != 0 {
                        self.current_page = self.current_page.saturating_sub(1);
                    }
                    return self.refresh_window();
                }
                "m" | "M" => { self.zoom_active = !self.zoom_active; self.zoom = 1.0; }
                "i" | "I" => {
                    self.show_info = !self.show_info;
                    if self.show_info { self.show_chapter_select = false; }
                }
                "c" | "C" if self.chapters.len() > 1 => {
                    self.show_chapter_select = !self.show_chapter_select;
                    if self.show_chapter_select {
                        self.show_info = false;
                        return self.queue_missing_covers();
                    }
                }
                "p" | "P" => {
                    if let Some(page) = self.page_data(self.current_page) {
                        return copy_task(page);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        Task::none()
    }
}

// ── Library view ──────────────────────────────────────────────────────────────

impl App {
    fn view_library(&self) -> Element<'_, Message> {
        let mut col = widget::Column::new().spacing(16).padding(20);

        if let Some(err) = self.error.as_deref() {
            col = col.push(error_banner(err));
        }

        // ── Continue Reading ──────────────────────────────────────────────────
        let recent: Vec<&SeriesEntry> = self.library_entries.iter()
            .filter(|e| e.last_read_page > 0)
            .take(8)
            .collect();

        if !recent.is_empty() {
            col = col.push(widget::text::title3("Continue Reading"));
            let mut row = widget::Row::new().spacing(12);
            for entry in &recent {
                row = row.push(self.series_card(entry, true));
            }
            col = col.push(
                widget::scrollable::scrollable(row).direction(
                    scroll_mod::Direction::Horizontal(scroll_mod::Scrollbar::new()),
                ),
            );
        }

        // ── All Series ────────────────────────────────────────────────────────
        if self.library_entries.is_empty() {
            col = col.push(
                widget::Column::new()
                    .spacing(12)
                    .align_x(Alignment::Center)
                    .push(icon::from_name("library-symbolic").size(64))
                    .push(widget::text::title3("Your library is empty"))
                    .push(widget::text("Open a comic or series to add it here, or drag and drop one in.").size(14))
                    .push(
                        widget::Row::new().spacing(8)
                            .push(widget::button::standard("Open File").on_press(Message::OpenFile))
                            .push(widget::button::suggested("Add Series").on_press(Message::OpenFolder)),
                    ),
            );
        } else {
            col = col.push(widget::text::title3("All Series"));
            // 5-column grid
            let mut grid_col = widget::Column::new().spacing(12);
            for chunk in self.library_entries.chunks(5) {
                let mut row = widget::Row::new().spacing(12);
                for entry in chunk {
                    row = row.push(self.series_card(entry, false));
                }
                grid_col = grid_col.push(row);
            }
            col = col.push(
                widget::scrollable::scrollable(grid_col).height(Length::Fill),
            );
        }

        widget::container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn series_card<'a>(&'a self, entry: &'a SeriesEntry, show_progress: bool) -> Element<'a, Message> {
        let cover: Element<'_, Message> = if let Some(handle) = self.library_covers.get(&entry.id) {
            widget::image(handle.clone())
                .width(Length::Fixed(THUMB_W as f32))
                .height(Length::Fixed(THUMB_H as f32))
                .content_fit(ContentFit::Cover)
                .into()
        } else {
            widget::container(
                icon::from_name("image-x-generic-symbolic").size(48),
            )
            .width(Length::Fixed(THUMB_W as f32))
            .height(Length::Fixed(THUMB_H as f32))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
        };

        let title_text = if entry.title.chars().count() > 22 {
            format!("{}…", entry.title.chars().take(20).collect::<String>())
        } else {
            entry.title.clone()
        };

        let mut card = widget::Column::new()
            .spacing(4)
            .push(cover)
            .push(widget::text(title_text).size(12));

        if show_progress && entry.total_pages > 0 {
            let fraction = entry.progress_fraction();
            card = card.push(
                cosmic::iced::widget::ProgressBar::new(0.0..=1.0, fraction),
            );
            let ch = if entry.last_read_chapter > 0 {
                format!("Ch.{}", entry.last_read_chapter + 1)
            } else {
                format!("P.{}", entry.last_read_page + 1)
            };
            card = card.push(widget::text(ch).size(11));
        }

        let path = PathBuf::from(&entry.path);
        let page = entry.last_read_page;
        let chapter = entry.last_read_chapter;

        mouse_area(card)
            .on_press(Message::OpenFromLibrary { path, page, chapter })
            .into()
    }
}

// ── Reader view ───────────────────────────────────────────────────────────────

impl App {
    fn view_reader(&self) -> Element<'_, Message> {
        // ── Page content ──────────────────────────────────────────────────────
        let viewer: Element<'_, Message> =
            if !self.sources.is_empty() && self.handle(self.current_page).is_some() {
                match (&self.layout, self.zoom_active) {
                    (_, true) => {
                        let h = self.handle(self.current_page).unwrap();
                        let page = self.page_data(self.current_page);
                        let (w, ht) = page
                            .map(|p| (p.width as f32 * self.zoom, p.height as f32 * self.zoom))
                            .unwrap_or((0.0, 0.0));
                        widget::scrollable::scrollable(
                            widget::image(h.clone())
                                .content_fit(ContentFit::Fill)
                                .width(Length::Fixed(w))
                                .height(Length::Fixed(ht)),
                        )
                        .id(self.scroll_id.clone())
                        .direction(scroll_mod::Direction::Both {
                            vertical: scroll_mod::Scrollbar::new(),
                            horizontal: scroll_mod::Scrollbar::new(),
                        })
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into()
                    }
                    (Layout::Single, false) => widget::image(
                        self.handle(self.current_page).unwrap().clone(),
                    )
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(ContentFit::Contain)
                    .into(),
                    (Layout::Dual, false) => {
                        let mut row = widget::Row::new().spacing(4).height(Length::Fill);
                        if let Some(h) = self.handle(self.current_page) {
                            row = row.push(
                                widget::image(h.clone())
                                    .width(Length::FillPortion(1))
                                    .height(Length::Fill)
                                    .content_fit(ContentFit::Contain),
                            );
                        }
                        let right = self.current_page + 1;
                        if right < self.sources.len() {
                            if let Some(h) = self.handle(right) {
                                row = row.push(
                                    widget::image(h.clone())
                                        .width(Length::FillPortion(1))
                                        .height(Length::Fill)
                                        .content_fit(ContentFit::Contain),
                                );
                            } else {
                                row = row.push(
                                    widget::container(widget::indeterminate_circular())
                                        .width(Length::FillPortion(1))
                                        .height(Length::Fill)
                                        .center_x(Length::Fill)
                                        .center_y(Length::Fill),
                                );
                            }
                        }
                        row.into()
                    }
                }
            } else if self.loading || !self.sources.is_empty() {
                widget::Column::new()
                    .spacing(12)
                    .align_x(Alignment::Center)
                    .push(widget::indeterminate_circular())
                    .push(widget::text(if self.sources.is_empty() {
                        "Loading comic…"
                    } else {
                        "Decoding page…"
                    }))
                    .into()
            } else {
                widget::Column::new()
                    .spacing(12)
                    .align_x(Alignment::Center)
                    .push(icon::from_name("image-x-generic-symbolic").size(64))
                    .push(widget::text::title3("Open a comic to start reading"))
                    .push(widget::text(
                        "← → page  ·  C chapters  ·  L layout  ·  M zoom  ·  pinch or Ctrl+scroll to zoom  ·  T theater  ·  F fullscreen  ·  I info  ·  P copy",
                    ).size(12))
                    .push(widget::text("You can also drag and drop a file or folder here.").size(12))
                    .push(widget::button::suggested("Open Comic").on_press(Message::OpenFile))
                    .into()
            };

        // ── Theater background ────────────────────────────────────────────────
        let viewer_bg: Element<'_, Message> = if self.theater_mode {
            widget::container(viewer)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(|_: &cosmic::Theme| widget::container::Style {
                    background: Some(Background::Color(Color::BLACK)),
                    ..Default::default()
                })
                .into()
        } else {
            widget::container(viewer)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        };

        // ── Right panel (chapter select or info) ──────────────────────────────
        let main_row: Element<'_, Message> = if self.show_chapter_select {
            widget::Row::new()
                .push(
                    widget::container(viewer_bg)
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .push(self.build_chapter_select())
                .into()
        } else if self.show_info {
            widget::Row::new()
                .push(
                    widget::container(viewer_bg)
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .push(self.build_info_panel())
                .into()
        } else {
            viewer_bg
        };

        let mut col = widget::Column::new().spacing(8);
        if let Some(err) = self.error.as_deref() {
            col = col.push(error_banner(err));
        }
        col.push(main_row).into()
    }

    fn build_chapter_select(&self) -> Element<'_, Message> {
        let mut col = widget::Column::new().spacing(0);

        // Header
        col = col.push(
            widget::Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .padding(12)
                .push(widget::text::title4("Chapters").width(Length::Fill))
                .push(
                    widget::button::icon(icon::from_name("window-close-symbolic"))
                        .on_press(Message::ToggleChapterSelect),
                ),
        );
        col = col.push(widget::divider::horizontal::light());

        // Chapter list
        let current_ch = self.current_chapter().map(|(i, _, _)| i);
        let mut list = widget::Column::new().spacing(2).padding(8);

        for (ch_idx, ch) in self.chapters.iter().enumerate() {
            let is_current = current_ch == Some(ch_idx);

            let cover: Element<'_, Message> = if let Some(h) = self.chapter_covers.get(&ch_idx) {
                widget::image(h.clone())
                    .width(Length::Fixed(60.0))
                    .height(Length::Fixed(85.0))
                    .content_fit(ContentFit::Cover)
                    .into()
            } else {
                widget::container(
                    widget::text("…").size(20),
                )
                .width(Length::Fixed(60.0))
                .height(Length::Fixed(85.0))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(|_: &cosmic::Theme| widget::container::Style {
                    background: Some(Background::Color(Color::from_rgba(0.5, 0.5, 0.5, 0.2))),
                    ..Default::default()
                })
                .into()
            };

            let label_size: u16 = if is_current { 14 } else { 13 };
            let ch_name = if ch.name.chars().count() > 30 {
                format!("{}…", ch.name.chars().take(28).collect::<String>())
            } else {
                ch.name.clone()
            };

            let entry_content = widget::Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(cover)
                .push(
                    widget::Column::new()
                        .spacing(2)
                        .push(widget::text(format!("Chapter {}", ch_idx + 1)).size(11))
                        .push(widget::text(ch_name).size(label_size))
                        .push(widget::text(format!("{} pages", ch.page_count)).size(11)),
                );

            let row_el: Element<'_, Message> = if is_current {
                widget::container(entry_content)
                    .padding(6)
                    .width(Length::Fill)
                    .style(|_: &cosmic::Theme| widget::container::Style {
                        background: Some(Background::Color(Color::from_rgba(0.3, 0.5, 1.0, 0.15))),
                        ..Default::default()
                    })
                    .into()
            } else {
                mouse_area(entry_content)
                    .on_press(Message::SelectChapter(ch_idx))
                    .into()
            };

            list = list.push(row_el);
        }

        col = col.push(widget::scrollable::scrollable(list).height(Length::Fill));

        widget::container(col)
            .width(Length::Fixed(300.0))
            .height(Length::Fill)
            .into()
    }

    fn build_info_panel(&self) -> Element<'_, Message> {
        let mut col = widget::Column::new().spacing(8).padding(12);

        if let Some((ch_idx, ch, pg)) = self.current_chapter() {
            let heading = if self.chapters.len() == 1 {
                ch.name.clone()
            } else {
                format!("Chapter {} — {}", ch_idx + 1, ch.name)
            };
            col = col.push(widget::text::title4(heading));
            col = col.push(widget::text(format!("Page {} of {}", pg + 1, ch.page_count)).size(13));
            col = col.push(widget::divider::horizontal::light());
        }

        // ── Debug: what we parsed out of the filename ─────────────────────────
        col = col.push(widget::text::title4("Debug: Metadata Matching"));
        if let Some(p) = &self.parsed_filename {
            col = col.push(widget::text(format!("Parsed series: {}", p.series)).size(12));
            col = col.push(
                widget::text(format!(
                    "Parsed issue: {}",
                    p.issue.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
                ))
                .size(12),
            );
            col = col.push(
                widget::text(format!(
                    "Parsed year: {}",
                    p.year.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
                ))
                .size(12),
            );
        }
        col = col.push(widget::divider::horizontal::light());

        // AniList (manga/anime only — won't match Western comics)
        col = col.push(widget::text::title4("AniList (manga/anime)"));
        if self.metadata_loading {
            col = col.push(widget::text("Fetching…").size(12))
                .push(widget::indeterminate_circular());
        } else if let Some(meta) = &self.metadata {
            col = col.push(widget::text(meta.title.clone()).size(13));
            if !meta.status.is_empty() {
                col = col.push(widget::text(format!("Status: {}", meta.status)).size(12));
            }
            if let Some(n) = meta.chapter_count {
                col = col.push(widget::text(format!("Chapters: {n}")).size(12));
            }
            if let Some(s) = meta.score {
                col = col.push(widget::text(format!("Score: {s}/100")).size(12));
            }
            if !meta.genres.is_empty() {
                col = col.push(widget::text(format!("Genres: {}", meta.genres.join(", "))).size(12));
            }
        } else {
            col = col.push(widget::text("No match.").size(12));
        }
        col = col.push(widget::divider::horizontal::light());

        // ComicVine (Western comics — Marvel/DC/etc.)
        col = col.push(widget::text::title4("ComicVine (Western comics)"));
        if self.comicvine_loading {
            col = col.push(widget::text("Fetching…").size(12))
                .push(widget::indeterminate_circular());
        } else {
            match &self.comicvine {
                Some(Ok(m)) => {
                    col = col.push(widget::text(m.name.clone()).size(13));
                    if let Some(v) = &m.volume {
                        col = col.push(widget::text(format!("Volume: {v}")).size(12));
                    }
                    if let Some(n) = &m.issue_number {
                        col = col.push(widget::text(format!("Issue #: {n}")).size(12));
                    }
                    if let Some(d) = &m.cover_date {
                        col = col.push(widget::text(format!("Cover date: {d}")).size(12));
                    }
                    if let Some(d) = &m.description {
                        let snippet: String = if d.chars().count() > 200 {
                            format!("{}…", d.chars().take(200).collect::<String>())
                        } else {
                            d.clone()
                        };
                        col = col.push(widget::text(snippet).size(12));
                    }
                    if let Some(u) = &m.site_url {
                        col = col.push(widget::text(u.clone()).size(11));
                    }
                }
                Some(Err(e)) => col = col.push(widget::text(e.clone()).size(12)),
                None => col = col.push(widget::text("No match.").size(12)),
            }
        }

        if let Some(meta) = &self.metadata {
            if !meta.description.is_empty() {
                col = col.push(widget::divider::horizontal::light());
                col = col.push(widget::text::title4("Synopsis"));
                col = col.push(
                    widget::scrollable::scrollable(widget::text(meta.description.clone()).size(13))
                        .height(Length::Fill),
                );
            }
        }

        widget::container(col)
            .width(Length::Fixed(300.0))
            .height(Length::Fill)
            .into()
    }
}

// ── Standalone helpers ────────────────────────────────────────────────────────

/// Parses a `text/uri-list` payload (as delivered by drag-and-drop) and
/// returns the first `file://` entry as a local path.
fn first_path_from_uri_list(data: &[u8]) -> Option<PathBuf> {
    let text = String::from_utf8_lossy(data);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .and_then(|line| Url::parse(line).ok())
        .and_then(|url| url.to_file_path().ok())
}

fn error_banner(msg: &str) -> Element<'_, Message> {
    widget::container(
        widget::Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(widget::text(msg).width(Length::Fill))
            .push(
                widget::button::icon(icon::from_name("window-close-symbolic"))
                    .on_press(Message::CloseError),
            ),
    )
    .padding(8)
    .width(Length::Fill)
    .into()
}
