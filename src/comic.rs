use std::fs::File;
use std::path::Path;

use compress_tools::{ArchiveContents, ArchiveIterator};

/// A single decoded page, ready to hand to `iced::widget::image::Handle::from_rgba`.
#[derive(Debug)]
pub struct Page {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

fn is_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Splits a filename into alternating runs of digits and non-digits so that
/// "page2.jpg" sorts before "page10.jpg".
fn natural_key(name: &str) -> Vec<(String, u64)> {
    let mut key = Vec::new();
    let mut chars = name.chars().peekable();
    while chars.peek().is_some() {
        let digits: String = std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_digit())).collect();
        if !digits.is_empty() {
            key.push((String::new(), digits.parse().unwrap_or(0)));
            continue;
        }
        let rest: String = std::iter::from_fn(|| chars.next_if(|c| !c.is_ascii_digit())).collect();
        key.push((rest, 0));
    }
    key
}

/// Reads a `.cbz`/`.cbr` (or any libarchive-supported zip/rar) file and returns
/// its image pages, decoded and sorted into reading order. Blocking; run on a
/// dedicated thread.
pub fn load(path: &Path) -> Result<Vec<Page>, String> {
    let file = File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;

    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut current_name = String::new();
    let mut current_data = Vec::new();
    let mut current_wanted = false;

    let iter =
        ArchiveIterator::from_read(file).map_err(|e| format!("failed to read archive: {e}"))?;

    for content in iter {
        match content {
            ArchiveContents::StartOfEntry(name, _stat) => {
                current_wanted = is_image_name(&name) && !name.ends_with('/');
                current_name = name;
                current_data.clear();
            }
            ArchiveContents::DataChunk(chunk) => {
                if current_wanted {
                    current_data.extend_from_slice(&chunk);
                }
            }
            ArchiveContents::EndOfEntry => {
                if current_wanted {
                    entries.push((std::mem::take(&mut current_name), std::mem::take(&mut current_data)));
                }
            }
            ArchiveContents::Err(e) => return Err(format!("archive error: {e}")),
        }
    }

    entries.sort_by(|(a, _), (b, _)| natural_key(a).cmp(&natural_key(b)));

    if entries.is_empty() {
        return Err("no image pages found in archive".into());
    }

    entries
        .into_iter()
        .map(|(name, data)| {
            let img = image::load_from_memory(&data)
                .map_err(|e| format!("failed to decode page {name}: {e}"))?
                .to_rgba8();
            let (width, height) = img.dimensions();
            Ok(Page {
                width,
                height,
                rgba: img.into_raw(),
            })
        })
        .collect()
}
