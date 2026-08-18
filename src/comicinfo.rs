//! Parses the `ComicInfo.xml` sidecar many scanned/scraped .cbz/.cbr files
//! embed (the ComicRack/ComicTagger standard). When present it's free,
//! offline, and authoritative — Jellyfin and other comic readers prefer it
//! over any online lookup, so we do too.

/// The subset of ComicInfo.xml fields we care about.
#[derive(Debug, Default, Clone)]
pub struct ComicInfo {
    pub series: Option<String>,
    pub number: Option<String>,
    pub volume: Option<String>,
    pub year: Option<i32>,
    pub summary: Option<String>,
    pub publisher: Option<String>,
    pub writer: Option<String>,
    pub genre: Option<String>,
}

impl ComicInfo {
    pub fn is_empty(&self) -> bool {
        self.series.is_none()
            && self.number.is_none()
            && self.volume.is_none()
            && self.year.is_none()
            && self.summary.is_none()
            && self.publisher.is_none()
            && self.writer.is_none()
            && self.genre.is_none()
    }
}

pub fn parse(xml: &str) -> ComicInfo {
    ComicInfo {
        series: tag(xml, "Series"),
        number: tag(xml, "Number"),
        volume: tag(xml, "Volume"),
        year: tag(xml, "Year").and_then(|s| s.parse().ok()),
        summary: tag(xml, "Summary"),
        publisher: tag(xml, "Publisher"),
        writer: tag(xml, "Writer"),
        genre: tag(xml, "Genre"),
    }
}

/// Extracts the text content of the first `<name>...</name>` element,
/// unescaping basic XML entities and unwrapping a CDATA section if present.
/// ComicInfo.xml is a flat schema (no nested nor attributed value tags), so
/// this simple approach avoids pulling in a full XML parser dependency.
fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let mut raw = xml[start..end].trim();

    if let Some(inner) = raw.strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")) {
        raw = inner.trim();
    }

    if raw.is_empty() {
        return None;
    }
    Some(unescape(raw))
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}
