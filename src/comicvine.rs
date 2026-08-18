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
}

#[derive(Deserialize)]
struct SearchResponse {
    error: String,
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    name: Option<String>,
    issue_number: Option<String>,
    cover_date: Option<String>,
    description: Option<String>,
    site_detail_url: Option<String>,
    volume: Option<VolumeRef>,
}

#[derive(Deserialize)]
struct VolumeRef {
    name: Option<String>,
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

/// Searches ComicVine for a single issue matching `query` (e.g. "Civil War #1").
pub async fn search_issue(query: &str) -> Result<ComicVineMatch, String> {
    let Some(key) = api_key() else {
        return Err(
            "no ComicVine API key set — set COMICVINE_API_KEY or write one to \
             ~/.local/share/cosmic-comic/comicvine_api_key.txt"
                .to_string(),
        );
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("cosmic-comic/0.1 (+https://github.com/NotTimm/cosmic-reader)")
        .build()
        .map_err(|e| format!("http client error: {e}"))?;

    let resp = client
        .get("https://comicvine.gamespot.com/api/search/")
        .query(&[
            ("api_key", key.as_str()),
            ("format", "json"),
            ("resources", "issue"),
            ("query", query),
            ("limit", "1"),
            ("field_list", "name,issue_number,cover_date,description,site_detail_url,volume"),
        ])
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?
        .json::<SearchResponse>()
        .await
        .map_err(|e| format!("parse error: {e}"))?;

    if resp.error != "OK" {
        return Err(format!("ComicVine error: {}", resp.error));
    }

    let first = resp
        .results
        .into_iter()
        .next()
        .ok_or_else(|| format!("no ComicVine results for '{query}'"))?;

    Ok(ComicVineMatch {
        name: first.name.unwrap_or_default(),
        volume: first.volume.and_then(|v| v.name),
        issue_number: first.issue_number,
        cover_date: first.cover_date,
        description: first.description.map(|d| strip_html(&d)),
        site_url: first.site_detail_url,
    })
}
