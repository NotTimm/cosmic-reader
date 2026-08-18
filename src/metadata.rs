use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct SeriesMetadata {
    pub title: String,
    pub description: String,
    pub status: String,
    pub genres: Vec<String>,
    pub chapter_count: Option<u32>,
    pub score: Option<u32>,
    pub url: Option<String>,
}

// ── AniList GraphQL response types ───────────────────────────────────────────

#[derive(Deserialize)]
struct AniListResponse {
    data: Option<AniListData>,
}

#[derive(Deserialize)]
struct AniListData {
    #[serde(rename = "Media")]
    media: Option<AniListMedia>,
}

#[derive(Deserialize)]
struct AniListMedia {
    title: AniListTitle,
    description: Option<String>,
    status: Option<String>,
    chapters: Option<u32>,
    genres: Option<Vec<String>>,
    #[serde(rename = "averageScore")]
    average_score: Option<u32>,
    #[serde(rename = "siteUrl")]
    site_url: Option<String>,
}

#[derive(Deserialize)]
struct AniListTitle {
    romaji: Option<String>,
    english: Option<String>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Derives a clean search title from a folder/file name by stripping trailing
/// ULID / UUID identifiers that tools like manga-tui append.
pub fn extract_title(raw: &str) -> String {
    // Strip file extension first
    let base = std::path::Path::new(raw)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string());

    let words: Vec<&str> = base.split_whitespace().collect();

    // Drop trailing words that look like ULID (26 uppercase alphanumeric) or
    // UUID (8-4-4-4-12 hex digits separated by hyphens).
    let clean: Vec<&str> = words
        .iter()
        .rev()
        .skip_while(|w| is_id_token(w))
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if clean.is_empty() { base } else { clean.join(" ") }
}

fn is_id_token(s: &str) -> bool {
    // ULID: exactly 26 uppercase alphanumeric characters
    if s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric() && !c.is_lowercase()) {
        return true;
    }
    // UUID: 8-4-4-4-12 lowercase hex with hyphens (36 chars total)
    if s.len() == 36 {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() == 5 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit())) {
            return true;
        }
    }
    false
}

/// Fetches manga series metadata from AniList (free, no API key required).
pub async fn fetch_series_metadata(search_title: &str) -> Result<SeriesMetadata, String> {
    const QUERY: &str = r#"
        query ($search: String) {
          Media(search: $search, type: MANGA) {
            title { romaji english }
            description(asHtml: false)
            status
            chapters
            genres
            averageScore
            siteUrl
          }
        }
    "#;

    let body = serde_json::json!({
        "query": QUERY,
        "variables": { "search": search_title }
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("http client error: {e}"))?;

    let resp = client
        .post("https://graphql.anilist.co")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?
        .json::<AniListResponse>()
        .await
        .map_err(|e| format!("parse error: {e}"))?;

    let media = resp
        .data
        .and_then(|d| d.media)
        .ok_or_else(|| format!("no AniList results for '{search_title}'"))?;

    let title = media
        .title
        .english
        .or(media.title.romaji)
        .unwrap_or_else(|| search_title.to_string());

    let description = media.description.map(|d| strip_html(&d)).unwrap_or_default();

    Ok(SeriesMetadata {
        title,
        description,
        status: media.status.unwrap_or_default(),
        genres: media.genres.unwrap_or_default(),
        chapter_count: media.chapters,
        score: media.average_score,
        url: media.site_url,
    })
}

fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
