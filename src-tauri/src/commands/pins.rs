use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Monitor, State, WebviewWindow};

pub const CONTROL_LABEL: &str = "control";
/// Window pool for "show all" — every simultaneously-shown image needs its own
/// window. Must stay in sync with `tauri.conf.json` + `capabilities/default.json`.
pub const PIN_LABELS: [&str; 12] = [
    "pin-0", "pin-1", "pin-2", "pin-3", "pin-4", "pin-5", "pin-6", "pin-7", "pin-8", "pin-9",
    "pin-10", "pin-11",
];
/// Images a single session can hold. Single mode carousels through all of them
/// in one window; "show all" can only display `pool_size()` (= PIN_LABELS.len())
/// at once — the rest are reachable via Single mode.
const MAX_IMAGES: usize = 50;
const CONTROL_WIDTH: f64 = 232.0;
// Largest fraction of the monitor a freshly-pinned image is allowed to take.
const FIT_FRACTION: f64 = 0.85;

/// How many images "show all" can display at once (one window each).
fn pool_size() -> usize {
    PIN_LABELS.len()
}

// --- state -------------------------------------------------------------------

/// Source pixel data + the logical (point) size the image was fitted to (the
/// "100%" zoom baseline). Shared verbatim with the frontend.
#[derive(Clone, serde::Serialize)]
pub struct PinImagePayload {
    #[serde(rename = "dataUrl")]
    data_url: String,
    width: u32,
    height: u32,
    #[serde(rename = "fitW")]
    fit_w: f64,
    #[serde(rename = "fitH")]
    fit_h: f64,
}

/// One image in the deck. `pos` is the window's physical top-left, remembered so
/// "show all" restores each image where the user dragged it.
struct DeckImage {
    id: u64,
    image: PinImagePayload,
    pos: Option<(i32, i32)>,
    scale: f64,
    opacity: f64,
    collapsed: bool,
    click_through: bool,
    /// Starred for the cross-session Favorites view.
    favorite: bool,
    /// Free-text note attached to this image (persisted, per-image).
    note: String,
    /// Color tag (a hex string from the preset palette, or "" for none).
    color: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    All,
    Single,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::All => "all",
            Mode::Single => "single",
        }
    }
}

struct Deck {
    images: Vec<DeckImage>,
    mode: Mode,
    current: usize,
    /// In single mode the one viewer window stays put across cycles — its
    /// position is tracked here, separate from any image's all-mode position.
    single_pos: Option<(i32, i32)>,
    /// Single-mode viewer rectangle size (logical). Fixed across navigation so
    /// differently-sized images all show inside the same stable frame. Computed
    /// (~60% of the monitor) the first time single mode is shown.
    single_size: Option<(f64, f64)>,
    /// window label -> image id currently shown there (rebuilt every render).
    assign: HashMap<String, u64>,
    /// The session currently loaded into this deck (SQLite `sessions.id`).
    active_session: i64,
    /// True when `active_session` is the special Favorites session, so the deck
    /// holds the cross-session aggregation of favorited images (changes how
    /// close / favorite behave: they remove-from-favorites, not destroy).
    favorites_view: bool,
    /// True while a web text field (e.g. a note) is focused. Tells the native
    /// key monitor to pass ← / → / ESC through to the DOM instead of hijacking
    /// them for carousel nav, so the note can be edited normally.
    text_editing: bool,
    /// Whether the pins have been shown this run. On launch we load the active
    /// session into memory but keep pins hidden ("launch quiet") until the user
    /// reveals them or pastes; switching sessions reveals immediately.
    revealed: bool,
}

impl Default for Deck {
    fn default() -> Self {
        Deck {
            images: Vec::new(),
            mode: Mode::All,
            current: 0,
            single_pos: None,
            single_size: None,
            assign: HashMap::new(),
            active_session: 0,
            favorites_view: false,
            text_editing: false,
            revealed: false,
        }
    }
}

pub struct PinStore(Mutex<Deck>);

impl Default for PinStore {
    fn default() -> Self {
        PinStore(Mutex::new(Deck::default()))
    }
}

/// Full render payload for a single pin window.
#[derive(Clone, serde::Serialize)]
pub struct PinView {
    id: u64,
    #[serde(rename = "dataUrl")]
    data_url: String,
    width: u32,
    height: u32,
    #[serde(rename = "fitW")]
    fit_w: f64,
    #[serde(rename = "fitH")]
    fit_h: f64,
    scale: f64,
    opacity: f64,
    collapsed: bool,
    mode: String,
    index: usize,
    total: usize,
    #[serde(rename = "clickThrough")]
    click_through: bool,
    favorite: bool,
    /// This window belongs to the cross-session Favorites view.
    #[serde(rename = "favoritesView")]
    favorites_view: bool,
    note: String,
    color: String,
}

#[derive(Clone, serde::Serialize)]
struct DeckSummary {
    count: usize,
    mode: String,
    current: usize,
    #[serde(rename = "anyClickThrough")]
    any_click_through: bool,
    #[serde(rename = "sessionId")]
    session_id: i64,
    /// false right after launch (pins loaded but hidden) until revealed.
    revealed: bool,
    /// How many images "show all" can display at once (window-pool size).
    #[serde(rename = "poolSize")]
    pool_size: usize,
    /// The active session is the cross-session Favorites view.
    #[serde(rename = "favoritesView")]
    favorites_view: bool,
}

/// One row for the session switcher in the control panel.
#[derive(Clone, serde::Serialize)]
pub struct SessionInfo {
    id: i64,
    name: String,
    count: usize,
    active: bool,
    #[serde(rename = "lastUsed")]
    last_used: i64,
    starred: bool,
    /// The special always-present session that aggregates favorited images.
    #[serde(rename = "isFavorites")]
    is_favorites: bool,
}

/// SQLite handle (managed state). Opened once in setup; see [`init_store`].
pub struct Db(pub Mutex<Connection>);

fn make_view(img: &DeckImage, index: usize, total: usize, mode: Mode, favorites_view: bool) -> PinView {
    PinView {
        id: img.id,
        data_url: img.image.data_url.clone(),
        width: img.image.width,
        height: img.image.height,
        fit_w: img.image.fit_w,
        fit_h: img.image.fit_h,
        scale: img.scale,
        opacity: img.opacity,
        collapsed: img.collapsed,
        mode: mode.as_str().to_string(),
        index: index + 1,
        total,
        click_through: img.click_through,
        favorite: img.favorite,
        favorites_view,
        note: img.note.clone(),
        color: img.color.clone(),
    }
}

fn find_index(deck: &Deck, id: u64) -> Option<usize> {
    deck.images.iter().position(|i| i.id == id)
}

// --- SQLite session persistence ---------------------------------------------
//
// One DB at `<app-data>/pinshot.sqlite3`. A `sessions` row owns many `images`
// rows; `app_state` holds the active session id. The deck image `id` IS the
// `images.id` rowid, so the frontend id and the DB row stay 1:1 — high-frequency
// drags/zooms become a single targeted UPDATE, not a full rewrite.

