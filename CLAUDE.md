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
- Control panel: Paste, a **Show all / Single** mode toggle (`set_mode`), the deck
  count / carousel position with ‹ › nav (`deck_step`), Close all.
- **Show all** = every image visible at once at its saved position. **Single** =
  one image (the carousel) with ‹ › nav on the viewer + control panel to cycle.
- **Arrow-key carousel nav**: in Single mode with >1 pin, ← / → cycle the deck
  (`deckStep(∓1)`). Implemented as a window-local `keydown` listener in both
  `Pin` and `Control` — NOT a global shortcut — so arrows only fire while a
  PinShot window is focused and never steal arrow keys from other apps.
  Click-through pins can't be focused, so arrows won't reach them — expected.
  - **Focus is grabbed deterministically, not via AppKit heuristics** (those
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
- Per pin (hover toolbar): collapse, zoom −/%/+, opacity slider, replace (⟳ =
  re-paste clipboard), click-through (👆), close. Scroll over a pin = zoom.

## Conventions

- After changing Rust: `cd src-tauri && cargo check`. After TS: `npm run build`.
- Max images = `PIN_LABELS.len()` (6). To change, declare more/fewer windows in
  `tauri.conf.json`, mirror them in `capabilities/default.json` `windows`, and
  edit the `PIN_LABELS` array — those three must stay in sync.
- Pins persist nothing — images live in memory only; quitting clears them.
- Install to /Applications (with the required ad-hoc re-sign): `./install.sh`.
- `Info.plist` keeps a Dock icon; flip `LSUIElement` to run Dock-less.
- Icons in `src-tauri/icons/` are placeholders copied from FocusFlow — replace.
