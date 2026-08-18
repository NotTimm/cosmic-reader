use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use cosmic::app::{Core, Task};
use cosmic::dialog::file_chooser::{self, FileFilter};
use cosmic::iced::{
    event,
    keyboard::{self, key::Named, Key},
    mouse, touch, window, Alignment, Background, Color, ContentFit, Length, Subscription,
};
use cosmic::iced::widget::scrollable as scroll_mod;
use cosmic::iced::widget::mouse_area;
use cosmic::widget::{self, icon};
use cosmic::{executor, Application, ApplicationExt, Element};
use url::Url;

use crate::comic::{self, ChapterInfo, PageSource};
use crate::epub;
use crate::comicinfo::{self, ComicInfo};
use crate::comicvine::{self, ComicVineMatch};
use crate::library::{self, SeriesEntry};
use crate::metadata::{self, ParsedFilename, SeriesMetadata};

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.0;

pub const APP_ID: &str = "com.tsingel.CosmicComic";

const PRELOAD_RADIUS: usize = 3;
const CACHE_RADIUS: usize = 10;
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
    EpubLoaded(PathBuf, Result<epub::EpubBook, String>),
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
    ComicVineCoverReady(Result<(u32, u32, Vec<u8>), String>),
    // Chapter covers
    ChapterCoverReady { chapter_idx: usize, result: Result<(u32, u32, Vec<u8>), String> },
    // Library
    LibrarySearchChanged(String),
    LibraryCoverReady { series_id: i64, path: PathBuf },
    // Keys / pointer / touch
    Key(Key),
    ModifiersChanged(keyboard::Modifiers),
    ZoomBootstrap,
    ZoomBootstrapWheel,
    ZoomChanged(f32),
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
    show_info: bool,
    show_chapter_select: bool,
    metadata: Option<SeriesMetadata>,
    metadata_loading: bool,
    parsed_filename: Option<ParsedFilename>,
    comic_info: Option<ComicInfo>,
    comicvine: Option<Result<ComicVineMatch, String>>,
    comicvine_loading: bool,
    comicvine_cover: Option<widget::image::Handle>,
    // epub
    epub_book: Option<epub::EpubBook>,
    epub_chapter: usize,
    epub_cover: Option<widget::image::Handle>,
    // pointer / touch state
    modifiers: keyboard::Modifiers,
    touches: HashSet<touch::Finger>,
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

fn epub_open_task(path: PathBuf) -> Task<Message> {
    cosmic::task::future(async move {
        let open_path = path.clone();
        match tokio::task::spawn_blocking(move || epub::open(&open_path)).await {
            Ok(Ok(book)) => Message::EpubLoaded(path, Ok(book)),
            Ok(Err(e)) => Message::EpubLoaded(path, Err(e)),
            Err(e) => Message::EpubLoaded(path, Err(format!("epub open task panicked: {e}"))),
        }
    })
}