const SCHEMA: &str = "
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS sessions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used INTEGER,
  starred INTEGER NOT NULL DEFAULT 0,
  is_favorites INTEGER NOT NULL DEFAULT 0,
  mode TEXT NOT NULL DEFAULT 'all',
  current_idx INTEGER NOT NULL DEFAULT 0,
  single_pos_x INTEGER,
  single_pos_y INTEGER
);
CREATE TABLE IF NOT EXISTS images (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  data_url TEXT NOT NULL,
  width INTEGER NOT NULL,
  height INTEGER NOT NULL,
  fit_w REAL NOT NULL,
  fit_h REAL NOT NULL,
  pos_x INTEGER,
  pos_y INTEGER,
  scale REAL NOT NULL,
  opacity REAL NOT NULL,
  collapsed INTEGER NOT NULL,
  click_through INTEGER NOT NULL,
  favorite INTEGER NOT NULL DEFAULT 0,
  note TEXT NOT NULL DEFAULT '',
  color TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS app_state (k TEXT PRIMARY KEY, v TEXT NOT NULL);
";

fn now_ts() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Open (creating if needed) the session DB and ensure the schema exists.
pub fn open_db(app: &AppHandle) -> Connection {
    let dir = app
        .path()
        .app_data_dir()
        .expect("resolve app data dir");
    let _ = std::fs::create_dir_all(&dir);
    let conn = Connection::open(dir.join("pinshot.sqlite3")).expect("open pinshot.sqlite3");
    conn.execute_batch(SCHEMA).expect("init schema");
    // Migration for DBs created before `last_used` existed (errors if the column
    // is already there — ignored). Seed it from created_at so ordering works.
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN last_used INTEGER", []);
    let _ = conn.execute(
        "UPDATE sessions SET last_used = created_at WHERE last_used IS NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE sessions ADD COLUMN starred INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Cross-session favorites: a per-image flag + a special "Favorites" session
    // that aggregates every favorited image. Both migrated for pre-existing DBs
    // (errors when the column already exists — ignored).
    let _ = conn.execute(
        "ALTER TABLE images ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE sessions ADD COLUMN is_favorites INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Per-image text note + color tag (migrated for pre-existing DBs).
    let _ = conn.execute(
        "ALTER TABLE images ADD COLUMN note TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE images ADD COLUMN color TEXT NOT NULL DEFAULT ''",
        [],
    );
    conn
}

/// Star / unstar a session (starred ones are pinned to the mini-bar quick list).
fn db_set_starred(conn: &Connection, id: i64, starred: bool) {
    let _ = conn.execute(
        "UPDATE sessions SET starred=?1 WHERE id=?2",
        params![starred as i64, id],
    );
}

fn db_create_session(conn: &Connection, name: &str) -> i64 {
    let now = now_ts();
    let _ = conn.execute(
        "INSERT INTO sessions (name, created_at, last_used) VALUES (?1, ?2, ?2)",
        params![name, now],
    );
    conn.last_insert_rowid()
}

/// Is `id` the special Favorites session (the one that aggregates every
/// favorited image, rather than owning its own session-scoped set)?
fn is_favorites_session(conn: &Connection, id: i64) -> bool {
    conn.query_row(
        "SELECT is_favorites FROM sessions WHERE id=?1",
        params![id],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v != 0)
    .unwrap_or(false)
}

/// Return the Favorites session id, creating it ("★ Favorites") the first time.
/// There is always exactly one; it can't be deleted.
fn db_favorites_or_init(conn: &Connection) -> i64 {
    if let Ok(id) = conn.query_row(
        "SELECT id FROM sessions WHERE is_favorites=1 ORDER BY id ASC LIMIT 1",
        [],
        |r| r.get::<_, i64>(0),
    ) {
        return id;
    }
    let now = now_ts();
    let _ = conn.execute(
        "INSERT INTO sessions (name, created_at, last_used, is_favorites) VALUES ('★ Favorites', ?1, ?1, 1)",
        params![now],
    );
    conn.last_insert_rowid()
}

/// Total favorited images across ALL sessions (the Favorites view's contents).
fn db_favorites_count(conn: &Connection) -> usize {
    conn.query_row("SELECT COUNT(*) FROM images WHERE favorite=1", [], |r| {
        r.get::<_, i64>(0)
    })
    .map(|v| v as usize)
    .unwrap_or(0)
}

fn db_set_image_favorite(conn: &Connection, id: u64, favorite: bool) {
    let _ = conn.execute(
        "UPDATE images SET favorite=?1 WHERE id=?2",
        params![favorite as i64, id as i64],
    );
}

/// Bump a session's recency (used by paste / switch) for the mini-bar list.
fn db_touch_session(conn: &Connection, id: i64) {
    let _ = conn.execute(
        "UPDATE sessions SET last_used=?1 WHERE id=?2",
        params![now_ts(), id],
    );
}

fn db_set_active(conn: &Connection, id: i64) {
    let _ = conn.execute(
        "INSERT INTO app_state (k, v) VALUES ('active_session', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![id.to_string()],
    );
}

/// Return a guaranteed-valid session id to write into: the candidate if it
/// still exists, otherwise heal via [`db_active_or_init`]. Prevents pasting an
/// image against a stale/deleted session id (which would fail or orphan it).
fn ensure_active_session(conn: &Connection, candidate: i64) -> i64 {
    if candidate > 0
        && conn
            .query_row("SELECT 1 FROM sessions WHERE id=?1", params![candidate], |_| Ok(()))
            .is_ok()
    {
        return candidate;
    }
    db_active_or_init(conn)
}

/// Return the active session id, healing a missing/stale pointer: prefer the
/// stored one, else the newest session, else create a fresh "Session 1".
fn db_active_or_init(conn: &Connection) -> i64 {
    let stored: Option<i64> = conn
        .query_row(
            "SELECT v FROM app_state WHERE k = 'active_session'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| s.parse().ok());
    if let Some(id) = stored {
        let exists = conn
            .query_row("SELECT 1 FROM sessions WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .is_ok();
        if exists {
            return id;
        }
    }
    // Fall back to the newest NORMAL session — never auto-land in Favorites
    // (it's a view; the user opens it deliberately).
    if let Ok(id) = conn.query_row(
        "SELECT id FROM sessions WHERE is_favorites=0 ORDER BY id DESC LIMIT 1",
        [],
        |r| r.get::<_, i64>(0),
    ) {
        db_set_active(conn, id);
        return id;
    }
    let id = db_create_session(conn, "Session 1");
    db_set_active(conn, id);
    id
}

fn db_insert_image(
    conn: &Connection,
    session_id: i64,
    img: &PinImagePayload,
    pos: Option<(i32, i32)>,
    scale: f64,
    opacity: f64,
    collapsed: bool,
    click_through: bool,
    favorite: bool,
) -> rusqlite::Result<i64> {
    let (px, py) = match pos {
        Some((x, y)) => (Some(x), Some(y)),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO images
           (session_id, data_url, width, height, fit_w, fit_h, pos_x, pos_y, scale, opacity, collapsed, click_through, favorite)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            session_id, img.data_url, img.width, img.height, img.fit_w, img.fit_h,
            px, py, scale, opacity, collapsed as i64, click_through as i64, favorite as i64
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn db_update_image_data(conn: &Connection, id: u64, img: &PinImagePayload, scale: f64, collapsed: bool) {
    let _ = conn.execute(
        "UPDATE images SET data_url=?1, width=?2, height=?3, fit_w=?4, fit_h=?5, scale=?6, collapsed=?7 WHERE id=?8",
        params![img.data_url, img.width, img.height, img.fit_w, img.fit_h, scale, collapsed as i64, id as i64],
    );
}

fn db_update_image_pos(conn: &Connection, id: u64, x: i32, y: i32) {
    let _ = conn.execute(
        "UPDATE images SET pos_x=?1, pos_y=?2 WHERE id=?3",
        params![x, y, id as i64],
    );
}

fn db_update_image_scale(conn: &Connection, id: u64, scale: f64) {
    let _ = conn.execute("UPDATE images SET scale=?1 WHERE id=?2", params![scale, id as i64]);
}

fn db_update_image_opacity(conn: &Connection, id: u64, opacity: f64) {
    let _ = conn.execute("UPDATE images SET opacity=?1 WHERE id=?2", params![opacity, id as i64]);
}

fn db_update_image_collapsed(conn: &Connection, id: u64, collapsed: bool) {
    let _ = conn.execute("UPDATE images SET collapsed=?1 WHERE id=?2", params![collapsed as i64, id as i64]);
}

fn db_update_image_click_through(conn: &Connection, id: u64, ct: bool) {
    let _ = conn.execute("UPDATE images SET click_through=?1 WHERE id=?2", params![ct as i64, id as i64]);
}

/// Returns the rusqlite result so callers can confirm the write actually landed
/// (the note-save toast must only show "saved" on a real success).
fn db_update_image_note(conn: &Connection, id: u64, note: &str) -> rusqlite::Result<usize> {
    conn.execute("UPDATE images SET note=?1 WHERE id=?2", params![note, id as i64])
}

fn db_update_image_color(conn: &Connection, id: u64, color: &str) {
    let _ = conn.execute("UPDATE images SET color=?1 WHERE id=?2", params![color, id as i64]);
}

fn db_delete_image(conn: &Connection, id: u64) {
    let _ = conn.execute("DELETE FROM images WHERE id=?1", params![id as i64]);
}

fn db_delete_session_images(conn: &Connection, session_id: i64) {
    let _ = conn.execute("DELETE FROM images WHERE session_id=?1", params![session_id]);
}

fn db_set_session_meta(conn: &Connection, session_id: i64, mode: Mode, current: usize, single_pos: Option<(i32, i32)>) {
    let (sx, sy) = match single_pos {
        Some((x, y)) => (Some(x), Some(y)),
        None => (None, None),
    };
    let _ = conn.execute(
        "UPDATE sessions SET mode=?1, current_idx=?2, single_pos_x=?3, single_pos_y=?4 WHERE id=?5",
        params![mode.as_str(), current as i64, sx, sy, session_id],
    );
}

/// Map one `images` row (in the fixed column order below) into a `DeckImage`.
fn row_to_deck_image(r: &rusqlite::Row) -> rusqlite::Result<DeckImage> {
    let px: Option<i32> = r.get(6)?;
    let py: Option<i32> = r.get(7)?;
    Ok(DeckImage {
        id: r.get::<_, i64>(0)? as u64,
        image: PinImagePayload {
            data_url: r.get(1)?,
            width: r.get(2)?,
            height: r.get(3)?,
            fit_w: r.get(4)?,
            fit_h: r.get(5)?,
        },
        pos: match (px, py) {
            (Some(x), Some(y)) => Some((x, y)),
            _ => None,
        },
        scale: r.get(8)?,
        opacity: r.get(9)?,
        collapsed: r.get::<_, i64>(10)? != 0,
        click_through: r.get::<_, i64>(11)? != 0,
        favorite: r.get::<_, i64>(12)? != 0,
        note: r.get(13)?,
        color: r.get(14)?,
    })
}

/// Read every image + meta for one session, ready to drop into the deck. The
/// Favorites session is special: instead of its own session-scoped images it
/// aggregates every favorited image across ALL sessions.
fn db_load_session(conn: &Connection, session_id: i64) -> (Vec<DeckImage>, Mode, usize, Option<(i32, i32)>) {
    const COLS: &str =
        "id, data_url, width, height, fit_w, fit_h, pos_x, pos_y, scale, opacity, collapsed, click_through, favorite, note, color";
    let mut images = Vec::new();
    if is_favorites_session(conn, session_id) {
        let sql = format!("SELECT {COLS} FROM images WHERE favorite=1 ORDER BY id ASC");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([], row_to_deck_image) {
                images.extend(rows.flatten());
            }
        }
    } else {
        let sql = format!("SELECT {COLS} FROM images WHERE session_id=?1 ORDER BY id ASC");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map(params![session_id], row_to_deck_image) {
                images.extend(rows.flatten());
            }
        }
    }
    let (mode_s, current, spx, spy): (String, i64, Option<i32>, Option<i32>) = conn
        .query_row(
            "SELECT mode, current_idx, single_pos_x, single_pos_y FROM sessions WHERE id=?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap_or_else(|_| ("all".to_string(), 0, None, None));
    let mode = if mode_s == "single" { Mode::Single } else { Mode::All };
    let single_pos = match (spx, spy) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    };
    let current = if images.is_empty() {
        0
    } else {
        (current as usize).min(images.len() - 1)
    };
    (images, mode, current, single_pos)
}

fn db_list_sessions(conn: &Connection, active: i64) -> Vec<SessionInfo> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT s.id, s.name, COUNT(i.id), COALESCE(s.last_used, s.created_at, 0), COALESCE(s.starred, 0), COALESCE(s.is_favorites, 0)
         FROM sessions s LEFT JOIN images i ON i.session_id = s.id
         GROUP BY s.id ORDER BY s.id ASC",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            let id: i64 = r.get(0)?;
            Ok(SessionInfo {
                id,
                name: r.get(1)?,
                count: r.get::<_, i64>(2)? as usize,
                active: id == active,
                last_used: r.get(3)?,
                starred: r.get::<_, i64>(4)? != 0,
                is_favorites: r.get::<_, i64>(5)? != 0,
            })
        }) {
            out.extend(rows.flatten());
        }
    }
    // The Favorites row's real contents are all favorited images across every
    // session, not its own session-scoped rows — report that count instead.
    let fav_count = db_favorites_count(conn);
    for s in out.iter_mut() {
        if s.is_favorites {
            s.count = fav_count;
        }
    }
    out
}

/// Convenience: lock the managed DB and run a closure with the connection.
fn with_db<T>(app: &AppHandle, f: impl FnOnce(&Connection) -> T) -> T {
    let db = app.state::<Db>();
    let conn = db.0.lock().unwrap();
    f(&conn)
}

/// Show the deck if pins are currently revealed; otherwise just refresh the
/// control-panel summary and leave everything hidden. This keeps a hidden state
/// hidden when you paste / change mode / cycle (the image is still stored).
fn render_or_summary(app: &AppHandle, deck: &mut Deck) {
    if deck.revealed {
        render(app, deck);
    } else {
        emit_summary(app, deck);
    }
}

/// Emit the current session list to the control panel (after any session op).
fn emit_sessions(app: &AppHandle, deck: &Deck) {
    let list = with_db(app, |c| db_list_sessions(c, deck.active_session));
    let _ = app.emit("sessions-changed", list);
}

/// Open the DB, pick/heal the active session, load it into the deck WITHOUT
/// showing the pins ("launch quiet"), then manage the connection. Called once
/// from setup.
pub fn init_store(app: &AppHandle) {
    let conn = open_db(app);
    // The cross-session Favorites view is always present (created once).
    let _ = db_favorites_or_init(&conn);
    let active = db_active_or_init(&conn);
    let favorites_view = is_favorites_session(&conn, active);
    let (images, mode, current, single_pos) = db_load_session(&conn, active);
    // Forensic breadcrumb: how much data exists at launch (catches any loss).
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))
        .unwrap_or(-1);
    log::info!(
        "init_store: active_session={active}, loaded {} images for it, {total} images total in DB",
        images.len()
    );
    {
        let store = app.state::<PinStore>();
        let mut deck = store.0.lock().unwrap();
        deck.images = images;
        deck.mode = mode;
        deck.current = current;
        deck.single_pos = single_pos;
        deck.active_session = active;
        deck.favorites_view = favorites_view;
        deck.revealed = false;
    }
    app.manage(Db(Mutex::new(conn)));
}

