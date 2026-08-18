//! A minimal EPUB parser and renderer: enough to open a book, show its
//! chapters with heading sizes and paragraph spacing, and jump between them.
//! There's no CSS engine and no reflow/pagination — chapters render as one
//! continuously-scrollable column, which is the same approach most simple
//! EPUB readers take before investing in a real layout engine. Images render
//! after the paragraph they appeared near, not at their exact inline
//! position.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::Reader;

/// One renderable unit of chapter content.
#[derive(Debug, Clone)]
pub enum Block {
    /// Heading level 1-6 (1 = biggest) and its text.
    Heading(u8, String),
    Paragraph(String),
    /// A blockquote, rendered indented/italicized.
    Quote(String),
    ListItem(String),
    /// Raw encoded image bytes (png/jpeg/etc.), handed straight to the
    /// renderer via `widget::image::Handle::from_bytes`.
    Image(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct EpubChapter {
    pub title: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub struct EpubBook {
    pub title: String,
    pub author: Option<String>,
    /// Raw encoded cover image bytes, if the book declares one.
    pub cover: Option<Vec<u8>>,
    pub chapters: Vec<EpubChapter>,
}

pub fn is_epub_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".epub")
}

pub fn open(path: &Path) -> Result<EpubBook, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| format!("failed to read epub {}: {e}", path.display()))?;

    let opf_path = find_opf_path(&mut zip)?;
    let opf_dir = Path::new(&opf_path).parent().unwrap_or(Path::new("")).to_path_buf();
    let opf_xml = read_zip_text(&mut zip, &opf_path)?;

    let (title, author, manifest, spine, cover_id) = parse_opf(&opf_xml);

    let cover = cover_id
        .and_then(|id| manifest.get(&id))
        .map(|href| join_epub_path(&opf_dir, href))
        .and_then(|p| read_zip_bytes(&mut zip, &p).ok());

    let mut chapters = Vec::new();
    for idref in spine {
        let Some(href) = manifest.get(&idref) else { continue };
        let chapter_path = join_epub_path(&opf_dir, href);
        let Ok(xhtml) = read_zip_text(&mut zip, &chapter_path) else { continue };
        let chapter_dir = Path::new(&chapter_path).parent().unwrap_or(Path::new("")).to_path_buf();

        let (mut blocks, image_srcs) = parse_xhtml_body(&xhtml);
        for src in image_srcs {
            let path = join_epub_path(&chapter_dir, &src);
            if let Ok(bytes) = read_zip_bytes(&mut zip, &path) {
                blocks.push(Block::Image(bytes));
            }
        }

        let chapter_title = blocks
            .iter()
            .find_map(|b| match b {
                Block::Heading(_, text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("Chapter {}", chapters.len() + 1));

        chapters.push(EpubChapter { title: chapter_title, blocks });
    }

    if chapters.is_empty() {
        return Err(format!("no readable chapters found in {}", path.display()));
    }

    Ok(EpubBook {
        title: if title.is_empty() {
            path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            title
        },
        author,
        cover,
        chapters,
    })
}

// ── container.xml / OPF ──────────────────────────────────────────────────────

fn find_opf_path(zip: &mut zip::ZipArchive<std::fs::File>) -> Result<String, String> {
    let xml = read_zip_text(zip, "META-INF/container.xml")
        .map_err(|e| format!("missing container.xml: {e}"))?;

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if local_name(&e.name()) == "rootfile" => {
                for attr in e.attributes().flatten() {
                    if local_name_bytes(attr.key.as_ref()) == "full-path" {
                        return Ok(attr.unescape_value().unwrap_or_default().into_owned());
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    Err("no <rootfile> found in container.xml".to_string())
}

/// Returns (title, author, manifest id->href, spine idrefs in order, cover manifest id).
fn parse_opf(
    xml: &str,
) -> (String, Option<String>, HashMap<String, String>, Vec<String>, Option<String>) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut title = String::new();
    let mut author = None;
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut cover_id = None;
    let mut in_title = false;
    let mut in_creator = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local_name(&e.name()).as_str() {
                "title" => in_title = true,
                "creator" => in_creator = true,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                let text = t.unescape().unwrap_or_default().into_owned();
                if in_title {
                    title = text;
                } else if in_creator {
                    author = Some(text);
                }
            }
            Ok(Event::End(e)) => match local_name(&e.name()).as_str() {
                "title" => in_title = false,
                "creator" => in_creator = false,
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                let name = local_name(&e.name());
                if name == "item" {
                    let mut id = None;
                    let mut href = None;
                    let mut is_cover = false;
                    for attr in e.attributes().flatten() {
                        match local_name_bytes(attr.key.as_ref()).as_str() {
                            "id" => id = Some(attr.unescape_value().unwrap_or_default().into_owned()),
                            "href" => {
                                href = Some(attr.unescape_value().unwrap_or_default().into_owned())
                            }
                            "properties"
                                if attr.unescape_value().unwrap_or_default().contains("cover-image") =>
                            {
                                is_cover = true;
                            }
                            _ => {}
                        }
                    }
                    if let (Some(id), Some(href)) = (id, href) {
                        if is_cover {
                            cover_id = Some(id.clone());
                        }
                        manifest.insert(id, href);
                    }
                } else if name == "itemref" {
                    for attr in e.attributes().flatten() {
                        if local_name_bytes(attr.key.as_ref()) == "idref" {
                            spine.push(attr.unescape_value().unwrap_or_default().into_owned());
                        }
                    }
                } else if name == "meta" {
                    let mut is_cover_meta = false;
                    let mut content = None;
                    for attr in e.attributes().flatten() {
                        match local_name_bytes(attr.key.as_ref()).as_str() {
                            "name" if attr.unescape_value().unwrap_or_default() == "cover" => {
                                is_cover_meta = true;
                            }
                            "content" => {
                                content = Some(attr.unescape_value().unwrap_or_default().into_owned())
                            }
                            _ => {}
                        }
                    }
                    if is_cover_meta {
                        cover_id = content.or(cover_id);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    (title, author, manifest, spine, cover_id)
}

// ── XHTML chapter body ───────────────────────────────────────────────────────

/// Returns the chapter's text blocks plus any image `src`/`href` values
/// found, in document order (appended after the blocks by the caller, which
/// has zip access to resolve them).
fn parse_xhtml_body(xhtml: &str) -> (Vec<Block>, Vec<String>) {
    let mut reader = Reader::from_str(xhtml);
    reader.config_mut().trim_text(true);
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::new();

    let mut blocks = Vec::new();
    let mut image_srcs = Vec::new();
    let mut text = String::new();
    let mut current: Option<(&'static str, u8)> = None; // (kind, heading level)

    macro_rules! flush {
        () => {
            if let Some((kind, level)) = current.take() {
                let t = text.trim().to_string();
                if !t.is_empty() {
                    blocks.push(match kind {
                        "heading" => Block::Heading(level, t),
                        "quote" => Block::Quote(t),
                        "li" => Block::ListItem(t),
                        _ => Block::Paragraph(t),
                    });
                }
            }
            text.clear();
        };
    }

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = local_name(&e.name());
                match name.as_str() {
                    "h1" => { flush!(); current = Some(("heading", 1)); }
                    "h2" => { flush!(); current = Some(("heading", 2)); }
                    "h3" => { flush!(); current = Some(("heading", 3)); }
                    "h4" => { flush!(); current = Some(("heading", 4)); }
                    "h5" => { flush!(); current = Some(("heading", 5)); }
                    "h6" => { flush!(); current = Some(("heading", 6)); }
                    "p" | "div" => { flush!(); current = Some(("p", 0)); }
                    "blockquote" => { flush!(); current = Some(("quote", 0)); }
                    "li" => { flush!(); current = Some(("li", 0)); }
                    "br" => text.push('\n'),
                    "img" | "image" => {
                        for attr in e.attributes().flatten() {
                            let key = local_name_bytes(attr.key.as_ref());
                            if key == "src" || key == "href" {
                                image_srcs.push(attr.unescape_value().unwrap_or_default().into_owned());
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if current.is_some() {
                    let s = t.unescape().unwrap_or_default();
                    if !text.is_empty() && !text.ends_with(char::is_whitespace) {
                        text.push(' ');
                    }
                    text.push_str(s.trim());
                }
            }
            Ok(Event::End(e)) => {
                let name = local_name(&e.name());
                if matches!(
                    name.as_str(),
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "blockquote" | "li"
                ) {
                    flush!();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    flush!();

    (blocks, image_srcs)
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn local_name(name: &quick_xml::name::QName) -> String {
    local_name_bytes(name.as_ref())
}

fn local_name_bytes(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.rsplit(':').next().unwrap_or(&s).to_string()
}

fn join_epub_path(base: &Path, href: &str) -> String {
    let href = href.split(['#', '?']).next().unwrap_or(href);
    let joined = if href.starts_with('/') {
        PathBuf::from(href.trim_start_matches('/'))
    } else {
        base.join(href)
    };
    // Normalize `..`/`.` components (zip entry names are always forward-slash).
    let joined = joined.to_string_lossy().into_owned();
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn read_zip_text(zip: &mut zip::ZipArchive<std::fs::File>, path: &str) -> Result<String, String> {
    let bytes = read_zip_bytes(zip, path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_zip_bytes(zip: &mut zip::ZipArchive<std::fs::File>, path: &str) -> Result<Vec<u8>, String> {
    let mut entry = zip
        .by_name(path)
        .map_err(|e| format!("entry {path} not found in epub: {e}"))?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data).map_err(|e| format!("failed to read {path}: {e}"))?;
    Ok(data)
}
