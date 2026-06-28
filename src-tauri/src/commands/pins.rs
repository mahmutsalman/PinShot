use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Monitor, State, WebviewWindow};

pub const CONTROL_LABEL: &str = "control";
pub const PIN_LABELS: [&str; 6] = ["pin-0", "pin-1", "pin-2", "pin-3", "pin-4", "pin-5"];
const CONTROL_WIDTH: f64 = 232.0;
// Largest fraction of the monitor a freshly-pinned image is allowed to take.
const FIT_FRACTION: f64 = 0.85;

/// Maximum images held at once — bounded by the pin-window pool (every image
/// needs its own window when "show all" is on).
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
    /// window label -> image id currently shown there (rebuilt every render).
    assign: HashMap<String, u64>,
    next_id: u64,
}

impl Default for Deck {
    fn default() -> Self {
        Deck {
            images: Vec::new(),
            mode: Mode::All,
            current: 0,
            single_pos: None,
            assign: HashMap::new(),
            next_id: 1,
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
}

#[derive(Clone, serde::Serialize)]
struct DeckSummary {
    count: usize,
    mode: String,
    current: usize,
    #[serde(rename = "anyClickThrough")]
    any_click_through: bool,
}

fn make_view(img: &DeckImage, index: usize, total: usize, mode: Mode) -> PinView {
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
    }
}

fn find_index(deck: &Deck, id: u64) -> Option<usize> {
    deck.images.iter().position(|i| i.id == id)
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

/// Make a panel the *key* window (so its webview receives the keyboard) WITHOUT
/// activating the app. The AppKit calls (`makeKeyWindow`, `makeFirstResponder`)
/// must run on the main thread — dispatching here is what makes arrow-key focus
/// robust instead of "works once then stops". On non-macOS, fall back to the
/// ordinary window focus. Used on pin click and re-asserted after each cycle.
fn focus_panel(app: &AppHandle, label: &str) {
    #[cfg(target_os = "macos")]
    if let Some(p) = panel(app, label) {
        let _ = app.run_on_main_thread(move || {
            let content = p.content_view();
            p.make_first_responder(Some(content));
            p.make_key_window();
        });
        return;
    }
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.set_focus();
    }
}

// --- the render: deck -> windows --------------------------------------------

/// Reconcile the windows with the deck + mode. Each visible image gets a window
/// positioned at its remembered spot; its view is pushed on a window-unique
/// event (`pin-view:<label>`) so windows never cross-talk. Unused windows hide.
fn render(app: &AppHandle, deck: &mut Deck) {
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

    deck.assign.clear();

    for (order, (label, idx)) in visible.iter().enumerate() {
        let label = *label;
        let idx = *idx;

        // In single mode the viewer keeps one stable position (seeded from the
        // first image's spot, or a default) so cycling swaps in place.
        let pos = if mode == Mode::Single {
            if deck.single_pos.is_none() {
                deck.single_pos = Some(deck.images[idx].pos.unwrap_or_else(|| default_pos(app, 0)));
            }
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
            let _ = window.set_position(tauri::PhysicalPosition::new(pos.0, pos.1));
            let _ = window.set_ignore_cursor_events(ct);
            show(app, &window, label);
        }
        deck.assign.insert(label.to_string(), id);

        let view = make_view(&deck.images[idx], idx, total, mode);
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

    if deck.images.len() >= pool_size() {
        return Err(format!(
            "Holding the maximum of {} images — close one first.",
            pool_size()
        ));
    }

    let id = deck.next_id;
    deck.next_id += 1;
    deck.images.push(DeckImage {
        id,
        image: PinImagePayload {
            data_url,
            width: w,
            height: h,
            fit_w,
            fit_h,
        },
        pos: None,
        scale: 1.0,
        opacity: 1.0,
        collapsed: false,
        click_through: false,
    });
    deck.current = deck.images.len() - 1;

    render(app, &mut deck);
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
    render(&app, &mut deck);
    Ok(())
}

/// Hand a window its current view on (re)mount (covers reloads / late listeners).
#[tauri::command]
pub fn get_pin_view(store: State<PinStore>, label: String) -> Option<PinView> {
    let deck = store.0.lock().unwrap();
    let id = *deck.assign.get(&label)?;
    let i = find_index(&deck, id)?;
    Some(make_view(&deck.images[i], i, deck.images.len(), deck.mode))
}

#[tauri::command]
pub fn get_deck_summary(store: State<PinStore>) -> serde_json::Value {
    let deck = store.0.lock().unwrap();
    serde_json::json!({
        "count": deck.images.len(),
        "mode": deck.mode.as_str(),
        "current": if deck.images.is_empty() { 0 } else { deck.current + 1 },
        "anyClickThrough": deck.images.iter().any(|i| i.click_through),
    })
}

// --- live, high-frequency mutations (store only, no re-render) ---------------

#[tauri::command]
pub fn set_image_pos(store: State<PinStore>, id: u64, x: i32, y: i32) {
    let mut deck = store.0.lock().unwrap();
    // A drag in single mode moves the shared viewer; in all mode it moves the
    // specific image's window.
    if deck.mode == Mode::Single {
        deck.single_pos = Some((x, y));
    } else if let Some(i) = find_index(&deck, id) {
        deck.images[i].pos = Some((x, y));
    }
}

#[tauri::command]
pub fn set_image_scale(store: State<PinStore>, id: u64, scale: f64) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].scale = scale;
    }
}