// --- clipboard -> PNG data URL ----------------------------------------------

/// Read the current clipboard image and return (data URL, width, height).
/// Errors (with a user-facing message) when the clipboard holds no image.
fn read_clipboard_png(app: &AppHandle) -> Result<(String, u32, u32), String> {
    use base64::Engine;
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let img = app
        .clipboard()
        .read_image()
        .map_err(|_| "No image found on the clipboard. Take a screenshot to the clipboard (⌃⇧⌘4 on macOS), then paste.".to_string())?;

    let (w, h) = (img.width(), img.height());
    let rgba = img.rgba().to_vec();

    let buf: image::RgbaImage = image::ImageBuffer::from_raw(w, h, rgba)
        .ok_or_else(|| "Clipboard image had an unexpected size.".to_string())?;

    let mut png: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("Could not encode the image: {e}"))?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    Ok((format!("data:image/png;base64,{b64}"), w, h))
}

// --- monitors / sizing -------------------------------------------------------

fn monitor_at(app: &AppHandle, x: f64, y: f64) -> Option<Monitor> {
    app.available_monitors().ok()?.into_iter().find(|m| {
        let p = m.position();
        let s = m.size();
        x >= p.x as f64
            && x < (p.x + s.width as i32) as f64
            && y >= p.y as f64
            && y < (p.y + s.height as i32) as f64
    })
}

fn cursor_monitor(app: &AppHandle) -> Option<Monitor> {
    if let Ok(c) = app.cursor_position() {
        if let Some(m) = monitor_at(app, c.x, c.y) {
            return Some(m);
        }
    }
    app.available_monitors().ok().and_then(|v| v.into_iter().next())
}

/// Logical size that fits an image (source px) within FIT_FRACTION of the
/// monitor, never upscaling past 1:1 logical pixels.
fn fit_logical(monitor: &Monitor, img_w: u32, img_h: u32) -> (f64, f64) {
    let scale = monitor.scale_factor();
    let mon = monitor.size().to_logical::<f64>(scale);
    let max_w = mon.width * FIT_FRACTION;
    let max_h = mon.height * FIT_FRACTION;
    let img_lw = (img_w as f64 / scale).max(1.0);
    let img_lh = (img_h as f64 / scale).max(1.0);
    let fit = (max_w / img_lw).min(max_h / img_lh).min(1.0);
    ((img_lw * fit).max(80.0), (img_lh * fit).max(60.0))
}

/// Default physical top-left for a freshly shown image, cascaded so stacked
/// images don't perfectly overlap.
fn default_pos(app: &AppHandle, order: usize) -> (i32, i32) {
    if let Some(m) = cursor_monitor(app) {
        let p = m.position();
        let off = 36 * order as i32;
        (p.x + 80 + off, p.y + 80 + off)
    } else {
        (120 + 36 * order as i32, 120 + 36 * order as i32)
    }
}

