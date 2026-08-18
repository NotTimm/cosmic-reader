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
    /// True if this entry is a series folder (potentially many chapters/
    /// issues), false if it's a single standalone file. Drives whether
    /// "Continue Reading" shows a chapter or a page number.
    pub is_series: bool,
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

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var(var)
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
        .join("cosmic-comic")
}

/// Durable user data — the library database. Never auto-cleared.
pub fn app_data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// User preferences.
pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// Derived artifacts (cover thumbnails, extracted RAR pages). Safe to
/// delete at any time — everything here is regenerated on demand.
pub fn cache_dir() -> PathBuf {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

pub fn db_path() -> PathBuf {
    app_data_dir().join("library.db")
}

/// Moves cache artifacts written by older versions (which put them in the
/// data directory) into the XDG cache directory, so upgrading doesn't
/// silently orphan a user's already-generated covers.
fn migrate_legacy_cache() {
    let legacy_root = app_data_dir();
    let cache_root = cache_dir();
    for name in ["covers", "rar-cache"] {
        let legacy = legacy_root.join(name);
        let target = cache_root.join(name);
        if legacy.is_dir() && !target.exists() {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::rename(&legacy, &target);
        }
    }
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
    cache_dir().join("covers").join(path_hash(series_path))
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
             last_opened     INTEGER DEFAULT 0,
             is_series       INTEGER DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS library_dirs (
             path TEXT UNIQUE NOT NULL
         );",
    )
    .map_err(|e| format!("init schema: {e}"))?;

    migrate_legacy_cache();
    run_migrations(&conn)?;
    Ok(conn)
}

/// Current expected schema version. Bump this and add a matching arm in
/// [`run_migrations`] whenever the schema changes.
const SCHEMA_VERSION: i64 = 1;

/// Brings an existing database up to [`SCHEMA_VERSION`] without touching
/// user data. Uses SQLite's `user_version` pragma as the version marker, so
/// upgrading the app never requires deleting the library.
fn run_migrations(conn: &Connection) -> Result<(), String> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| format!("read schema version: {e}"))?;

    if current >= SCHEMA_VERSION {
        return Ok(());
    }

    // v0 -> v1: `is_series` distinguishes series folders from single files.
    // `ALTER TABLE ... ADD COLUMN` errors if it already exists (fresh
    // databases get it from CREATE TABLE), so a failure here is expected
    // and harmless.
    if current < 1 {
        let _ = conn.execute("ALTER TABLE series ADD COLUMN is_series INTEGER DEFAULT 0", []);
    }

    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
        .map_err(|e| format!("set schema version: {e}"))?;
    Ok(())
}

// ── Library directories ──────────────────────────────────────────────────────

pub fn add_library_dir(conn: &Connection, path: &str) -> Result<(), String> {
    conn.execute("INSERT OR IGNORE INTO library_dirs (path) VALUES (?1)", params![path])
        .map_err(|e| format!("add_library_dir: {e}"))?;
    Ok(())
}

pub fn remove_library_dir(conn: &Connection, path: &str) -> Result<(), String> {
    conn.execute("DELETE FROM library_dirs WHERE path = ?1", params![path])
        .map_err(|e| format!("remove_library_dir: {e}"))?;
    Ok(())
}

pub fn list_library_dirs(conn: &Connection) -> Vec<String> {
    let sql = "SELECT path FROM library_dirs ORDER BY path";
    match conn.prepare(sql) {
        Err(_) => vec![],
        Ok(mut stmt) => stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map(|it| it.flatten().collect())
            .unwrap_or_default(),
    }
}

// ── Write operations ──────────────────────────────────────────────────────────

pub fn upsert_series(
    conn: &Connection,
    path: &str,
    title: &str,
    total_pages: usize,
    is_series: bool,
) -> Result<(), String> {
    let now = now_unix();
    conn.execute(
        "INSERT INTO series (path, title, total_pages, date_added, last_opened, is_series)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5)
         ON CONFLICT(path) DO UPDATE SET
             title       = CASE WHEN excluded.title != '' THEN excluded.title ELSE title END,
             total_pages = excluded.total_pages,
             is_series   = excluded.is_series",
        params![path, title, total_pages as i64, now, is_series as i64],
    )
    .map_err(|e| format!("upsert_series: {e}"))?;
    Ok(())
}

/// Bumps `last_opened` without touching anything else — call when a series
/// is actually opened in the reader (bulk library scans shouldn't bump it).
pub fn touch_opened(conn: &Connection, path: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE series SET last_opened = ?2 WHERE path = ?1",
        params![path, now_unix()],
    )
    .map_err(|e| format!("touch_opened: {e}"))?;
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
               last_read_page,last_read_ch,total_pages,date_added,last_opened,is_series \
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
               last_read_page,last_read_ch,total_pages,date_added,last_opened,is_series \
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
        is_series: row.get::<_, i64>(13)? != 0,
    })
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
