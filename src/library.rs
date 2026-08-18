use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct SeriesEntry {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub genres: String,
    pub chapter_count: Option<u32>,
    pub anilist_score: Option<u32>,
    pub last_read_page: usize,
    pub last_read_chapter: usize,
    pub total_pages: usize,
    pub date_added: i64,
    pub last_opened: i64,
}

impl SeriesEntry {
    pub fn progress_fraction(&self) -> f32 {
        if self.total_pages == 0 {
            return 0.0;
        }
        (self.last_read_page as f32 / self.total_pages as f32).clamp(0.0, 1.0)
    }
}

// ── Paths ─────────────────────────────────────────────────────────────────────

pub fn app_data_dir() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                .join(".local/share")
        })
        .join("cosmic-comic")
}

pub fn db_path() -> PathBuf {
    app_data_dir().join("library.db")
}

/// A stable hash of a path/string, used to derive cache directory names that
/// don't change across runs.
pub fn path_hash(s: &str) -> String {
    let hash: u64 = s.bytes().enumerate().fold(0u64, |acc, (i, b)| {
        acc.wrapping_add((b as u64).wrapping_mul(31u64.wrapping_pow(i as u32 & 0x1f)))
    });
    format!("{hash:016x}")
}

/// Returns the directory where chapter-cover thumbnails for a series are cached.
pub fn series_cover_dir(series_path: &str) -> PathBuf {
    app_data_dir().join("covers").join(path_hash(series_path))
}

pub fn chapter_cover_path(series_path: &str, chapter_idx: usize) -> PathBuf {
    series_cover_dir(series_path).join(format!("ch{chapter_idx:04}.png"))
}

// ── Schema ────────────────────────────────────────────────────────────────────

pub fn open_db() -> Result<Connection, String> {
    let path = db_path();
    std::fs::create_dir_all(path.parent().unwrap())
        .map_err(|e| format!("failed to create data dir: {e}"))?;
    let conn = Connection::open(&path).map_err(|e| format!("open db: {e}"))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS series (
             id              INTEGER PRIMARY KEY AUTOINCREMENT,
             path            TEXT    UNIQUE NOT NULL,
             title           TEXT    NOT NULL DEFAULT '',
             description     TEXT    DEFAULT '',
             status          TEXT    DEFAULT '',
             genres          TEXT    DEFAULT '',
             chapter_count   INTEGER,
             anilist_score   INTEGER,
             last_read_page  INTEGER DEFAULT 0,
             last_read_ch    INTEGER DEFAULT 0,
             total_pages     INTEGER DEFAULT 0,
             date_added      INTEGER DEFAULT 0,
             last_opened     INTEGER DEFAULT 0
         );",
    )
    .map_err(|e| format!("init schema: {e}"))?;
    Ok(conn)
}

// ── Write operations ──────────────────────────────────────────────────────────

pub fn upsert_series(
    conn: &Connection,
    path: &str,
    title: &str,
    total_pages: usize,
) -> Result<(), String> {
    let now = now_unix();
    conn.execute(
        "INSERT INTO series (path, title, total_pages, date_added, last_opened)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(path) DO UPDATE SET
             title       = CASE WHEN excluded.title != '' THEN excluded.title ELSE title END,
             total_pages = excluded.total_pages,
             last_opened = excluded.last_opened",
        params![path, title, total_pages as i64, now],
    )
    .map_err(|e| format!("upsert_series: {e}"))?;
    Ok(())
}

pub fn update_metadata(
    conn: &Connection,
    path: &str,
    meta: &crate::metadata::SeriesMetadata,
) -> Result<(), String> {
    conn.execute(
        "UPDATE series SET
             title         = ?2,
             description   = ?3,
             status        = ?4,
             genres        = ?5,
             chapter_count = ?6,
             anilist_score = ?7
         WHERE path = ?1",
        params![
            path,
            meta.title,
            meta.description,
            meta.status,
            meta.genres.join(", "),
            meta.chapter_count.map(|c| c as i64),
            meta.score.map(|s| s as i64),
        ],
    )
    .map_err(|e| format!("update_metadata: {e}"))?;
    Ok(())
}

pub fn save_progress(
    conn: &Connection,
    path: &str,
    page: usize,
    chapter: usize,
) -> Result<(), String> {
    conn.execute(
        "UPDATE series SET last_read_page = ?2, last_read_ch = ?3 WHERE path = ?1",
        params![path, page as i64, chapter as i64],
    )
    .map_err(|e| format!("save_progress: {e}"))?;
    Ok(())
}

// ── Read operations ───────────────────────────────────────────────────────────

pub fn get_progress(conn: &Connection, path: &str) -> Option<(usize, usize)> {
    conn.query_row(
        "SELECT last_read_page, last_read_ch FROM series WHERE path = ?1",
        params![path],
        |row| Ok((row.get::<_, i64>(0)? as usize, row.get::<_, i64>(1)? as usize)),
    )
    .ok()
}

pub fn all_series(conn: &Connection) -> Vec<SeriesEntry> {
    let sql = "SELECT id,path,title,description,status,genres,chapter_count,anilist_score,\
               last_read_page,last_read_ch,total_pages,date_added,last_opened \
               FROM series ORDER BY last_opened DESC";
    match conn.prepare(sql) {
        Err(_) => vec![],
        Ok(mut stmt) => stmt
            .query_map([], row_to_entry)
            .map(|it| it.flatten().collect())
            .unwrap_or_default(),
    }
}

pub fn search_series(conn: &Connection, query: &str) -> Vec<SeriesEntry> {
    let pattern = format!("%{}%", query.to_lowercase());
    let sql = "SELECT id,path,title,description,status,genres,chapter_count,anilist_score,\
               last_read_page,last_read_ch,total_pages,date_added,last_opened \
               FROM series WHERE lower(title) LIKE ?1 OR lower(description) LIKE ?1 \
               OR lower(genres) LIKE ?1 ORDER BY last_opened DESC";
    match conn.prepare(sql) {
        Err(_) => vec![],
        Ok(mut stmt) => stmt
            .query_map(params![pattern], row_to_entry)
            .map(|it| it.flatten().collect())
            .unwrap_or_default(),
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<SeriesEntry> {
    Ok(SeriesEntry {
        id: row.get(0)?,
        path: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        status: row.get(4)?,
        genres: row.get(5)?,
        chapter_count: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
        anilist_score: row.get::<_, Option<i64>>(7)?.map(|v| v as u32),
        last_read_page: row.get::<_, i64>(8)? as usize,
        last_read_chapter: row.get::<_, i64>(9)? as usize,
        total_pages: row.get::<_, i64>(10)? as usize,
        date_added: row.get(11)?,
        last_opened: row.get(12)?,
    })
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