/// Resize a window keeping either its top-left pinned or its center fixed.
/// macOS anchors resizes to the bottom-left, so capture geometry and restore.
fn resize_keep_anchor(window: &WebviewWindow, width: f64, height: f64, center: bool) {
    let old_pos = window.outer_position().ok();
    let old_size = window.outer_size().ok();
    let scale = window.scale_factor().unwrap_or(1.0);
    if window
        .set_size(tauri::LogicalSize::new(width, height))
        .is_err()
    {
        return;
    }
    if center {
        if let (Some(p), Some(s)) = (old_pos, old_size) {
            let new_w = (width * scale).round() as i32;
            let new_h = (height * scale).round() as i32;
            let cx = p.x + s.width as i32 / 2;
            let cy = p.y + s.height as i32 / 2;
            let _ = window.set_position(tauri::PhysicalPosition::new(cx - new_w / 2, cy - new_h / 2));
        }
    } else if let Some(p) = old_pos {
        let _ = window.set_position(p);
    }
}

// --- panel-aware show / hide (macOS NSPanel; plain window elsewhere) ---------

/// Convert a (hidden, config-created) floating window into a non-activating
/// NSPanel so it floats over OTHER apps' fullscreen Spaces. Call once per window,
/// from setup (main thread). Recipe: tauri-nspanel's `fullscreen` example (see
/// ~/.claude/notes/tauri-macos-floating-widget-over-fullscreen.md).
#[cfg(target_os = "macos")]
#[allow(deprecated)] // cocoa re-exports; same API the plugin's own example uses
pub fn convert_to_panel(app: &AppHandle, label: &str) {
    use tauri_nspanel::{cocoa::appkit::NSWindowCollectionBehavior, WebviewWindowExt};

    let Some(window) = app.get_webview_window(label) else {
        log::error!("pins: window '{}' not found, cannot convert", label);
        return;
    };

    match window.to_panel() {
        Ok(panel) => {
            #[allow(non_upper_case_globals)]
            const NSStatusWindowLevel: i32 = 25;
            panel.set_level(NSStatusWindowLevel);

            #[allow(non_upper_case_globals)]
            const NSWindowStyleMaskNonActivatingPanel: i32 = 1 << 7;
            panel.set_style_mask(NSWindowStyleMaskNonActivatingPanel);

            // Become key (receive the keyboard) on ANY click, not only when a
            // text field is clicked — required for ← / → arrow nav to reach the
            // pin even though it holds only an image. Pairs with the explicit
            // `focus_pin` grab on mousedown.
            panel.set_becomes_key_only_if_needed(false);

            panel.set_collection_behaviour(
                NSWindowCollectionBehavior::NSWindowCollectionBehaviorFullScreenAuxiliary
                    | NSWindowCollectionBehavior::NSWindowCollectionBehaviorCanJoinAllSpaces,
            );
        }
        Err(e) => log::error!("pins: to_panel failed for '{}': {:?}", label, e),
    }
}

/// Make WKWebView text inputs (the note field) reliably focusable / show a caret.
///
/// A borderless / non-titled window that CANNOT become the *main* window is the
/// classic cause of "clicking a web text input does nothing / no cursor" on
/// macOS. `tauri-nspanel`'s `RawNSPanel` overrides `canBecomeKeyWindow` (YES)
/// but leaves `canBecomeMainWindow` at NSPanel's default (NO), so text focus was
/// flaky ("works sometimes"). We add a `canBecomeMainWindow → YES` method to the
/// `RawNSPanel` class at runtime. This does NOT activate the app or change the
/// over-fullscreen / non-activating behavior (that's the style mask + collection
/// behavior); it only lets the panel hold first-responder focus for editing.
/// Call once, AFTER the first panel exists (so the class is registered).
#[cfg(target_os = "macos")]
pub fn patch_panel_focusable() {
    use tauri_nspanel::objc::runtime::{class_addMethod, Class, Object, Sel, BOOL, YES};
    use tauri_nspanel::objc::{sel, sel_impl};

    extern "C" fn can_become_main(_: &Object, _: Sel) -> BOOL {
        YES
    }

    unsafe {
        let Some(cls) = Class::get("RawNSPanel") else {
            log::error!("pins: RawNSPanel class not registered yet; can't patch focus");
            return;
        };
        let cls_mut = cls as *const Class as *mut Class;
        // Type encoding: BOOL return (c), self (@), _cmd (:).
        let types = b"c@:\0".as_ptr() as *const std::os::raw::c_char;
        let imp: extern "C" fn(&Object, Sel) -> BOOL = can_become_main;
        let added = class_addMethod(
            cls_mut,
            sel!(canBecomeMainWindow),
            std::mem::transmute::<_, tauri_nspanel::objc::runtime::Imp>(imp),
            types,
        );
        log::info!("pins: patched RawNSPanel canBecomeMainWindow (added={:?})", added);
    }
}

#[cfg(target_os = "macos")]
fn panel(
    app: &AppHandle,
    label: &str,
) -> Option<tauri_nspanel::objc_id::ShareId<tauri_nspanel::raw_nspanel::RawNSPanel>> {
    use tauri_nspanel::ManagerExt;
    app.get_webview_panel(label).ok()
}

fn show(app: &AppHandle, window: &WebviewWindow, label: &str) {
    #[cfg(target_os = "macos")]
    if let Some(panel) = panel(app, label) {
        panel.show();
        return;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, label);
    let _ = window.show();
}

fn hide(app: &AppHandle, window: &WebviewWindow, label: &str) {
    #[cfg(target_os = "macos")]
    if let Some(panel) = panel(app, label) {
        panel.order_out(None);
        return;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, label);
    let _ = window.hide();
}

fn is_visible(app: &AppHandle, window: &WebviewWindow, label: &str) -> bool {
    #[cfg(target_os = "macos")]
    if let Some(panel) = panel(app, label) {
        return panel.is_visible();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, label);
    window.is_visible().unwrap_or(false)
}

/// Make a panel the *key* window AND put its **WKWebView** (not the container
/// view) at the front of the responder chain, so the DOM receives the keyboard —
/// text inputs accept typing and ← / → `keydown` fires. Targeting the container
/// view instead (what `RawNSPanel::show()` does) silently steals focus away from
/// the webview, which is why typing/arrows "didn't work". All AppKit calls run on
/// the main thread. Without activating the app. On non-macOS, plain window focus.
#[allow(deprecated)] // cocoa `id` alias; same API the nspanel crate itself uses
fn focus_panel(app: &AppHandle, label: &str) {
    #[cfg(target_os = "macos")]
    if let Some(p) = panel(app, label) {
        let _ = app.run_on_main_thread(move || {
            use tauri_nspanel::cocoa::base::id;
            use tauri_nspanel::objc::{msg_send, sel, sel_impl};
            // The WKWebView is a subview of the panel's content view.
            let content: id = p.content_view();
            let webview: id = unsafe {
                let subviews: id = msg_send![content, subviews];
                let count: usize = msg_send![subviews, count];
                if count > 0 {
                    msg_send![subviews, objectAtIndex: 0usize]
                } else {
                    content
                }
            };
            p.make_first_responder(Some(webview));
            p.make_key_window();
        });
        return;
    }
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.set_focus();
    }
}

