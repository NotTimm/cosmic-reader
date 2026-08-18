use serde::Deserialize;
use std::path::Path;

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

/// A best-effort breakdown of a comic filename into series / issue / year,
/// e.g. "Civil War 001 (2006) (Digital) (Zone-Empire).cbr" ->
/// series "Civil War", issue 1, year 2006.
#[derive(Clone, Debug)]
pub struct ParsedFilename {
    pub series: String,
    pub issue: Option<u32>,
    pub year: Option<u32>,
}

pub fn parse_filename(raw: &str) -> ParsedFilename {
    let base = Path::new(raw)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string());

    // Strip bracketed/parenthesized groups, e.g. "(2006)", "(Digital)",
    // "(Zone-Empire)" — remembering the first that looks like a year.
    let mut year = None;
    let mut without_groups = String::with_capacity(base.len());
    let mut depth = 0u32;
    let mut group = String::new();
    for c in base.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                group.clear();
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if year.is_none() {
                    let g = group.trim();
                    if g.len() == 4 && g.chars().all(|c| c.is_ascii_digit()) {
                        if let Ok(y) = g.parse::<u32>() {
                            if (1900..=2100).contains(&y) {
                                year = Some(y);
                            }
                        }
                    }
                }
            }
            _ if depth > 0 => group.push(c),
            _ => without_groups.push(c),
        }
    }

    // The trailing numeric token (if any) is the issue number.
    let mut tokens: Vec<&str> = without_groups.split_whitespace().collect();
    let mut issue = None;
    if let Some(last) = tokens.last() {
        let all_digits = !last.is_empty() && last.chars().all(|c| c.is_ascii_digit());
        if all_digits && last.len() <= 4 {
            if let Ok(n) = last.parse::<u32>() {
                issue = Some(n);
                tokens.pop();
            }
        }
    }

    let series = tokens.join(" ").trim().to_string();
    ParsedFilename { series: if series.is_empty() { base } else { series }, issue, year }
}

pub(crate) fn strip_html(s: &str) -> String {
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
