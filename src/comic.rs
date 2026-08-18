use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use compress_tools::{ArchiveContents, ArchiveIterator};

/// Open zip archives, cached by path, so preloading nearby pages doesn't
/// reopen the file and re-parse the central directory on every single page.
type ZipCache = Mutex<HashMap<PathBuf, Arc<Mutex<zip::ZipArchive<File>>>>>;

fn zip_cache() -> &'static ZipCache {
    static CACHE: OnceLock<ZipCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn open_zip(path: &Path) -> Result<Arc<Mutex<zip::ZipArchive<File>>>, String> {
    let mut cache = zip_cache().lock().unwrap();
    if let Some(archive) = cache.get(path) {
        return Ok(archive.clone());
    }
    let file = File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    let archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("failed to read zip {}: {e}", path.display()))?;
    let archive = Arc::new(Mutex::new(archive));
    cache.insert(path.to_path_buf(), archive.clone());
    Ok(archive)
}

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
    let archive = open_zip(path)?;
    let mut archive = archive.lock().unwrap();

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

fn decode_zip_entry(archive_path: &Path, entry: &str) -> Result<Page, String> {
    let archive = open_zip(archive_path)?;
    let data = {
        let mut archive = archive.lock().unwrap();
        let mut zf = archive
            .by_name(entry)
            .map_err(|e| format!("entry {entry} not found: {e}"))?;

        let mut data = Vec::with_capacity(zf.size() as usize);
        std::io::Read::read_to_end(&mut zf, &mut data)
            .map_err(|e| format!("failed to read entry {entry}: {e}"))?;
        data
    };

    decode_image(entry, &data)
}

// ── RAR/CBR ──────────────────────────────────────────────────────────────────
//
// RAR has no central directory: reading entry N means streaming through
// entries 1..N first. Decoding pages lazily (as ZIP does) would mean
// re-streaming the archive from the start for every single page, which is
// O(n^2) in page count and was the main cause of slow CBR opens. Instead we
// extract every image in a single sequential pass up front and cache the
// result on disk, keyed by the source file's size+mtime so re-opening the
// same archive later is just a directory read.

#[derive(serde::Serialize, serde::Deserialize)]
struct RarManifest {
    size: u64,
    mtime: i64,
    /// Original in-archive names, in extraction order; file `i` on disk is
    /// named `{i:05}` alongside this manifest.
    names: Vec<String>,
}

fn rar_cache_dir(archive: &Path) -> PathBuf {
    crate::library::app_data_dir()
        .join("rar-cache")
        .join(crate::library::path_hash(&archive.to_string_lossy()))
}