/// Robust arrow-key / ESC navigation for the single-mode viewer.
///
/// THE PROBLEM: getting hardware keys into a WKWebView that lives in a
/// non-activating NSPanel is flaky — `makeFirstResponder` on a WKWebView is a
/// long-standing WebKit weak spot, so after clicking a floating viewer (esp.
/// when another app on another screen had focus) the DOM `keydown` sometimes
/// never fired and ← / → did nothing.
///
/// THE FIX: an **app-local** `NSEvent` monitor. It fires whenever ANY PinShot
/// window is the *key* window (which clicking the viewer reliably makes it —
/// `becomesKeyOnlyIfNeeded(false)` + a panel that can become key), WITHOUT
/// needing the WKWebView to be first responder. It is NOT a global monitor, so
/// it never steals keys from other apps — only from PinShot's own key window.
/// We skip the control panel (its web text inputs need arrows/ESC) and only act
/// in single mode while pins are revealed; handled keys are swallowed so the DOM
/// listeners can't double-fire.
#[cfg(target_os = "macos")]
#[allow(deprecated)] // cocoa `id`/`nil` aliases; same API the nspanel crate uses
pub fn install_key_monitor(app: &AppHandle) {
    use block::ConcreteBlock;
    use tauri_nspanel::cocoa::base::{id, nil};
    use tauri_nspanel::objc::{class, msg_send, sel, sel_impl};

    const KEY_DOWN_MASK: u64 = 1 << 10; // NSEventMaskKeyDown
    const LEFT: u16 = 123;
    const RIGHT: u16 = 124;
    const ESC: u16 = 53;

    // The control panel hosts web text inputs (session name) that legitimately
    // use arrows/ESC, so never hijack keys when IT is key — only the viewers.
    let control_win: usize = panel(app, CONTROL_LABEL)
        .map(|p| {
            let content: id = p.content_view();
            let w: id = unsafe { msg_send![content, window] };
            w as usize
        })
        .unwrap_or(0);

    let app_handle = app.clone();
    let block = ConcreteBlock::new(move |event: id| -> id {
        let key_code: u16 = unsafe { msg_send![event, keyCode] };
        if key_code != LEFT && key_code != RIGHT && key_code != ESC {
            return event;
        }
        let win: id = unsafe { msg_send![event, window] };
        if win as usize == control_win {
            return event; // let the control panel's web UI handle it
        }
        // Snapshot deck state, then RELEASE the lock before re-entering commands
        // (the mutex isn't reentrant; deck_step_internal locks it again).
        let (single, revealed, count, text_editing) = {
            let store = app_handle.state::<PinStore>();
            let deck = store.0.lock().unwrap();
            (deck.mode == Mode::Single, deck.revealed, deck.images.len(), deck.text_editing)
        };
        // A note (or any web text field) is focused — let it have arrows + ESC.
        if text_editing {
            return event;
        }
        if !revealed {
            return event;
        }
        if key_code == ESC {
            hide_pins_internal(&app_handle);
            return nil;
        }
        if single && count > 1 {
            deck_step_internal(&app_handle, if key_code == LEFT { -1 } else { 1 });
            return nil; // swallow so the DOM keydown can't also fire
        }
        event
    });
    let block = block.copy();
    // AppKit copies/retains the handler, so the local `block` can drop after this.
    let _monitor: id = unsafe {
        msg_send![class!(NSEvent), addLocalMonitorForEventsMatchingMask: KEY_DOWN_MASK handler: &*block]
    };
}

#[cfg(not(target_os = "macos"))]
pub fn install_key_monitor(_app: &AppHandle) {}

/// Ensure the single-mode viewer has a size (~60% of the cursor monitor) and a
/// centered position the first time it's shown. Position then persists (drag the
/// header to move it); size stays the computed default.
fn ensure_viewer(app: &AppHandle, deck: &mut Deck) {
    if deck.single_size.is_some() && deck.single_pos.is_some() {
        return;
    }
    if let Some(m) = cursor_monitor(app) {
        let sf = m.scale_factor();
        let mon = m.size().to_logical::<f64>(sf);
        let w = (mon.width * 0.6).round();
        let h = (mon.height * 0.6).round();
        deck.single_size.get_or_insert((w, h));
        if deck.single_pos.is_none() {
            let mp = m.position();
            let px = mp.x + (((mon.width - w) / 2.0) * sf).round() as i32;
            let py = mp.y + (((mon.height - h) / 2.0) * sf).round() as i32;
            deck.single_pos = Some((px, py));
        }
    } else {
        deck.single_size.get_or_insert((800.0, 600.0));
        deck.single_pos.get_or_insert((120, 120));
    }
}

// --- the render: deck -> windows --------------------------------------------

/// Reconcile the windows with the deck + mode. Each visible image gets a window
/// positioned at its remembered spot; its view is pushed on a window-unique
/// event (`pin-view:<label>`) so windows never cross-talk. Unused windows hide.
fn render(app: &AppHandle, deck: &mut Deck) {
    // Reaching render means we're showing pins — clears the "launch quiet" flag.
    deck.revealed = true;
    let total = deck.images.len();
    let mode = deck.mode;
    if deck.current >= total {
        deck.current = total.saturating_sub(1);
    }

    // Which (window, deck-index) pairs are visible right now.
    let visible: Vec<(&'static str, usize)> = match mode {
        Mode::All => (0..total.min(pool_size()))
            .map(|i| (PIN_LABELS[i], i))
            .collect(),
        Mode::Single => {
            if total == 0 {
                Vec::new()
            } else {
                vec![(PIN_LABELS[0], deck.current)]
            }
        }
    };

    // Single mode shows everything inside one fixed, centered viewer rectangle.
    if mode == Mode::Single && total > 0 {
        ensure_viewer(app, deck);
    }

    deck.assign.clear();

    for (order, (label, idx)) in visible.iter().enumerate() {
        let label = *label;
        let idx = *idx;

        // In single mode the viewer keeps one stable position so cycling swaps
        // in place; in all mode each image remembers its own spot.
        let pos = if mode == Mode::Single {
            deck.single_pos.unwrap()
        } else {
            if deck.images[idx].pos.is_none() {
                deck.images[idx].pos = Some(default_pos(app, order));
            }
            deck.images[idx].pos.unwrap()
        };
        let ct = deck.images[idx].click_through;
        let id = deck.images[idx].id;

        if let Some(window) = app.get_webview_window(label) {
            // Single mode: Rust owns the window SIZE too (the fixed viewer rect);
            // all mode: the frontend sizes it to the image (fit × scale).
            if mode == Mode::Single {
                if let Some((w, h)) = deck.single_size {
                    let _ = window.set_size(tauri::LogicalSize::new(w, h));
                }
            }
            let _ = window.set_position(tauri::PhysicalPosition::new(pos.0, pos.1));
            let _ = window.set_ignore_cursor_events(ct);
            show(app, &window, label);
        }
        deck.assign.insert(label.to_string(), id);

        let view = make_view(&deck.images[idx], idx, total, mode, deck.favorites_view);
        let _ = app.emit(&format!("pin-view:{label}"), view);
    }

    // Hide every window not currently showing an image.
    for label in PIN_LABELS {
        if !visible.iter().any(|(l, _)| *l == label) {
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.set_ignore_cursor_events(false);
                hide(app, &window, label);
            }
        }
    }

    emit_summary(app, deck);
}

fn emit_summary(app: &AppHandle, deck: &Deck) {
    let summary = DeckSummary {
        count: deck.images.len(),
        mode: deck.mode.as_str().to_string(),
        current: if deck.images.is_empty() {
            0
        } else {
            deck.current + 1
        },
        any_click_through: deck.images.iter().any(|i| i.click_through),
        session_id: deck.active_session,
        revealed: deck.revealed,
        pool_size: pool_size(),
        favorites_view: deck.favorites_view,
    };
    let _ = app.emit("deck-changed", summary);
}

// --- commands ----------------------------------------------------------------

/// Read the clipboard, add a NEW image to the deck (becomes the current one),
/// and re-render. Returns the new image id.
#[tauri::command]
pub fn create_pin(app: AppHandle) -> Result<u64, String> {
    create_pin_internal(&app)
}

/// Same as [`create_pin`] but callable from the global shortcut / tray.
pub fn create_pin_internal(app: &AppHandle) -> Result<u64, String> {
    let (data_url, w, h) = read_clipboard_png(app)?;
    let (fit_w, fit_h) = match cursor_monitor(app) {
        Some(m) => fit_logical(&m, w, h),
        None => (w as f64, h as f64),
    };

    let store = app.state::<PinStore>();
    let mut deck = store.0.lock().unwrap();

    if deck.images.len() >= MAX_IMAGES {
        return Err(format!(
            "This session is holding the maximum of {MAX_IMAGES} images — close one first."
        ));
    }

    let payload = PinImagePayload {
        data_url,
        width: w,
        height: h,
        fit_w,
        fit_h,
    };

    // Persist the new image; its DB rowid becomes the deck id (1:1 mapping).
    // Heal the active session first so the pin always lands in a real session
    // (and re-assert it as active so app_state can never drift from where the
    // images actually go). If the write fails, FAIL LOUDLY — never keep an
    // unpersisted in-memory image that would silently vanish on switch/restart.
    let session_id = with_db(app, |c| ensure_active_session(c, deck.active_session));
    deck.active_session = session_id;
    // Pasting while the Favorites view is active auto-favorites the new image so
    // it shows up there (the view only loads favorite=1 images).
    let fav = with_db(app, |c| is_favorites_session(c, session_id));
    deck.favorites_view = fav;
    let id = match with_db(app, |c| {
        db_set_active(c, session_id);
        db_touch_session(c, session_id);
        db_insert_image(c, session_id, &payload, None, 1.0, 1.0, false, false, fav)
    }) {
        Ok(rowid) => rowid as u64,
        Err(e) => {
            log::error!("pins: db_insert_image failed: {e}");
            return Err(format!("Could not save the pin to the database: {e}"));
        }
    };
    log::info!("create_pin: saved image id={id} to session={session_id}");

    deck.images.push(DeckImage {
        id,
        image: payload,
        pos: None,
        scale: 1.0,
        opacity: 1.0,
        collapsed: false,
        click_through: false,
        favorite: fav,
        note: String::new(),
        color: String::new(),
    });
    deck.current = deck.images.len() - 1;
    let (mode, current, single_pos) = (deck.mode, deck.current, deck.single_pos);
    with_db(app, |c| db_set_session_meta(c, session_id, mode, current, single_pos));

    // Respect the current visibility: pasting while hidden adds the image to the
    // session but keeps everything hidden (the count updates; reveal to show).
    let count = deck.images.len();
    render_or_summary(app, &mut deck);
    // Tell the control panel a save succeeded (covers paste via button, ⌥⌘V, and
    // the tray) so it can show a confirmation toast.
    let _ = app.emit("pin-saved", count);
    Ok(id)
}