#[tauri::command]
pub fn set_image_opacity(store: State<PinStore>, id: u64, opacity: f64) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].opacity = opacity;
    }
}

#[tauri::command]
pub fn set_image_collapsed(store: State<PinStore>, id: u64, collapsed: bool) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].collapsed = collapsed;
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
    if let Some(i) = find_index(&deck, id) {
        deck.images.remove(i);
        if deck.current > i || deck.current >= deck.images.len() {
            deck.current = deck.current.saturating_sub(1);
        }
    }
    render(&app, &mut deck);
}

#[tauri::command]
pub fn close_all_pins(app: AppHandle, store: State<PinStore>) {
    let mut deck = store.0.lock().unwrap();
    deck.images.clear();
    deck.current = 0;
    render(&app, &mut deck);
}

#[tauri::command]
pub fn set_image_click_through(app: AppHandle, store: State<PinStore>, id: u64, ignore: bool) {
    let mut deck = store.0.lock().unwrap();
    if let Some(i) = find_index(&deck, id) {
        deck.images[i].click_through = ignore;
    }
    render(&app, &mut deck);
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
    render(&app, &mut deck);
}

#[tauri::command]
pub fn deck_step(app: AppHandle, store: State<PinStore>, delta: i32) {
    let mut deck = store.0.lock().unwrap();
    let n = deck.images.len();
    if n == 0 {
        return;
    }
    let cur = deck.current as i32;
    let next = ((cur + delta) % n as i32 + n as i32) % n as i32;
    deck.current = next as usize;
    let single = deck.mode == Mode::Single;
    render(&app, &mut deck);
    drop(deck);
    // A cycle only happens when a focused PinShot window received the arrow key,
    // so re-assert that focus on the single-mode viewer — render() re-shows the
    // window, which would otherwise reset first-responder and break the NEXT
    // arrow press. (No-op effect on focus for "show all".)
    if single {
        focus_panel(&app, PIN_LABELS[0]);
    }
}

/// Make a pin (or the control panel) the key window so ← / → reach it. Called
/// from the frontend on mousedown — deterministic focus instead of relying on
/// AppKit's click-to-key heuristics (which fail when another app owns focus).
#[tauri::command]
pub fn focus_pin(app: AppHandle, label: String) {
    focus_panel(&app, &label);
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

pub fn toggle_control_internal(app: &AppHandle) {
    let Some(window) = app.get_webview_window(CONTROL_LABEL) else {
        return;
    };
    if is_visible(app, &window, CONTROL_LABEL) {
        hide(app, &window, CONTROL_LABEL);
    } else {
        if let Some(monitor) = cursor_monitor(app) {
            position_top_right(&window, &monitor, CONTROL_WIDTH);
        }
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
