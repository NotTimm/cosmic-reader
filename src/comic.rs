use std::fs::File;
use std::path::{Path, PathBuf};

use compress_tools::{ArchiveContents, ArchiveIterator};

/// A single decoded page.
#[derive(Debug)]
pub struct Page {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Where to find a page's bytes — cheap to list without decoding.
#[derive(Clone, Debug)]
pub enum PageSource {
    File(PathBuf),
    Zip { archive: PathBuf, entry: String },
    Rar { archive: PathBuf, entry: String },
}

/// The page range one chapter occupies in the flat [`PageSource`] list.
#[derive(Clone, Debug)]
pub struct ChapterInfo {
    pub name: String,
    pub start: usize,
    pub page_count: usize,
}

fn is_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn is_zip_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".cbz") || lower.ends_with(".zip")
}

fn is_rar_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".cbr") || lower.ends_with(".rar")
}

fn is_archive_name(name: &str) -> bool {
    is_zip_name(name) || is_rar_name(name)
}

fn natural_key(name: &str) -> Vec<(String, u64)> {
    let mut key = Vec::new();
    let mut chars = name.chars().peekable();
    while chars.peek().is_some() {
        let digits: String =
            std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_digit())).collect();
        if !digits.is_empty() {
            key.push((String::new(), digits.parse().unwrap_or(0)));
            continue;
        }
        let rest: String =
            std::iter::from_fn(|| chars.next_if(|c| !c.is_ascii_digit())).collect();
        key.push((rest, 0));
    }
    key
}

fn decode_image(name: &str, data: &[u8]) -> Result<Page, String> {
    let img = image::load_from_memory(data)
        .map_err(|e| format!("failed to decode page {name}: {e}"))?
        .to_rgba8();
    let (width, height) = img.dimensions();
    Ok(Page { width, height, rgba: img.into_raw() })
}

// ── ZIP/CBZ ──────────────────────────────────────────────────────────────────

fn list_zip_entries(path: &Path) -> Result<Vec<String>, String> {
    let file = File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("failed to read zip {}: {e}", path.display()))?;

    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive.by_index_raw(i).ok().and_then(|entry| {
                let name = entry.name().to_owned();
                (is_image_name(&name) && !name.ends_with('/')).then_some(name)
            })
        })
        .collect();

    names.sort_by(|a, b| natural_key(a).cmp(&natural_key(b)));
    Ok(names)
}

fn decode_zip_entry(archive: &Path, entry: &str) -> Result<Page, String> {
    let file = File::open(archive)
        .map_err(|e| format!("failed to open {}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| format!("failed to read zip {}: {e}", archive.display()))?;

    let mut zf = zip
        .by_name(entry)
        .map_err(|e| format!("entry {entry} not found: {e}"))?;

    let mut data = Vec::with_capacity(zf.size() as usize);
    std::io::Read::read_to_end(&mut zf, &mut data)
        .map_err(|e| format!("failed to read entry {entry}: {e}"))?;

    decode_image(entry, &data)
}

// ── RAR/CBR ──────────────────────────────────────────────────────────────────

fn list_rar_entries(path: &Path) -> Result<Vec<String>, String> {
    let file = File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    let mut iter =
        ArchiveIterator::from_read(file).map_err(|e| format!("failed to read archive: {e}"))?;

    let mut names = Vec::new();
    while let Some(content) = iter.next_header() {
        if let ArchiveContents::StartOfEntry(name, _stat) = content {
            if is_image_name(&name) && !name.ends_with('/') {
                names.push(name);
            }
        }
    }

    names.sort_by(|a, b| natural_key(a).cmp(&natural_key(b)));
    Ok(names)
}

fn decode_rar_entry(archive: &Path, entry: &str) -> Result<Page, String> {
    let file =
        File::open(archive).map_err(|e| format!("failed to open {}: {e}", archive.display()))?;
    let iter =
        ArchiveIterator::from_read(file).map_err(|e| format!("failed to read archive: {e}"))?;

    let mut current_name = String::new();
    let mut current_data = Vec::new();
    let mut wanted = false;

    for content in iter {
        match content {
            ArchiveContents::StartOfEntry(name, _stat) => {
                wanted = name == entry;
                current_name = name;
                current_data.clear();
            }
            ArchiveContents::DataChunk(chunk) => {
                if wanted {
                    current_data.extend_from_slice(&chunk);
                }
            }
            ArchiveContents::EndOfEntry => {
                if wanted {
                    return decode_image(&current_name, &current_data);
                }
            }
            ArchiveContents::Err(e) => return Err(format!("archive error: {e}")),
        }
    }

    Err(format!("entry {entry} not found in {}", archive.display()))
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn decode_source(source: &PageSource) -> Result<Page, String> {
    match source {
        PageSource::File(path) => {
            let data = std::fs::read(path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            decode_image(&path.to_string_lossy(), &data)
        }
        PageSource::Zip { archive, entry } => decode_zip_entry(archive, entry),
        PageSource::Rar { archive, entry } => decode_rar_entry(archive, entry),
    }
}

fn list_image_dir(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read {}: {e}", dir.display()))?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            (path.is_file() && is_image_name(&name)).then_some((name, path))
        })
        .collect();

    files.sort_by(|(a, _), (b, _)| natural_key(a).cmp(&natural_key(b)));
    Ok(files.into_iter().map(|(_, path)| path).collect())
}

enum Source {
    Zip(PathBuf),
    Rar(PathBuf),
    ImageDir(PathBuf),
}

fn dir_has_direct_images(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry.path().is_file() && is_image_name(&entry.file_name().to_string_lossy())
            })
        })
        .unwrap_or(false)
}