/// Replace an existing image (by id) with the current clipboard image.
#[tauri::command]
pub fn replace_image(app: AppHandle, store: State<PinStore>, id: u64) -> Result<(), String> {
    let (data_url, w, h) = read_clipboard_png(&app)?;
    let (fit_w, fit_h) = match cursor_monitor(&app) {
        Some(m) => fit_logical(&m, w, h),
        None => (w as f64, h as f64),
    };
    let mut deck = store.0.lock().unwrap();
    let Some(i) = find_index(&deck, id) else {
        return Err("That image is no longer pinned.".into());
    };
    deck.images[i].image = PinImagePayload {
        data_url,
        width: w,
        height: h,
        fit_w,
        fit_h,
    };
    deck.images[i].scale = 1.0;
    deck.images[i].collapsed = false;
    let payload = deck.images[i].image.clone();
    with_db(&app, |c| db_update_image_data(c, id, &payload, 1.0, false));
    render(&app, &mut deck);
    Ok(())
}

/// Hand a window its current view on (re)mount (covers reloads / late listeners).
#[tauri::command]
pub fn get_pin_view(store: State<PinStore>, label: String) -> Option<PinView> {
    let deck = store.0.lock().unwrap();
    let id = *deck.assign.get(&label)?;
    let i = find_index(&deck, id)?;
    Some(make_view(&deck.images[i], i, deck.images.len(), deck.mode, deck.favorites_view))
}

#[tauri::command]
pub fn get_deck_summary(store: State<PinStore>) -> serde_json::Value {
    let deck = store.0.lock().unwrap();
    serde_json::json!({
        "count": deck.images.len(),
        "mode": deck.mode.as_str(),
        "current": if deck.images.is_empty() { 0 } else { deck.current + 1 },
        "anyClickThrough": deck.images.iter().any(|i| i.click_through),
        "sessionId": deck.active_session,
        "revealed": deck.revealed,
        "poolSize": pool_size(),
        "favoritesView": deck.favorites_view,
    })
}

// --- live, high-frequency mutations (store only, no re-render) ---------------

#[tauri::command]
pub fn set_image_pos(app: AppHandle, store: State<PinStore>, id: u64, x: i32, y: i32) {
    let mut deck = store.0.lock().unwrap();
    // A drag in single mode moves the shared viewer (persist to the session); in
    // all mode it moves the specific image's window (persist to that row).
    if deck.mode == Mode::Single {
        deck.single_pos = Some((x, y));
        let (session_id, mode, current, single_pos) =
            (deck.active_session, deck.mode, deck.current, deck.single_pos);
        with_db(&app, |c| db_set_session_meta(c, session_id, mode, current, single_pos));
    } else if let Some(i) = find_index(&deck, id) {
        deck.images[i].pos = Some((x, y));
        with_db(&app, |c| db_update_image_pos(c, id, x, y));
    }
}

#[tauri::command]
pub fn set_image_scale(app: AppHandle, store: State<PinStore>, id: u64, scale: f64) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].scale = scale;
        with_db(&app, |c| db_update_image_scale(c, id, scale));
    }
}

#[tauri::command]
pub fn set_image_opacity(app: AppHandle, store: State<PinStore>, id: u64, opacity: f64) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].opacity = opacity;
        with_db(&app, |c| db_update_image_opacity(c, id, opacity));
    }
}

#[tauri::command]
pub fn set_image_collapsed(app: AppHandle, store: State<PinStore>, id: u64, collapsed: bool) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].collapsed = collapsed;
        with_db(&app, |c| db_update_image_collapsed(c, id, collapsed));
    }
}

/// Let the frontend resize its own window (snappy zoom / collapse).
#[tauri::command]
pub fn resize_pin(app: AppHandle, label: String, width: f64, height: f64, center: bool) {
    if let Some(window) = app.get_webview_window(&label) {
        resize_keep_anchor(&window, width, height, center);
    }
}

// --- structural mutations (re-render) ---------------------------------------

#[tauri::command]
pub fn close_image(app: AppHandle, store: State<PinStore>, id: u64) {
    let mut deck = store.0.lock().unwrap();

    // In the Favorites view, ✕ means "remove from Favorites", not "destroy": the
    // image still belongs to its real session. (An image that lives *in* the
    // Favorites session itself — pasted while it was active — has no other home,
    // so it's deleted.) Then reload the aggregation so it drops out.
    if deck.favorites_view {
        let fav_id = deck.active_session;
        let home: i64 = with_db(&app, |c| {
            c.query_row("SELECT session_id FROM images WHERE id=?1", params![id as i64], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0)
        });
        with_db(&app, |c| {
            if home == fav_id {
                db_delete_image(c, id);
            } else {
                db_set_image_favorite(c, id, false);
            }
        });
        let (images, mode, current, single_pos) =
            with_db(&app, |c| db_load_session(c, fav_id));
        deck.images = images;
        deck.mode = mode;
        deck.current = current;
        deck.single_pos = single_pos;
        render(&app, &mut deck);
        emit_sessions(&app, &deck);
        return;
    }

    if let Some(i) = find_index(&deck, id) {
        log::warn!(
            "close_image: deleting image id={id} from session={} (had {} images)",
            deck.active_session,
            deck.images.len()
        );
        deck.images.remove(i);
        if deck.current > i || deck.current >= deck.images.len() {
            deck.current = deck.current.saturating_sub(1);
        }
        let (session_id, mode, current, single_pos) =
            (deck.active_session, deck.mode, deck.current, deck.single_pos);
        with_db(&app, |c| {
            db_delete_image(c, id);
            db_set_session_meta(c, session_id, mode, current, single_pos);
        });
    }
    render(&app, &mut deck);
}

#[tauri::command]
pub fn close_all_pins(app: AppHandle, store: State<PinStore>) {
    let mut deck = store.0.lock().unwrap();

    // Favorites view: "Close all" empties the favorites list non-destructively —
    // un-favorite every foreign image (kept in its real session) and delete only
    // the images that live in the Favorites session itself.
    if deck.favorites_view {
        let fav_id = deck.active_session;
        with_db(&app, |c| {
            let _ = c.execute(
                "DELETE FROM images WHERE session_id=?1 AND favorite=1",
                params![fav_id],
            );
            let _ = c.execute("UPDATE images SET favorite=0 WHERE favorite=1", []);
        });
        deck.images.clear();
        deck.current = 0;
        render(&app, &mut deck);
        emit_sessions(&app, &deck);
        return;
    }

    log::warn!(
        "close_all_pins: deleting ALL {} images from session={}",
        deck.images.len(),
        deck.active_session
    );
    deck.images.clear();
    deck.current = 0;
    let (session_id, mode) = (deck.active_session, deck.mode);
    with_db(&app, |c| {
        db_delete_session_images(c, session_id);
        db_set_session_meta(c, session_id, mode, 0, None);
    });
    render(&app, &mut deck);
}

#[tauri::command]
pub fn set_image_click_through(app: AppHandle, store: State<PinStore>, id: u64, ignore: bool) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].click_through = ignore;
        with_db(&app, |c| db_update_image_click_through(c, id, ignore));
    }
    render(&app, &mut deck);
}