/// Opens `path`, routing to the epub or comic pipeline based on extension.
fn open_any(path: PathBuf) -> Task<Message> {
    let is_epub = path
        .file_name()
        .map(|n| epub::is_epub_name(&n.to_string_lossy()))
        .unwrap_or(false);
    if is_epub {
        epub_open_task(path)
    } else {
        open_task(path)
    }
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

fn comicvine_task(series: String, issue_number: Option<String>, year: Option<u32>) -> Task<Message> {
    cosmic::task::future(async move {
        Message::ComicVineLoaded(
            comicvine::find_issue(&series, issue_number.as_deref(), year).await,
        )
    })
}

fn comicvine_cover_task(url: String) -> Task<Message> {
    cosmic::task::future(async move { Message::ComicVineCoverReady(comicvine::fetch_cover(&url).await) })
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

    /// Enters zoom/pan mode (at 100%) if not already active. The actual
    /// zoom-to-cursor math (mouse wheel, trackpad pinch, touchscreen pinch,
    /// click-drag pan) all lives inside `widget::image::viewer`, which is
    /// only present in the tree once zoom is active — this just handles
    /// the very first gesture tick that switches into that widget.
    fn enter_zoom(&mut self) {
        if !self.zoom_active {
            self.zoom_active = true;
            self.zoom = 1.0;
        }
    }

    /// Tracks active touch points only to detect the two-finger-down that
    /// bootstraps zoom mode from the fit-to-window view.
    fn handle_touch(&mut self, event: touch::Event) {
        match event {
            touch::Event::FingerPressed { id, .. } => {
                self.touches.insert(id);
                if self.app_view == AppView::Reader && self.touches.len() == 2 {
                    self.enter_zoom();
                }
            }
            touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. } => {
                self.touches.remove(&id);
            }
            touch::Event::FingerMoved { .. } => {}
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
        if self.epub_book.is_some() {
            let _ =
                library::save_progress(db, &path.to_string_lossy(), 0, self.epub_chapter);
            return;
        }
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
            show_info: false,
            show_chapter_select: false,
            metadata: None,
            metadata_loading: false,
            parsed_filename: None,
            comic_info: None,
            comicvine: None,
            comicvine_loading: false,
            comicvine_cover: None,
            epub_book: None,
            epub_chapter: 0,
            epub_cover: None,
            modifiers: keyboard::Modifiers::default(),
            touches: HashSet::new(),
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
            tasks.push(open_any(path));
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
            AppView::Reader if self.epub_book.is_some() => {
                let book = self.epub_book.as_ref().unwrap();
                els.push(
                    widget::button::icon(icon::from_name("go-previous-symbolic"))
                        .on_press(Message::PrevPage)
                        .into(),
                );
                els.push(
                    widget::text(format!("Chapter {} / {}", self.epub_chapter + 1, book.chapters.len()))
                        .into(),
                );
                els.push(
                    widget::button::icon(icon::from_name("go-next-symbolic"))
                        .on_press(Message::NextPage)
                        .into(),
                );
                els.push(widget::divider::vertical::light().into());
                if book.chapters.len() > 1 {
                    let ch_label = if self.show_chapter_select { "Chapters ▾" } else { "Chapters" };
                    els.push(
                        widget::button::standard(ch_label)
                            .on_press(Message::ToggleChapterSelect)
                            .into(),
                    );
                }
                els.push(
                    widget::button::icon(icon::from_name("dialog-information-symbolic"))
                        .on_press(Message::ToggleInfo)
                        .into(),
                );
                els.push(widget::divider::vertical::light().into());

                let theater_label = if self.theater_mode { "Theater: On" } else { "Theater: Off" };
                els.push(widget::button::standard(theater_label).on_press(Message::ToggleTheater).into());
                let fs_icon = if self.fullscreen {
                    icon::from_name("view-restore-symbolic")
                } else {
                    icon::from_name("view-fullscreen-symbolic")
                };
                els.push(widget::button::icon(fs_icon).on_press(Message::ToggleFullscreen).into());
                els.push(widget::divider::vertical::light().into());
                els.push(widget::button::standard("Open").on_press(Message::OpenFile).into());
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
            // Ctrl+scroll and trackpad pinch: while zoom mode is already
            // active, `widget::image::viewer` owns and captures these
            // itself. While inactive, the first tick just bootstraps into
            // zoom mode (see `App::enter_zoom`); the viewer then takes over
            // for the rest of the gesture. Plain scroll (no Ctrl) is
            // filtered in `update` so it never bootstraps zoom.
            event::Event::Mouse(mouse::Event::WheelScrolled { .. }) => match status {
                event::Status::Ignored => Some(Message::ZoomBootstrapWheel),
                event::Status::Captured => None,
            },
            event::Event::Mouse(mouse::Event::WheelZoomed { .. }) => match status {
                event::Status::Ignored => Some(Message::ZoomBootstrap),
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
                    let filter = FileFilter::new("Comics & Books")
                        .glob("*.cbz").glob("*.cbr").glob("*.zip").glob("*.rar").glob("*.epub");
                    let dialog = file_chooser::open::Dialog::new()
                        .title("Open Comic or Book").filter(filter);
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
                return open_any(path);
            }
            Message::OpenFromLibrary { path, page, chapter: _ } => {
                self.loading = true;
                self.error = None;
                self.app_view = AppView::Reader;
                // We'll resume at the saved page after Loaded
                // Store page temporarily in current_page; reset properly in Loaded
                self.current_page = page;
                return open_any(path);
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
                self.comic_info = None;
                self.comicvine = None;
                self.comicvine_loading = false;
                self.comicvine_cover = None;
                self.epub_book = None;
                self.epub_chapter = 0;
                self.epub_cover = None;
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
                self.parsed_filename = Some(parsed.clone());

                // Embedded ComicInfo.xml is free, offline, and authoritative
                // when present — prefer it over filename guesswork for the
                // ComicVine query, the same way Jellyfin does.
                let comic_info = comic::find_comic_info(&path).map(|xml| comicinfo::parse(&xml));
                let (cv_series, cv_issue, cv_year) = match &comic_info {
                    Some(info) if info.series.is_some() => (
                        info.series.clone().unwrap(),
                        info.number.clone().or(parsed.issue.map(|n| n.to_string())),
                        info.year.map(|y| y as u32).or(parsed.year),
                    ),
                    _ => (parsed.series.clone(), parsed.issue.map(|n| n.to_string()), parsed.year),
                };
                self.comic_info = comic_info;

                self.comicvine_loading = true;

                let mut tasks = vec![
                    self.refresh_window(),
                    metadata_task(raw_name),
                    comicvine_task(cv_series, cv_issue, cv_year),
                ];
                if let Some(id) = self.core.main_window_id() {
                    tasks.push(self.set_window_title(self.title.clone(), id));
                }
                return Task::batch(tasks);
            }
            Message::EpubLoaded(path, result) => {
                self.loading = false;
                match result {
                    Ok(book) => {
                        let path_str = path.to_string_lossy().to_string();
                        self.sources.clear();
                        self.chapters.clear();
                        self.cache.clear();
                        self.pending.clear();
                        self.chapter_covers.clear();
                        self.covers_pending.clear();
                        self.zoom_active = false;
                        self.zoom = 1.0;
                        self.metadata = None;
                        self.metadata_loading = false;
                        self.comic_info = None;
                        self.comicvine = None;
                        self.comicvine_loading = false;
                        self.comicvine_cover = None;
                        self.show_chapter_select = false;
                        self.open_path = Some(path.clone());
                        self.title = book.title.clone();
                        self.set_header_title(self.title.clone());

                        self.epub_cover = book
                            .cover
                            .clone()
                            .map(widget::image::Handle::from_bytes);

                        let chapter_count = book.chapters.len();
                        self.epub_chapter = if let Some(db) = &self.db {
                            library::get_progress(db, &path_str)
                                .map(|(_, ch)| ch)
                                .unwrap_or(0)
                                .min(chapter_count.saturating_sub(1))
                        } else {
                            0
                        };
                        self.epub_book = Some(book);

                        if let Some(db) = &self.db {
                            let _ = library::upsert_series(db, &path_str, &self.title, chapter_count);
                        }

                        if let Some(id) = self.core.main_window_id() {
                            return self.set_window_title(self.title.clone(), id);
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
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
                let cover_url = result.as_ref().ok().and_then(|m| m.cover_url.clone());
                self.comicvine = Some(result);
                if let Some(url) = cover_url {
                    return comicvine_cover_task(url);
                }
            }
            Message::ComicVineCoverReady(result) => {
                if let Ok((w, h, rgba)) = result {
                    self.comicvine_cover = Some(widget::image::Handle::from_rgba(w, h, rgba));
                }
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
                if let Some(book) = &self.epub_book {
                    let new = (self.epub_chapter + 1).min(book.chapters.len().saturating_sub(1));
                    if new != self.epub_chapter {
                        self.epub_chapter = new;
                        self.persist_progress();
                    }
                    return Task::none();
                }
                let step = self.page_step();
                let new = (self.current_page + step).min(self.sources.len().saturating_sub(1));
                if new != self.current_page {
                    self.current_page = new;
                    self.persist_progress();
                    return self.refresh_window();
                }
            }
            Message::PrevPage => {
                if let Some(_book) = &self.epub_book {
                    let new = self.epub_chapter.saturating_sub(1);
                    if new != self.epub_chapter {
                        self.epub_chapter = new;
                        self.persist_progress();
                    }
                    return Task::none();
                }
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
                if self.epub_book.is_some() {
                    self.epub_chapter = ch_idx;
                    self.show_chapter_select = false;
                    self.persist_progress();
                } else if let Some(ch) = self.chapters.get(ch_idx) {
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
            Message::ZoomBootstrapWheel => {
                if self.app_view == AppView::Reader
                    && self.modifiers.control()
                    && !self.sources.is_empty()
                {
                    self.enter_zoom();
                }
            }
            Message::ZoomBootstrap => {
                if self.app_view == AppView::Reader && !self.sources.is_empty() {
                    self.enter_zoom();
                }
            }
            Message::ZoomChanged(scale) => {
                self.zoom = scale.clamp(MIN_ZOOM, MAX_ZOOM);
            }
            Message::Touch(event) => self.handle_touch(event),
            Message::FilesDropped(paths) => {
                if let Some(path) = paths.into_iter().next() {
                    self.loading = true;
                    self.error = None;
                    self.app_view = AppView::Reader;
                    return open_any(path);
                }
            }
            Message::DndDataReceived(mime, data) => {
                if mime.contains("uri-list") {
                    if let Some(path) = first_path_from_uri_list(&data) {
                        self.loading = true;
                        self.error = None;
                        self.app_view = AppView::Reader;
                        return open_any(path);
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
                return self.update(Message::NextPage);
            }
            Key::Named(Named::ArrowLeft) | Key::Named(Named::PageUp) => {
                if self.app_view != AppView::Reader { return Task::none(); }
                return self.update(Message::PrevPage);
            }
            Key::Named(Named::Escape) => {
                if self.zoom_active { self.zoom_active = false; self.zoom = 1.0; }
                else if self.show_chapter_select { self.show_chapter_select = false; }
            }
            Key::Character(c)
                if self.app_view == AppView::Reader
                    && self.modifiers.control()
                    && c.as_str() == "0" =>
            {
                self.zoom_active = false;
                self.zoom = 1.0;
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
        if self.epub_book.is_some() {
            return self.view_epub_reader();
        }

        // ── Page content ──────────────────────────────────────────────────────
        let viewer: Element<'_, Message> =
            if !self.sources.is_empty() && self.handle(self.current_page).is_some() {
                match (&self.layout, self.zoom_active) {
                    (_, true) => {
                        let h = self.handle(self.current_page).unwrap();
                        widget::image::viewer(h.clone())
                            .content_fit(ContentFit::Contain)
                            .min_scale(MIN_ZOOM)
                            .max_scale(MAX_ZOOM)
                            .on_zoom(Message::ZoomChanged)
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
                    .push(widget::text::title3("Open a comic or book to start reading"))
                    .push(widget::text(
                        "← → page  ·  C chapters  ·  L layout  ·  M zoom  ·  pinch or Ctrl+scroll to zoom  ·  T theater  ·  F fullscreen  ·  I info  ·  P copy",
                    ).size(12))
                    .push(widget::text("You can also drag and drop a file or folder here.").size(12))
                    .push(widget::button::suggested("Open Comic or Book").on_press(Message::OpenFile))
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

    fn view_epub_reader(&self) -> Element<'_, Message> {
        let book = self.epub_book.as_ref().unwrap();

        let content: Element<'_, Message> = if let Some(chapter) = book.chapters.get(self.epub_chapter)
        {
            let mut page = widget::Column::new().spacing(14).max_width(720.0).padding(24);
            for block in &chapter.blocks {
                page = page.push(render_epub_block(block));
            }
            widget::scrollable::scrollable(
                widget::container(page).width(Length::Fill).center_x(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        } else {
            widget::text("This book has no chapters.").into()
        };

        let content_bg: Element<'_, Message> = if self.theater_mode {
            widget::container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_: &cosmic::Theme| widget::container::Style {
                    background: Some(Background::Color(Color::BLACK)),
                    ..Default::default()
                })
                .into()
        } else {
            widget::container(content).width(Length::Fill).height(Length::Fill).into()
        };

        let main_row: Element<'_, Message> = if self.show_chapter_select {
            widget::Row::new()
                .push(widget::container(content_bg).width(Length::Fill).height(Length::Fill))
                .push(self.build_epub_chapter_select())
                .into()
        } else if self.show_info {
            widget::Row::new()
                .push(widget::container(content_bg).width(Length::Fill).height(Length::Fill))
                .push(self.build_epub_info_panel())
                .into()
        } else {
            content_bg
        };

        let mut col = widget::Column::new().spacing(8);
        if let Some(err) = self.error.as_deref() {
            col = col.push(error_banner(err));
        }
        col.push(main_row).into()
    }

    fn build_epub_chapter_select(&self) -> Element<'_, Message> {
        let book = self.epub_book.as_ref().unwrap();
        let mut col = widget::Column::new().spacing(0);

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

        let mut list = widget::Column::new().spacing(2).padding(8);
        for (idx, chapter) in book.chapters.iter().enumerate() {
            let is_current = idx == self.epub_chapter;
            let label = widget::text(chapter.title.clone())
                .size(if is_current { 14 } else { 13 });

            let row_el: Element<'_, Message> = if is_current {
                widget::container(label)
                    .padding(6)
                    .width(Length::Fill)
                    .style(|_: &cosmic::Theme| widget::container::Style {
                        background: Some(Background::Color(Color::from_rgba(0.3, 0.5, 1.0, 0.15))),
                        ..Default::default()
                    })
                    .into()
            } else {
                mouse_area(widget::container(label).padding(6).width(Length::Fill))
                    .on_press(Message::SelectChapter(idx))
                    .into()
            };
            list = list.push(row_el);
        }

        col = col.push(widget::scrollable::scrollable(list).height(Length::Fill));

        widget::container(col).width(Length::Fixed(300.0)).height(Length::Fill).into()
    }

    fn build_epub_info_panel(&self) -> Element<'_, Message> {
        let book = self.epub_book.as_ref().unwrap();
        let mut col = widget::Column::new().spacing(8).padding(12);

        if let Some(cover) = &self.epub_cover {
            col = col.push(
                widget::image(cover.clone())
                    .width(Length::Fixed(160.0))
                    .content_fit(ContentFit::Contain),
            );
        }

        col = col.push(widget::text::title4(book.title.clone()));
        if let Some(author) = &book.author {
            col = col.push(widget::text(author.clone()).size(13));
        }
        col = col.push(widget::divider::horizontal::light());

        if let Some(chapter) = book.chapters.get(self.epub_chapter) {
            col = col.push(
                widget::text(format!("Chapter {} of {}", self.epub_chapter + 1, book.chapters.len()))
                    .size(13),
            );
            col = col.push(widget::text(chapter.title.clone()).size(13));
        }

        widget::container(col).width(Length::Fixed(280.0)).height(Length::Fill).into()
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

        // ── Embedded ComicInfo.xml, when present (free, offline, authoritative) ─
        if let Some(info) = &self.comic_info {
            if !info.is_empty() {
                col = col.push(widget::text::title4("ComicInfo.xml"));
                if let Some(s) = &info.series {
                    col = col.push(widget::text(format!("Series: {s}")).size(12));
                }
                if let Some(n) = &info.number {
                    col = col.push(widget::text(format!("Number: {n}")).size(12));
                }
                if let Some(v) = &info.volume {
                    col = col.push(widget::text(format!("Volume: {v}")).size(12));
                }
                if let Some(y) = info.year {
                    col = col.push(widget::text(format!("Year: {y}")).size(12));
                }
                if let Some(p) = &info.publisher {
                    col = col.push(widget::text(format!("Publisher: {p}")).size(12));
                }
                if let Some(w) = &info.writer {
                    col = col.push(widget::text(format!("Writer: {w}")).size(12));
                }
                if let Some(g) = &info.genre {
                    col = col.push(widget::text(format!("Genre: {g}")).size(12));
                }
                col = col.push(widget::divider::horizontal::light());
            }
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
                    if let Some(cover) = &self.comicvine_cover {
                        col = col.push(
                            widget::image(cover.clone())
                                .width(Length::Fixed(160.0))
                                .content_fit(ContentFit::Contain),
                        );
                    }
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

/// Renders one parsed EPUB block with heading-appropriate sizing.
fn render_epub_block(block: &epub::Block) -> Element<'_, Message> {
    match block {
        epub::Block::Heading(level, text) => {
            let size: u16 = match level {
                1 => 28,
                2 => 24,
                3 => 20,
                4 => 18,
                5 => 16,
                _ => 15,
            };
            widget::text(text.clone()).size(size).into()
        }
        epub::Block::Paragraph(text) => widget::text(text.clone()).size(16).into(),
        epub::Block::Quote(text) => widget::container(widget::text(text.clone()).size(15))
            .padding([4, 16])
            .into(),
        epub::Block::ListItem(text) => widget::text(format!("•  {text}")).size(16).into(),
        epub::Block::Image(bytes) => widget::image(widget::image::Handle::from_bytes(bytes.clone()))
            .width(Length::Fill)
            .content_fit(ContentFit::Contain)
            .into(),
    }
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