fn find_sources(root: &Path, sources: &mut Vec<(String, Source)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_file() {
            if is_zip_name(&name) {
                sources.push((name, Source::Zip(path)));
            } else if is_rar_name(&name) {
                sources.push((name, Source::Rar(path)));
            }
        } else if path.is_dir() {
            subdirs.push(path);
        }
    }

    if dir_has_direct_images(root) {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        sources.push((name, Source::ImageDir(root.to_path_buf())));
    }

    for subdir in subdirs {
        find_sources(&subdir, sources);
    }
}

/// Strip file extension from a chapter filename to get a display name.
fn display_name(raw: &str) -> String {
    std::path::Path::new(raw)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string())
}

/// Lists pages without decoding any.  Returns pages and chapter metadata.
pub fn collect_sources(path: &Path) -> Result<(Vec<PageSource>, Vec<ChapterInfo>), String> {
    if path.is_file() {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let is_zip = is_zip_name(&name);

        let entries = if is_zip {
            list_zip_entries(path)?
        } else {
            list_rar_entries(path)?
        };

        if entries.is_empty() {
            return Err(format!("no image pages found in {}", path.display()));
        }

        let ch_name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string());

        let archive = path.to_path_buf();
        let page_count = entries.len();
        let pages: Vec<PageSource> = entries
            .into_iter()
            .map(|entry| {
                if is_zip {
                    PageSource::Zip { archive: archive.clone(), entry }
                } else {
                    PageSource::Rar { archive: archive.clone(), entry }
                }
            })
            .collect();

        let chapters = vec![ChapterInfo { name: ch_name, start: 0, page_count }];
        return Ok((pages, chapters));
    }

    if !path.is_dir() {
        return Err(format!("{} is neither a file nor a directory", path.display()));
    }

    let mut raw_chapters = Vec::new();
    find_sources(path, &mut raw_chapters);

    if raw_chapters.is_empty() {
        return Err(format!("no comic chapters or images found under {}", path.display()));
    }

    raw_chapters.sort_by(|(a, _), (b, _)| natural_key(a).cmp(&natural_key(b)));

    let mut pages = Vec::new();
    let mut chapters = Vec::new();

    for (raw_name, source) in raw_chapters {
        let start = pages.len();
        match source {
            Source::Zip(archive) => {
                let entries = list_zip_entries(&archive)?;
                let page_count = entries.len();
                pages.extend(entries.into_iter().map(|entry| PageSource::Zip {
                    archive: archive.clone(),
                    entry,
                }));
                chapters.push(ChapterInfo {
                    name: display_name(&raw_name),
                    start,
                    page_count,
                });
            }
            Source::Rar(archive) => {
                let entries = list_rar_entries(&archive)?;
                let page_count = entries.len();
                pages.extend(entries.into_iter().map(|entry| PageSource::Rar {
                    archive: archive.clone(),
                    entry,
                }));
                chapters.push(ChapterInfo {
                    name: display_name(&raw_name),
                    start,
                    page_count,
                });
            }
            Source::ImageDir(dir) => {
                let dir_pages = list_image_dir(&dir)?;
                let page_count = dir_pages.len();
                pages.extend(dir_pages.into_iter().map(PageSource::File));
                chapters.push(ChapterInfo {
                    name: display_name(&raw_name),
                    start,
                    page_count,
                });
            }
        }
    }

    Ok((pages, chapters))
}