/// Persist an image's text note (store-only, no re-render — the editing window
/// already shows the live text optimistically; no other window shows this id).
/// Returns Err with a message if the DB write fails, so the UI can show a
/// "saved" confirmation ONLY on a real success (and an error otherwise).
#[tauri::command]
pub fn set_image_note(app: AppHandle, store: State<PinStore>, id: u64, note: String) -> Result<(), String> {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].note = note.clone();
    }
    with_db(&app, |c| db_update_image_note(c, id, &note))
        .map(|_| ())
        .map_err(|e| format!("Could not save the note: {e}"))
}

/// Persist an image's color tag (a preset hex string, or "" for none).
/// Store-only — the editing window applies the frame/dot from its local state.
#[tauri::command]
pub fn set_image_color(app: AppHandle, store: State<PinStore>, id: u64, color: String) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].color = color.clone();
        with_db(&app, |c| db_update_image_color(c, id, &color));
    }
}

/// Toggle the "a web text field is focused" flag so the native key monitor
/// stops hijacking ← / → / ESC while the user is typing a note.
#[tauri::command]
pub fn set_text_editing(store: State<PinStore>, editing: bool) {
    store.0.lock().unwrap().text_editing = editing;
}

/// Star / unstar an image for the cross-session Favorites view. Persists the
/// flag, then refreshes: if the Favorites view is the active deck the toggled
/// image appears/disappears (reload + re-render); in a normal session only the
/// star state updates. Either way the switcher's Favorites count is refreshed.
#[tauri::command]
pub fn set_image_favorite(app: AppHandle, store: State<PinStore>, id: u64, favorite: bool) {
    let mut deck = store.0.lock().unwrap();
    with_db(&app, |c| db_set_image_favorite(c, id, favorite));

    let active_is_fav = with_db(&app, |c| is_favorites_session(c, deck.active_session));
    if active_is_fav {
        // Reload the aggregated favorites so an un-favorited image drops out (and
        // re-favoriting elsewhere wouldn't apply here, but keeps it consistent).
        let (images, mode, current, single_pos) =
            with_db(&app, |c| db_load_session(c, deck.active_session));
        deck.images = images;
        deck.mode = mode;
        deck.current = current;
        deck.single_pos = single_pos;
    } else if let Some(i) = find_index(&deck, id) {
        deck.images[i].favorite = favorite;
    }
    render_or_summary(&app, &mut deck);
    emit_sessions(&app, &deck);
}

pub fn toggle_click_through_all_internal(app: &AppHandle) {
    let store = app.state::<PinStore>();
    let mut deck = store.0.lock().unwrap();
    if deck.images.is_empty() {
        return;
    }
    let any_on = deck.images.iter().any(|i| i.click_through);
    let next = !any_on;
    for img in deck.images.iter_mut() {
        img.click_through = next;
    }
    let ids: Vec<u64> = deck.images.iter().map(|i| i.id).collect();
    with_db(app, |c| {
        for id in &ids {
            db_update_image_click_through(c, *id, next);
        }
    });
    render(app, &mut deck);
}

#[tauri::command]
pub fn toggle_click_through_all(app: AppHandle) {
    toggle_click_through_all_internal(&app);
}

/// Switch between "show all" (`all = true`) and the single-image carousel.
#[tauri::command]
pub fn set_mode(app: AppHandle, store: State<PinStore>, all: bool) {
    let mut deck = store.0.lock().unwrap();
    deck.mode = if all { Mode::All } else { Mode::Single };
    let (session_id, mode, current, single_pos) =
        (deck.active_session, deck.mode, deck.current, deck.single_pos);
    with_db(&app, |c| db_set_session_meta(c, session_id, mode, current, single_pos));
    render_or_summary(&app, &mut deck);
}

#[tauri::command]
pub fn deck_step(app: AppHandle, delta: i32) {
    deck_step_internal(&app, delta);
}

/// Advance the single-mode carousel (also called by the native key monitor).
pub fn deck_step_internal(app: &AppHandle, delta: i32) {
    let store = app.state::<PinStore>();
    let mut deck = store.0.lock().unwrap();
    let n = deck.images.len();
    if n == 0 {
        return;
    }
    let cur = deck.current as i32;
    let next = ((cur + delta) % n as i32 + n as i32) % n as i32;
    deck.current = next as usize;
    let single = deck.mode == Mode::Single;
    let revealed = deck.revealed;
    let (session_id, mode, current, single_pos) =
        (deck.active_session, deck.mode, deck.current, deck.single_pos);
    with_db(app, |c| db_set_session_meta(c, session_id, mode, current, single_pos));
    render_or_summary(app, &mut deck);
    drop(deck);
    // A cycle only happens when a focused PinShot window received the arrow key,
    // so re-assert that focus on the single-mode viewer — render() re-shows the
    // window, which would otherwise reset first-responder and break the NEXT
    // arrow press. (No-op effect on focus for "show all"; skip while hidden.)
    if single && revealed {
        focus_panel(app, PIN_LABELS[0]);
    }
}

/// Make a pin (or the control panel) the key window so ← / → reach it. Called
/// from the frontend on mousedown — deterministic focus instead of relying on
/// AppKit's click-to-key heuristics (which fail when another app owns focus).
#[tauri::command]
pub fn focus_pin(app: AppHandle, label: String) {
    focus_panel(&app, &label);
}

// --- sessions ----------------------------------------------------------------

/// All sessions (with image counts) for the control-panel switcher.
#[tauri::command]
pub fn list_sessions(app: AppHandle, store: State<PinStore>) -> Vec<SessionInfo> {
    let active = store.0.lock().unwrap().active_session;
    with_db(&app, |c| db_list_sessions(c, active))
}

/// Create a fresh, empty session and switch to it (revealed = shows nothing,
/// since it's empty). Returns the new session id.
#[tauri::command]
pub fn create_session(app: AppHandle, store: State<PinStore>, name: String) -> i64 {
    let name = {
        let t = name.trim();
        if t.is_empty() {
            "Untitled".to_string()
        } else {
            t.to_string()
        }
    };
    let mut deck = store.0.lock().unwrap();
    let id = with_db(&app, |c| {
        let id = db_create_session(c, &name);
        db_set_active(c, id);
        id
    });
    deck.images.clear();
    deck.current = 0;
    deck.single_pos = None;
    deck.mode = Mode::All;
    deck.active_session = id;
    deck.favorites_view = false; // create_session always makes a normal session
    render_or_summary(&app, &mut deck);
    emit_sessions(&app, &deck);
    id
}

/// Load another session into the deck and show its pins (the current session is
/// already auto-saved continuously, so nothing is lost).
#[tauri::command]
pub fn switch_session(app: AppHandle, store: State<PinStore>, id: i64) {
    let mut deck = store.0.lock().unwrap();
    if deck.active_session == id {
        return;
    }
    let (images, mode, current, single_pos, favorites_view) = with_db(&app, |c| {
        db_set_active(c, id);
        db_touch_session(c, id);
        let fav = is_favorites_session(c, id);
        let (images, mode, current, single_pos) = db_load_session(c, id);
        (images, mode, current, single_pos, fav)
    });
    deck.images = images;
    deck.mode = mode;
    deck.current = current;
    deck.single_pos = single_pos;
    deck.active_session = id;
    deck.favorites_view = favorites_view;
    // Respect the global visibility: if pins are hidden, switching loads the new
    // session but keeps it hidden (the hidden state applies across all sessions).
    render_or_summary(&app, &mut deck);
    emit_sessions(&app, &deck);
}

#[tauri::command]
pub fn rename_session(app: AppHandle, store: State<PinStore>, id: i64, name: String) {
    let name = {
        let t = name.trim();
        if t.is_empty() {
            "Untitled".to_string()
        } else {
            t.to_string()
        }
    };
    with_db(&app, |c| {
        let _ = c.execute(
            "UPDATE sessions SET name=?1 WHERE id=?2",
            params![name, id],
        );
    });
    let deck = store.0.lock().unwrap();
    emit_sessions(&app, &deck);
}

/// Star / unstar a session — starred ones are pinned to the mini-bar quick list.
#[tauri::command]
pub fn set_session_starred(app: AppHandle, store: State<PinStore>, id: i64, starred: bool) {
    with_db(&app, |c| db_set_starred(c, id, starred));
    let deck = store.0.lock().unwrap();
    emit_sessions(&app, &deck);
}

