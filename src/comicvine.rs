use serde::Deserialize;

use crate::metadata::strip_html;

/// A single issue match from ComicVine — the standard database for Western
/// (non-manga) comics, used to fill the gap AniList leaves for titles like
/// Marvel/DC events.
#[derive(Clone, Debug)]
pub struct ComicVineMatch {
    pub name: String,
    pub volume: Option<String>,
    pub issue_number: Option<String>,
    pub cover_date: Option<String>,
    pub description: Option<String>,
    pub site_url: Option<String>,
    pub cover_url: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse<T> {
    error: String,
    results: Vec<T>,
}

#[derive(Deserialize)]
struct VolumeResult {
    id: u64,
    name: Option<String>,
    start_year: Option<String>,
    publisher: Option<PublisherRef>,
}

#[derive(Deserialize)]
struct PublisherRef {
    name: Option<String>,
}

#[derive(Deserialize)]
struct IssueResult {
    name: Option<String>,
    issue_number: Option<String>,
    cover_date: Option<String>,
    description: Option<String>,
    site_detail_url: Option<String>,
    volume: Option<VolumeNameRef>,
    image: Option<ImageRef>,
}

#[derive(Deserialize)]
struct VolumeNameRef {
    name: Option<String>,
}

#[derive(Deserialize)]
struct ImageRef {
    medium_url: Option<String>,
}

/// Reads the ComicVine API key from `COMICVINE_API_KEY`, falling back to
/// `<data dir>/comicvine_api_key.txt` (handy for desktop-launched instances
/// where env vars aren't set). Get a free key at comicvine.gamespot.com/api/.
pub fn api_key() -> Option<String> {
    if let Ok(k) = std::env::var("COMICVINE_API_KEY") {
        let k = k.trim();
        if !k.is_empty() {
            return Some(k.to_string());
        }
    }
    let path = crate::library::app_data_dir().join("comicvine_api_key.txt");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("cosmic-comic/0.1 (+https://github.com/NotTimm/cosmic-reader)")
        .build()
        .map_err(|e| format!("http client error: {e}"))
}

/// Rough case/whitespace-insensitive similarity in `[0, 1]`, `1.0` being an
/// exact match. Good enough to rank ComicVine's already-relevance-sorted
/// volume search results without pulling in a fuzzy-matching crate.
fn similarity(a: &str, b: &str) -> f32 {
    let a = a.trim().to_lowercase();
    let b = b.trim().to_lowercase();
    if a == b {
        return 1.0;
    }
    if a.contains(&b) || b.contains(&a) {
        return 0.75;
    }
    let a_words: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let b_words: std::collections::HashSet<&str> = b.split_whitespace().collect();
    let overlap = a_words.intersection(&b_words).count();
    let union = a_words.union(&b_words).count().max(1);
    0.5 * (overlap as f32 / union as f32)
}

/// Finds the single issue matching `series`/`issue_number`/`year`, the way
/// Jellyfin's ComicVine plugin does it: search volumes (series) first, pick
/// the best-matching one, then look up the specific issue inside it — far
/// more reliable than a single blended free-text issue search, especially
/// for long-running or rebooted series that share a name.
pub async fn find_issue(
    series: &str,
    issue_number: Option<&str>,
    year: Option<u32>,
) -> Result<ComicVineMatch, String> {
    let Some(key) = api_key() else {
        return Err(
            "no ComicVine API key set — set COMICVINE_API_KEY or write one to \
             ~/.local/share/cosmic-comic/comicvine_api_key.txt"
                .to_string(),
        );
    };
    let client = client()?;

    let volumes: SearchResponse<VolumeResult> = client
        .get("https://comicvine.gamespot.com/api/search/")
        .query(&[
            ("api_key", key.as_str()),
            ("format", "json"),
            ("resources", "volume"),
            ("query", series),
            ("limit", "10"),
            ("field_list", "id,name,start_year,publisher"),
        ])
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse error: {e}"))?;

    if volumes.error != "OK" {
        return Err(format!("ComicVine error: {}", volumes.error));
    }

    let best_volume = volumes
        .results
        .into_iter()
        .max_by(|a, b| {
            score_volume(a, series, year)
                .total_cmp(&score_volume(b, series, year))
        })
        .ok_or_else(|| format!("no ComicVine volumes found for '{series}'"))?;

    let volume_name = best_volume.name.clone().unwrap_or_default();

    // No issue number to narrow by — best we can do is describe the volume.
    let Some(issue_number) = issue_number else {
        return Ok(ComicVineMatch {
            name: volume_name.clone(),
            volume: Some(volume_name),
            issue_number: None,
            cover_date: best_volume.start_year.clone(),
            description: None,
            site_url: None,
            cover_url: None,
        });
    };

    let filter = format!("volume:{},issue_number:{issue_number}", best_volume.id);
    let issues: SearchResponse<IssueResult> = client
        .get("https://comicvine.gamespot.com/api/issues/")
        .query(&[
            ("api_key", key.as_str()),
            ("format", "json"),
            ("filter", filter.as_str()),
            ("field_list", "name,issue_number,cover_date,description,site_detail_url,volume,image"),
        ])
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse error: {e}"))?;

    if issues.error != "OK" {
        return Err(format!("ComicVine error: {}", issues.error));
    }

    let issue = issues.results.into_iter().next().ok_or_else(|| {
        format!("found volume '{volume_name}' but no issue #{issue_number} in it")
    })?;

    Ok(ComicVineMatch {
        name: issue.name.unwrap_or_else(|| volume_name.clone()),
        volume: issue.volume.and_then(|v| v.name).or(Some(volume_name)),
        issue_number: issue.issue_number,
        cover_date: issue.cover_date,
        description: issue.description.map(|d| strip_html(&d)),
        site_url: issue.site_detail_url,
        cover_url: issue.image.and_then(|i| i.medium_url),
    })
}

fn score_volume(v: &VolumeResult, series: &str, year: Option<u32>) -> f32 {
    let mut score = v.name.as_deref().map(|n| similarity(n, series)).unwrap_or(0.0);
    if let (Some(target), Some(start)) = (year, v.start_year.as_deref().and_then(|s| s.parse::<u32>().ok())) {
        let diff = target.abs_diff(start);
        // Small bonus for a close/matching start year, tapering off over a
        // decade — publication runs often span years, so this is a nudge,
        // not a hard filter.
        score += (1.0 - (diff as f32 / 10.0).min(1.0)) * 0.2;
    }
    if let Some(publisher) = v.publisher.as_ref().and_then(|p| p.name.as_deref()) {
        // Marvel/DC events and long runs are common enough to nudge toward.
        if matches!(publisher, "Marvel" | "DC Comics") {
            score += 0.05;
        }
    }
    score
}

/// Downloads and decodes a ComicVine cover image URL to raw RGBA.
pub async fn fetch_cover(url: &str) -> Result<(u32, u32, Vec<u8>), String> {
    let client = client()?;
    let bytes = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("download error: {e}"))?;

    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("failed to decode cover: {e}"))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Ok((w, h, img.into_raw()))
}