fn rar_source_stamp(archive: &Path) -> Result<(u64, i64), String> {
    let meta = std::fs::metadata(archive)
        .map_err(|e| format!("failed to stat {}: {e}", archive.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok((meta.len(), mtime))
}

/// Extracts every image in `archive` to a per-archive disk cache (reusing a
/// prior extraction if the source file hasn't changed), returning
/// `(original_name, extracted_path)` pairs in natural sort order.
fn extract_rar(archive: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let cache_dir = rar_cache_dir(archive);
    let manifest_path = cache_dir.join("manifest.json");
    let (size, mtime) = rar_source_stamp(archive)?;

    if let Ok(data) = std::fs::read(&manifest_path) {
        if let Ok(manifest) = serde_json::from_slice::<RarManifest>(&data) {
            if manifest.size == size && manifest.mtime == mtime {
                let entries: Vec<(String, PathBuf)> = manifest
                    .names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (name.clone(), cache_dir.join(format!("{i:05}"))))
                    .collect();
                if entries.iter().all(|(_, p)| p.is_file()) {
                    let mut entries = entries;
                    entries.sort_by(|(a, _), (b, _)| natural_key(a).cmp(&natural_key(b)));
                    return Ok(entries);
                }
            }
        }
    }

    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("failed to create {}: {e}", cache_dir.display()))?;

    let file = File::open(archive).map_err(|e| format!("failed to open {}: {e}", archive.display()))?;
    let iter =
        ArchiveIterator::from_read(file).map_err(|e| format!("failed to read archive: {e}"))?;

    let mut names = Vec::new();
    let mut entries = Vec::new();
    let mut current_name = String::new();
    let mut current_data = Vec::new();
    let mut wanted = false;
    let mut is_comic_info = false;

    for content in iter {
        match content {
            ArchiveContents::StartOfEntry(name, _stat) => {
                wanted = is_image_name(&name) && !name.ends_with('/');
                is_comic_info = name.to_ascii_lowercase().ends_with("comicinfo.xml");
                current_name = name;
                current_data.clear();
            }
            ArchiveContents::DataChunk(chunk) => {
                if wanted || is_comic_info {
                    current_data.extend_from_slice(&chunk);
                }
            }
            ArchiveContents::EndOfEntry => {
                if wanted {
                    let idx = names.len();
                    let out_path = cache_dir.join(format!("{idx:05}"));
                    std::fs::write(&out_path, &current_data)
                        .map_err(|e| format!("failed to write {}: {e}", out_path.display()))?;
                    names.push(current_name.clone());
                    entries.push((current_name.clone(), out_path));
                } else if is_comic_info {
                    let _ = std::fs::write(cache_dir.join("ComicInfo.xml"), &current_data);
                }
            }
            ArchiveContents::Err(e) => return Err(format!("archive error: {e}")),
        }
    }

    let manifest = RarManifest { size, mtime, names };
    if let Ok(json) = serde_json::to_vec(&manifest) {
        let _ = std::fs::write(&manifest_path, json);
    }

    entries.sort_by(|(a, _), (b, _)| natural_key(a).cmp(&natural_key(b)));
    Ok(entries)
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

        let ch_name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string());

        let archive = path.to_path_buf();
        let pages: Vec<PageSource> = if is_zip {
            let entries = list_zip_entries(path)?;
            entries
                .into_iter()
                .map(|entry| PageSource::Zip { archive: archive.clone(), entry })
                .collect()
        } else {
            extract_rar(path)?
                .into_iter()
                .map(|(_, extracted)| PageSource::File(extracted))
                .collect()
        };

        if pages.is_empty() {
            return Err(format!("no image pages found in {}", path.display()));
        }

        let page_count = pages.len();
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
                let entries = extract_rar(&archive)?;
                let page_count = entries.len();
                pages.extend(
                    entries.into_iter().map(|(_, extracted)| PageSource::File(extracted)),
                );
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

/// Looks for an embedded `ComicInfo.xml` at the top level of `path` (a
/// single archive file, or a series/chapter directory), returning its raw
/// text if found. Cheap: for zip it's one cached-archive lookup, for rar
/// it's a cache-dir file read (already extracted by [`collect_sources`]),
/// for directories it's a single stat.
pub fn find_comic_info(path: &Path) -> Option<String> {
    if path.is_dir() {
        for name in ["ComicInfo.xml", "comicinfo.xml", "COMICINFO.XML"] {
            let candidate = path.join(name);
            if candidate.is_file() {
                return std::fs::read_to_string(candidate).ok();
            }
        }
        return None;
    }

    let name = path.file_name()?.to_string_lossy();
    if is_zip_name(&name) {
        let archive = open_zip(path).ok()?;
        let mut archive = archive.lock().unwrap();
        let idx = (0..archive.len()).find(|&i| {
            archive
                .by_index_raw(i)
                .map(|e| e.name().to_ascii_lowercase().ends_with("comicinfo.xml"))
                .unwrap_or(false)
        })?;
        let mut entry = archive.by_index(idx).ok()?;
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut entry, &mut buf).ok()?;
        Some(buf)
    } else if is_rar_name(&name) {
        let candidate = rar_cache_dir(path).join("ComicInfo.xml");
        candidate.is_file().then(|| std::fs::read_to_string(candidate).ok()).flatten()
    } else {
        None
    }
}
