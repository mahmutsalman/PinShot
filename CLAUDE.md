# PinShot

Floating screenshot-pin overlay (Tauri 2 + React + TS). Copy any screenshot to
the clipboard, pin it on top of everything — including other apps' fullscreen
Spaces — then enlarge / collapse / zoom / fade / drag it, and re-paste to update.
A reference-image HUD for tracing, comparing, or keeping a spec visible.

**Sibling of FocusFlow** — reuses its non-activating-NSPanel core (see
`~/.claude/notes/tauri-macos-floating-widget-over-fullscreen.md`), does NOT depend
on it.

## Architecture

- **Central deck → windows (the key model).** Rust owns the single source of
  truth: a `Deck` (managed `Mutex` in `PinStore`) holding `Vec<DeckImage>` (image
  + per-image position / scale / opacity / collapsed / click-through), a `mode`
  (`All` | `Single`), a `current` index, a `single_pos` (the carousel's stable
  viewer position), and an `assign` map (window label → image id currently shown).
  The frontend windows are **dumb renderers** driven by it. This replaced an
  earlier per-window-slot model whose bug was that `emit_to(label, …)` didn't
  reliably scope, so every window updated to the newest image ("two pins, same
  picture"). Fix: **window-unique event names** (see below).
- **Window pool, declared in `tauri.conf.json`** (never created at runtime — that
  silently fails in bundled .apps): one `control` panel + a fixed pool
  `pin-0..pin-5` (max images = `PIN_LABELS.len()` = 6, because "show all" needs
  one window per image). All start `visible:false`, `transparent`,
  decorationless. In `lib.rs` setup each is converted to a **non-activating
  NSPanel** (`commands/pins.rs::convert_to_panel`, level 25, `CanJoinAllSpaces |
  FullScreenAuxiliary`) so it floats over fullscreen and clicking it never
  activates the app. Show/hide go through the panel API on macOS.
- **One frontend bundle, branch on label** (`src/App.tsx`): `pin-*` → `<Pin/>`,
  else `<Control/>`.
- **`render(deck)` is the heart**: reconciles windows with the deck. All mode →
  `pin-i` shows `images[i]` at its remembered position; Single mode → only `pin-0`
  shows `images[current]` at `single_pos`, the rest hide. For each visible window
  it sets position (Rust owns position), `set_ignore_cursor_events`, shows it, and
  pushes a `PinView` on a **window-unique event `pin-view:<label>`** (broadcast
  `emit` + unique name = zero cross-talk). A `deck-changed` summary goes to the
  control panel. Called on every structural change (paste / replace / close /
  mode / cycle / click-through). `get_pin_view` re-feeds a window on mount.
- **Image flow**: `create_pin` reads the clipboard (`tauri-plugin-clipboard-
  manager`, Rust side — no JS capability), encodes PNG (`image` crate) → base64
  data URL, fits to the cursor's monitor, appends to the deck, makes it current,
  renders. `replace_image(id)` swaps an image in place.
- **Sizing/position split**: the **frontend** sizes its own window (`resize_pin`,
  snappy zoom/collapse — keeps top-left, the macOS bottom-left anchor captured +
  restored, same trick as FocusFlow); **Rust** owns position. Pins drag with OS
  `startDragging`; `win.onMoved` → `set_image_pos` persists the new spot (mode-
  aware: single mode writes `single_pos`, all mode writes the image's pos). High-
  frequency mutations (`set_image_pos/scale/opacity/collapsed`) are store-only (no
  re-render); structural ones re-render.
- **Click-through**: `set_image_click_through` → `set_ignore_cursor_events`,
  re-renders so the toggle stays in sync. ⌥⌘C (`toggle_click_through_all`) is the
  escape hatch when a pin is click-through and its toolbar is unreachable.

## Controls

- Global: **⌥⌘V** new pin · **⌥⌘C** toggle click-through (all) · **⌥⌘P** show/hide
  control panel. Tray mirrors these + Close All + Quit.
- Control panel: a **session switcher** (dropdown + name field + ＋/🗑), Paste, a
  **Show all / Single** mode toggle (`set_mode`), the deck count / carousel
  position with ‹ › nav (`deck_step`), a **Show N pins** reveal button (only when
  loaded-but-hidden after launch), Close all.
- **Arrange all** (`arrange_pins`): shelf-packs every image onto the cursor's
  monitor (no overlap, all visible), shrinking by one uniform factor if needed.
  Reads each image's *current* size (`fit × scale`, so zoom is respected),
  switches to Show all, persists the new pos/scale. Dependency-free built-in
  (a bin-packer crate buys nothing for ≤6 pins + scale-to-fit).
- Each pinned image has a subtle inset frame (1px ring + soft shadow).
- macOS: clicking the **Dock icon** (handled via `RunEvent::Reopen`) re-shows the
  control panel in place; ⌥⌘P and Dock both reappear it where you left it (panels
  retain their frame — we no longer force it back to the top-right corner).
- **Color-coded actions**: cyan = Paste (chunky square, the only cyan), violet =
  Show/Hide pins (outline when hidden), red = Close all (destructive). Mode
  toggle uses a neutral highlight so cyan stays unique to Paste.
- **Collapsed mini bar** (⊟ in titlebar, persisted in `localStorage` `pinshot.mini`):
  small Paste + Show/Hide + grip + ⤢ expand, plus a **vertical quick-switch list**
  of sessions (click → switch, then Paste lands there). The list is **starred
  sessions first (pinned, always shown), then most-recent**, capped at 5. Recency
  = `sessions.last_used`, star = `sessions.starred` (both migrated via `ALTER TABLE`;
  star toggled by the ☆/★ button on each row in the sessions pane via
  `set_session_starred`). Mini window: fixed width 248, height = `52 + n*29`.
- **Cross-session Favorites.** Each image has a `favorite` flag (★/☆ button on
  every pin's toolbar — all-mode toolbar, single-mode viewer header, and the
  collapsed chip-bar; `set_image_favorite`). A special, always-present,
  undeletable **Favorites session** (`sessions.is_favorites = 1`, name "★
  Favorites", created once in `init_store` via `db_favorites_or_init`) is a
  **view, not a container**: when it's active, `db_load_session` aggregates
  `images WHERE favorite=1` across ALL sessions instead of `WHERE session_id=`.
  The deck carries a `favorites_view` flag (set wherever `active_session`
  changes) surfaced on `PinView.favoritesView` + `DeckSummary.favoritesView`.
  In the Favorites view, **✕ / Close all are non-destructive** — they
  un-favorite (the original stays in its real session); only images that live
  *in* the Favorites session itself (pasted while it was active — auto-favorited
  so they show) are truly deleted. Edits (drag/zoom/opacity) persist to the
  original rows since the deck id == `images.id` rowid regardless of which
  session loaded it. `db_active_or_init` excludes Favorites so launch/healing
  never auto-lands there. Control panel: a ★ jump button on the session row,
  Favorites pinned to the top of the sessions pane + mini quick list (no
  delete/star-toggle, gold styling), and an in-panel explainer when active.
- **Visibility is global + sticky across sessions.** `revealed` is one deck-level
  flag; `switch_session`/`create_session`/`delete_session` go through
  `render_or_summary`, so hiding pins stays hidden when you switch sessions (and
  launch is always quiet, so hidden persists across restart).
- **Save toast**: `create_pin` emits a `pin-saved` event (count) after a
  successful insert; the control panel shows a green "✓ Saved to database (N)"
  toast for ~2.2s. Covers paste via button, ⌥⌘V, and tray — works in mini too.
- **Show all** = every image visible at once at its saved position. **Single** =
  one **fixed viewer rectangle** (a single window, ~60% of the monitor, centered
  first time, position persisted via `single_pos`, size = `Deck.single_size`
  set by `ensure_viewer`; **Rust owns the window size in single mode**, frontend
  skips its resize). Header bar = drag to move + ‹ N/M › nav + ⚙ + ✕; body =
  the image shown `object-fit: contain`, **drag to pan**, **⌘+scroll to zoom**
  (native non-passive listener), reset-to-fit on every image switch (transient
  `zoom`/`pan` state in `Pin`, not the persisted `scale`). Keeps differently-
  sized images framed in one stable spot.
- **Arrow-key carousel nav**: in Single mode with >1 pin, ← / → cycle the deck
  (`deckStep(∓1)`). Implemented as a window-local `keydown` listener in both
  `Pin` and `Control` — NOT a global shortcut — so arrows only fire while a
  PinShot window is focused and never steal arrow keys from other apps.
  Click-through pins can't be focused, so arrows won't reach them — expected.
  **ESC** (while a pin/viewer or the control panel is focused) hides all pins
  (`hide_pins`) — a quick dismiss while navigating.
  - **Primary mechanism: an app-local `NSEvent` key monitor** (`install_key_monitor`,
    macOS). It catches ← / → / ESC whenever ANY PinShot panel is the *key* window —
    which clicking the viewer reliably makes it — **without** depending on the
    WKWebView becoming first responder (that grab is the flaky part: WebKit's
    `makeFirstResponder` on a WKWebView is a long-standing weak spot, so after
    clicking a floating viewer the DOM `keydown` sometimes never fired). It is
    **app-local, not global**, so it only ever takes keys from PinShot's own key
    window — never from other apps. It skips the control panel (whose web text
    inputs need arrows/ESC), only acts in single mode while revealed, and
    **swallows** handled keys so the DOM listeners below can't double-fire. The
    DOM `keydown` listeners in `Pin`/`Control` remain as a secondary path (and
    handle the control panel). This is the robust fix for "I clicked the rectangle
    on another screen but arrows didn't navigate".
  - **Focus is also grabbed deterministically, not via AppKit heuristics** (those
    were flaky — "works once then stops", or arrows leaking to the app you were
    in). Three pieces, all required: (1) `convert_to_panel` sets
    `becomesKeyOnlyIfNeeded(false)` so clicking the *image* (not just a text
    field) makes the panel key; (2) every primary mousedown calls the
    `focus_pin` command → `focus_panel`, which runs `makeFirstResponder` +
    `makeKeyWindow` **on the main thread** (`run_on_main_thread` — off-thread
    AppKit calls silently no-op and were the root of the flakiness); (3)
    `deck_step` re-asserts `focus_panel(pin-0)` after its render in single mode,
    because `RawNSPanel::show()` resets first-responder and would otherwise kill
    the NEXT arrow press. A cycle only happens when a focused PinShot window got
    the key, so re-asserting never steals focus unprompted.
- Per pin: a top-right **⚙ toggle** reveals/hides the toolbar (it used to show on
  hover, which covered the image). Toolbar: collapse, zoom −/%/+, opacity slider,
  replace (⟳ = re-paste clipboard), click-through (👆), close. The ⚙ is subtle by
  default, brighter on hover, accent when open. Scroll over a pin = zoom.

## Conventions

- After changing Rust: `cd src-tauri && cargo check`. After TS: `npm run build`.
- **Capacity is decoupled from the window pool.** A session holds up to
  `MAX_IMAGES` (50) images — Single mode carousels through all of them in one
  window. "Show all" can only display `pool_size()` = `PIN_LABELS.len()` (12) at
  once (one window each); beyond that the control panel shows "showing 12 (Single
  mode for all)". To change the pool, declare more/fewer windows in
  `tauri.conf.json`, mirror them in `capabilities/default.json` `windows`, and
  edit `PIN_LABELS` — those three must stay in sync. (More windows = more
  always-loaded WKWebviews = more memory; 12 is the balance.)
- **Hidden state is sticky.** When pins are hidden (`revealed == false`: after
  "Hide pins" or launch-quiet), pasting / mode-toggle / cycle go through
  `render_or_summary` — the image is stored + the count updates, but nothing is
  shown until you reveal. Only an explicit reveal/switch/arrange shows pins.
- **Persistence: SQLite sessions** (`<app-data>/pinshot.sqlite3`, via `rusqlite`
  bundled). Tables: `sessions` (name, mode, current_idx, single_pos) → `images`
  (data_url + per-pin pos/scale/opacity/collapsed/click_through) with
  `ON DELETE CASCADE`; `app_state` holds the active session id. **The deck image
  `id` IS the `images.id` rowid** (1:1), so high-frequency drags/zooms persist as
  a single targeted `UPDATE`, not a full rewrite. Every mutating command
  auto-saves (`with_db` + `db_*` helpers). `init_store` (setup) opens the DB,
  heals/creates the active session, and loads it into the deck **without showing
  pins** ("launch quiet"); the control panel shows a "Show N pins" button while
  `revealed == false`. `render()` sets `revealed = true`. Switching sessions
  (`switch_session`) loads + renders immediately. Session CRUD commands:
  `list_sessions`, `create_session`, `switch_session`, `rename_session`,
  `delete_session`, `reveal_pins`; the control panel has a switcher dropdown +
  name field + ＋ / 🗑. `sessions-changed` event re-feeds the switcher.
- Install to /Applications (with the required ad-hoc re-sign): `./install.sh`.
- `Info.plist` keeps a Dock icon; flip `LSUIElement` to run Dock-less.
- Icons in `src-tauri/icons/` are placeholders copied from FocusFlow — replace.