/// Delete a session (cascades its images). If it was the active one, fall back
/// to another session (creating a default if it was the last) and show it.
#[tauri::command]
pub fn delete_session(app: AppHandle, store: State<PinStore>, id: i64) {
    let mut deck = store.0.lock().unwrap();
    // The Favorites view is permanent — never delete it (deleting it would also
    // wipe the favorited images, which still belong to their real sessions).
    if with_db(&app, |c| is_favorites_session(c, id)) {
        return;
    }
    let was_active = deck.active_session == id;
    log::warn!("delete_session: deleting session id={id} and its images");
    with_db(&app, |c| {
        // FK cascade is off — delete the session's images explicitly so they
        // don't linger as orphans.
        db_delete_session_images(c, id);
        let _ = c.execute("DELETE FROM sessions WHERE id=?1", params![id]);
    });
    if was_active {
        let (active, images, mode, current, single_pos, favorites_view) = with_db(&app, |c| {
            let active = db_active_or_init(c);
            let fav = is_favorites_session(c, active);
            let (images, mode, current, single_pos) = db_load_session(c, active);
            (active, images, mode, current, single_pos, fav)
        });
        deck.images = images;
        deck.mode = mode;
        deck.current = current;
        deck.single_pos = single_pos;
        deck.active_session = active;
        deck.favorites_view = favorites_view;
        render_or_summary(&app, &mut deck);
    }
    emit_sessions(&app, &deck);
}

/// Show the pins for the loaded session (used after a "launch quiet" startup,
/// or the Show/Hide toggle).
#[tauri::command]
pub fn reveal_pins(app: AppHandle, store: State<PinStore>) {
    let mut deck = store.0.lock().unwrap();
    render(&app, &mut deck);
}

/// Hide every pin window WITHOUT clearing the deck or the session — the images
/// (and their saved state) stay; only the windows go away. `revealed` flips to
/// false so the control panel shows "Show pins" again.
#[tauri::command]
pub fn hide_pins(app: AppHandle) {
    hide_pins_internal(&app);
}

/// Hide all pins (also called by the native key monitor on ESC).
pub fn hide_pins_internal(app: &AppHandle) {
    let store = app.state::<PinStore>();
    let mut deck = store.0.lock().unwrap();
    deck.revealed = false;
    for label in PIN_LABELS {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.set_ignore_cursor_events(false);
            hide(app, &window, label);
        }
    }
    deck.assign.clear();
    emit_summary(app, &deck);
}

/// Auto-arrange every image on the cursor's monitor so they're all visible and
/// as large as fit allows — no overlap. Uses each image's CURRENT displayed size
/// (`fit × scale`, so your zoom is respected), shelf-packs them left-to-right,
/// and shrinks everything by one uniform factor if the screen can't hold them at
/// full size. Switches to "Show all" and re-renders. Dependency-free: for ≤6
/// pins a heavy bin-packer (binpack2d / rectangle-pack) buys nothing over this.
#[tauri::command]
pub fn arrange_pins(app: AppHandle, store: State<PinStore>) {
    let mut deck = store.0.lock().unwrap();
    if deck.images.is_empty() {
        return;
    }
    let Some(monitor) = cursor_monitor(&app) else {
        deck.mode = Mode::All;
        render(&app, &mut deck);
        return;
    };
    let sf = monitor.scale_factor();
    let mon = monitor.size().to_logical::<f64>(sf);
    let work_w = (mon.width * 0.94).max(1.0);
    let work_h = (mon.height * 0.90).max(1.0);
    let gap = 12.0_f64;

    // Only the images "show all" can actually display (the window pool) get laid
    // out — don't shrink the visible ones to make room for off-screen extras.
    let n = deck.images.len().min(pool_size());
    // Each image's current on-screen size (logical) — collapsed pins count as
    // their expanded fit so arranging always lays out real images.
    let sizes: Vec<(f64, f64)> = deck
        .images
        .iter()
        .take(n)
        .map(|i| (i.image.fit_w * i.scale, i.image.fit_h * i.scale))
        .collect();

    // Shelf-pack at an extra uniform factor `f`; Some(placements) if it fits the
    // work area, else None.
    let pack = |f: f64| -> Option<Vec<(f64, f64)>> {
        let mut placements = Vec::with_capacity(sizes.len());
        let (mut x, mut y, mut row_h) = (0.0_f64, 0.0_f64, 0.0_f64);
        for (w, h) in &sizes {
            let (sw, sh) = (w * f, h * f);
            if x > 0.0 && x + sw > work_w + 0.5 {
                x = 0.0;
                y += row_h;
                row_h = 0.0;
            }
            placements.push((x, y));
            x += sw + gap;
            row_h = row_h.max(sh + gap);
        }
        if y + row_h <= work_h + 0.5 {
            Some(placements)
        } else {
            None
        }
    };

    // Largest factor in (0, 1] that fits (never upscale past current size).
    let mut best = pack(0.05).map(|p| (0.05_f64, p));
    let (mut lo, mut hi) = (0.05_f64, 1.0_f64);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if let Some(p) = pack(mid) {
            best = Some((mid, p));
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let Some((f, placements)) = best else {
        deck.mode = Mode::All;
        render(&app, &mut deck);
        return;
    };

    // Center the packed block within the work area.
    let used_w = placements
        .iter()
        .zip(&sizes)
        .map(|((x, _), (w, _))| x + w * f)
        .fold(0.0_f64, f64::max);
    let used_h = placements
        .iter()
        .zip(&sizes)
        .map(|((_, y), (_, h))| y + h * f)
        .fold(0.0_f64, f64::max);
    let off_x = ((mon.width - used_w) / 2.0).max(0.0);
    let off_y = ((mon.height - used_h) / 2.0).max(28.0); // clear of the menu bar

    let mp = monitor.position();
    for (i, (lx, ly)) in placements.iter().enumerate() {
        let px = mp.x + (((off_x + lx) * sf).round() as i32);
        let py = mp.y + (((off_y + ly) * sf).round() as i32);
        deck.images[i].pos = Some((px, py));
        deck.images[i].scale *= f;
        deck.images[i].collapsed = false;
    }
    deck.mode = Mode::All;

    // Persist the new layout (positions + scales + uncollapsed) and meta.
    let rows: Vec<(u64, (i32, i32), f64)> = deck
        .images
        .iter()
        .map(|i| (i.id, i.pos.unwrap_or((0, 0)), i.scale))
        .collect();
    let (session_id, mode, current, single_pos) =
        (deck.active_session, deck.mode, deck.current, deck.single_pos);
    with_db(&app, |c| {
        for (id, (x, y), scale) in &rows {
            db_update_image_pos(c, *id, *x, *y);
            db_update_image_scale(c, *id, *scale);
            db_update_image_collapsed(c, *id, false);
        }
        db_set_session_meta(c, session_id, mode, current, single_pos);
    });

    render(&app, &mut deck);
}

// --- control window ----------------------------------------------------------

fn position_top_right(window: &WebviewWindow, monitor: &Monitor, win_width: f64) {
    let scale = monitor.scale_factor();
    let size = monitor.size().to_logical::<f64>(scale);
    let pos = monitor.position().to_logical::<f64>(scale);
    let target = tauri::LogicalPosition::new(pos.x + size.width - win_width - 24.0, pos.y + 48.0);
    let _ = window.set_position(target);
}

pub fn show_control_initial(app: &AppHandle) {
    let Some(window) = app.get_webview_window(CONTROL_LABEL) else {
        return;
    };
    if let Some(monitor) = cursor_monitor(app) {
        position_top_right(&window, &monitor, CONTROL_WIDTH);
    }
    show(app, &window, CONTROL_LABEL);
}

/// Show the control panel WHERE IT IS — no repositioning. NSPanels retain their
/// frame across `order_out`/`order_front`, so this reappears it exactly where
/// the user last had it. Used by ⌥⌘P and the macOS Dock-icon reopen.
pub fn show_control(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(CONTROL_LABEL) {
        show(app, &window, CONTROL_LABEL);
    }
}

pub fn toggle_control_internal(app: &AppHandle) {
    let Some(window) = app.get_webview_window(CONTROL_LABEL) else {
        return;
    };
    if is_visible(app, &window, CONTROL_LABEL) {
        hide(app, &window, CONTROL_LABEL);
    } else {
        // Reappear in place (don't snap back to top-right). Only the initial
        // launch positions the panel; after that it stays where it was dragged.
        show(app, &window, CONTROL_LABEL);
    }
}

#[tauri::command]
pub fn toggle_control(app: AppHandle) {
    toggle_control_internal(&app);
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}
