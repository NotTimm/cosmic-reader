use std::path::PathBuf;
use std::sync::Arc;

use cosmic::app::{Core, Task};
use cosmic::dialog::file_chooser::{self, FileFilter};
use cosmic::iced::{
    event,
    keyboard::{self, key::Named, Key},
    Alignment, ContentFit, Length, Subscription,
};
use cosmic::widget::{self, icon};
use cosmic::{executor, Application, ApplicationExt, Element};
use url::Url;

use crate::comic::{self, Page};

pub const APP_ID: &str = "com.tsingel.CosmicComic";

#[derive(Clone, Debug)]
pub enum Message {
    OpenFile,
    PathSelected(Url),
    Loaded(PathBuf, Vec<Arc<Page>>),
    LoadFailed(String),
    Cancelled,
    NextPage,
    PrevPage,
    CloseError,
    Key(Key),
}

pub struct App {
    core: Core,
    title: String,
    pages: Vec<widget::image::Handle>,
    current_page: usize,
    loading: bool,
    error: Option<String>,
}

impl Application for App {
    type Executor = executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let mut app = App {
            core,
            title: "Comic".to_string(),
            pages: Vec::new(),
            current_page: 0,
            loading: false,
            error: None,
        };
        app.set_header_title("Cosmic Comic".into());
        let task = match app.core.main_window_id() {
            Some(id) => app.set_window_title("Cosmic Comic".into(), id),
            None => Task::none(),
        };
        (app, task)
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        let mut elements = Vec::new();

        if !self.pages.is_empty() {
            elements.push(
                widget::button::icon(icon::from_name("go-previous-symbolic"))
                    .on_press(Message::PrevPage)
                    .into(),
            );
            elements.push(
                widget::text(format!("{} / {}", self.current_page + 1, self.pages.len()))
                    .into(),
            );
            elements.push(
                widget::button::icon(icon::from_name("go-next-symbolic"))
                    .on_press(Message::NextPage)
                    .into(),
            );
        }

        elements.push(
            widget::button::standard("Open")
                .on_press(Message::OpenFile)
                .into(),
        );

        elements
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        event::listen_with(|event, status, _window_id| match event {
            event::Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => match status {
                event::Status::Ignored => Some(Message::Key(key)),
                event::Status::Captured => None,
            },
            _ => None,
        })
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::OpenFile => {
                return cosmic::task::future(async move {
                    let filter = FileFilter::new("Comic Archives")
                        .glob("*.cbz")
                        .glob("*.cbr")
                        .glob("*.zip")
                        .glob("*.rar");

                    let dialog = file_chooser::open::Dialog::new()
                        .title("Open Comic")
                        .filter(filter);

                    match dialog.open_file().await {
                        Ok(response) => Message::PathSelected(response.url().to_owned()),
                        Err(file_chooser::Error::Cancelled) => Message::Cancelled,
                        Err(why) => Message::LoadFailed(why.to_string()),
                    }
                });
            }
            Message::PathSelected(url) => {
                let path = match url.scheme() {
                    "file" => match url.to_file_path() {
                        Ok(path) => path,
                        Err(()) => {
                            self.error = Some(format!("invalid file path: {url}"));
                            return Task::none();
                        }
                    },
                    other => {
                        self.error = Some(format!("unsupported location scheme: {other}"));
                        return Task::none();
                    }
                };

                self.loading = true;
                self.error = None;

                let load_path = path.clone();
                return cosmic::task::future(async move {
                    match tokio::task::spawn_blocking(move || comic::load(&load_path)).await {
                        Ok(Ok(pages)) => Message::Loaded(
                            path,
                            pages.into_iter().map(Arc::new).collect(),
                        ),
                        Ok(Err(e)) => Message::LoadFailed(e),
                        Err(e) => Message::LoadFailed(format!("loading task panicked: {e}")),
                    }
                });
            }
            Message::Loaded(path, pages) => {
                self.loading = false;
                self.pages = pages
                    .into_iter()
                    .map(|p| match Arc::try_unwrap(p) {
                        Ok(page) => widget::image::Handle::from_rgba(page.width, page.height, page.rgba),
                        Err(p) => widget::image::Handle::from_rgba(p.width, p.height, p.rgba.clone()),
                    })
                    .collect();
                self.current_page = 0;
                self.title = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Comic".to_string());
                self.set_header_title(self.title.clone());
                let id = self.core.main_window_id();
                if let Some(id) = id {
                    return self.set_window_title(self.title.clone(), id);
                }
            }
            Message::LoadFailed(why) => {
                self.loading = false;
                self.error = Some(why);
            }
            Message::Cancelled => {
                self.loading = false;
            }
            Message::NextPage => {
                if self.current_page + 1 < self.pages.len() {
                    self.current_page += 1;
                }
            }
            Message::PrevPage => {
                self.current_page = self.current_page.saturating_sub(1);
            }
            Message::CloseError => {
                self.error = None;
            }
            Message::Key(key) => {
                if self.pages.is_empty() {
                    return Task::none();
                }
                match key {
                    Key::Named(Named::ArrowRight) | Key::Named(Named::PageDown) => {
                        if self.current_page + 1 < self.pages.len() {
                            self.current_page += 1;
                        }
                    }
                    Key::Named(Named::ArrowLeft) | Key::Named(Named::PageUp) => {
                        self.current_page = self.current_page.saturating_sub(1);
                    }
                    _ => {}
                }
            }
        }

        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let content: Element<'_, Message> = if let Some(handle) = self.pages.get(self.current_page) {
            widget::image(handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(ContentFit::Contain)
                .into()
        } else if self.loading {
            widget::text("Loading comic...").into()
        } else {
            widget::Column::new()
                .spacing(12)
                .align_x(Alignment::Center)
                .push(icon::from_name("image-x-generic-symbolic").size(64))
                .push(widget::text::title3("Open a comic to start reading"))
                .push(
                    widget::button::suggested("Open Comic")
                        .on_press(Message::OpenFile),
                )
                .into()
        };

        let mut column = widget::Column::new().spacing(8);

        if let Some(error) = self.error.as_deref() {
            column = column.push(widget::warning(error).on_close(Message::CloseError));
        }

        column
            .push(
                widget::container(content)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
            )
            .into()
    }
}
