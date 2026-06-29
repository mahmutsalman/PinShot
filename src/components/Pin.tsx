import { useEffect, useRef, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { listen } from "@tauri-apps/api/event";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  type PinView,
  getPinView,
  resizePin,
  replaceImage,
  closeImage,
  setImagePos,
  setImageScale,
  setImageOpacity,
  setImageCollapsed,
  setImageClickThrough,
  setImageFavorite,
  setImageNote,
  saveImageNote,
  setImageColor,
  setTextEditing,
  deckStep,
  focusPin,
  focusPinEdit,
  hidePins,
} from "../lib/ipc";

const win = getCurrentWebviewWindow();
const label = win.label;

/** Preset color tags for images (frame + dot). "" = no color. */
const PALETTE = [
  "#ef4444", // red
  "#f59e0b", // amber
  "#eab308", // yellow
  "#22c55e", // green
  "#38bdf8", // sky
  "#a78bfa", // violet
  "#ec4899", // pink
] as const;

const NOTE_SAVE_DELAY = 500; // ms debounce before persisting note edits

const ZOOM_STEP = 1.15;
const ZOOM_MIN = 0.15;
const ZOOM_MAX = 4;
const COLLAPSED_MAX = 132; // largest dimension of the collapsed thumbnail (logical px)
const OPACITY_MIN = 0.15;

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** Drag the whole image except when the press lands on a real control. Any
 *  primary click first grabs keyboard focus so ← / → land on THIS pin (even if
 *  another app, e.g. a text editor, currently owns focus). */
function onDragStart(e: React.MouseEvent) {
  if (e.button !== 0) return;
  void focusPin(label);
  if ((e.target as HTMLElement).closest("button, input, textarea")) return;
  void win.startDragging();
}

export default function Pin() {
  const [view, setView] = useState<PinView | null>(null);
  // Live, locally-driven values (seeded from the view, persisted back to Rust).
  const [scale, setScale] = useState(1);
  const [opacity, setOpacity] = useState(1);
  const [collapsed, setCollapsed] = useState(false);
  const [clickThrough, setClickThrough] = useState(false);
  const [favorite, setFavorite] = useState(false);
  const [note, setNote] = useState("");
  const [color, setColor] = useState("");
  // Debounced note persistence: remember the (id, text) still owed to the DB so
  // we can flush it on blur / when the shown image changes (never lose an edit).
  const noteTimer = useRef<number | null>(null);
  const pendingNote = useRef<{ id: number; text: string } | null>(null);
  // Save confirmation / error shown inside the pin (green ok, red error).
  const [noteToast, setNoteToast] = useState<{ ok: boolean; text: string } | null>(null);
  const noteToastTimer = useRef<number | null>(null);
  // Brief green pulse on the note field right after a successful save.
  const [savedPulse, setSavedPulse] = useState(false);
  const savedPulseTimer = useRef<number | null>(null);
  // Toolbar is hidden by default (it covered the image) — revealed by the ⚙.
  const [showTools, setShowTools] = useState(false);
  // Single-mode viewer: transient zoom + pan of the image WITHIN the fixed
  // viewer rectangle (reset to fit whenever the shown image changes).
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const idRef = useRef<number | null>(null);
  const panning = useRef<{ sx: number; sy: number; px: number; py: number } | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  // Mirror zoom/pan into refs so the native wheel listener reads fresh values.
  const zoomRef = useRef(1);
  const panRef = useRef({ x: 0, y: 0 });
  useEffect(() => {
    zoomRef.current = zoom;
  }, [zoom]);
  useEffect(() => {
    panRef.current = pan;
  }, [pan]);

  // Receive this window's view (render / cycle / replace) and on (re)mount.
  useEffect(() => {
    void getPinView(label).then((v) => v && setView(v));
    const un = listen<PinView>(`pin-view:${label}`, (e) => setView(e.payload));
    return () => {
      void un.then((f) => f());
    };
  }, []);

  // Persist any note still owed to the DB right now (on blur, image switch, etc.).
  function flushNote() {
    if (noteTimer.current) {
      window.clearTimeout(noteTimer.current);
      noteTimer.current = null;
    }
    const p = pendingNote.current;
    if (p) {
      void setImageNote(p.id, p.text);
      pendingNote.current = null;
    }
  }

  // Seed local state whenever the shown image changes (cycle / replace / mode).
  // Resetting zoom/pan here is the "reset to fit on switch" behavior.
  useEffect(() => {
    if (!view) return;
    // Save the previous image's pending note before swapping in the new one.
    flushNote();
    setScale(view.scale);
    setOpacity(view.opacity);
    setCollapsed(view.collapsed);
    setClickThrough(view.clickThrough);
    setFavorite(view.favorite);
    setNote(view.note);
    setColor(view.color);
    setZoom(1);
    setPan({ x: 0, y: 0 });
    idRef.current = view.id;
  }, [view?.id, view?.dataUrl]);

  // Keep the native window sized to the current image. NOT in single mode — the
  // viewer rectangle's size is owned by Rust there. Collapse → thumbnail.
  useEffect(() => {
    if (!view || view.mode === "single") return;
    if (collapsed) {
      const ar = view.fitW / view.fitH;
      const [w, h] =
        ar >= 1 ? [COLLAPSED_MAX, COLLAPSED_MAX / ar] : [COLLAPSED_MAX * ar, COLLAPSED_MAX];
      void resizePin(label, w, h, false);
    } else {
      void resizePin(label, view.fitW * scale, view.fitH * scale, false);
    }
  }, [view, collapsed, scale]);

  // ← / → cycle the carousel (single mode, >1) and ESC hides all pins — both
  // only while THIS pin window is focused (never steals keys from other apps).
  useEffect(() => {
    const single = view?.mode === "single";
    const canCycle = single && (view?.total ?? 0) > 1;
    const onKey = (e: KeyboardEvent) => {
      // Never hijack keys while typing in a note (or any text field).
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "TEXTAREA" || t.tagName === "INPUT")) return;
      if (e.key === "Escape") {
        e.preventDefault();
        void hidePins();
      } else if (canCycle && e.key === "ArrowLeft") {
        e.preventDefault();
        void deckStep(-1);
      } else if (canCycle && e.key === "ArrowRight") {
        e.preventDefault();
        void deckStep(1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [view?.mode, view?.total]);

  // Persist this window's position after the user drags it.
  useEffect(() => {
    const p = win.onMoved(({ payload }) => {
      if (idRef.current != null) void setImagePos(idRef.current, payload.x, payload.y);
    });
    return () => {
      void p.then((f) => f());
    };
  }, []);

  function applyZoom(next: number) {
    const z = clamp(next, ZOOM_MIN, ZOOM_MAX);
    setScale(z);
    if (view) void setImageScale(view.id, z);
  }

  function onWheel(e: React.WheelEvent) {
    if (collapsed || !view) return;
    applyZoom(scale * (e.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP));
  }

  function changeOpacity(v: number) {
    setOpacity(v);
    if (view) void setImageOpacity(view.id, v);
  }

  function toggleCollapsed() {
    const next = !collapsed;
    setCollapsed(next);
    if (view) void setImageCollapsed(view.id, next);
  }

  function toggleClickThrough() {
    const next = !clickThrough;
    setClickThrough(next);
    if (view) void setImageClickThrough(view.id, next);
  }

  function toggleFavorite() {
    const next = !favorite;
    setFavorite(next);
    if (view) void setImageFavorite(view.id, next);
  }

  // --- note + color ----------------------------------------------------------

  function onNoteChange(text: string) {
    setNote(text);
    if (!view) return;
    pendingNote.current = { id: view.id, text };
    if (noteTimer.current) window.clearTimeout(noteTimer.current);
    noteTimer.current = window.setTimeout(flushNote, NOTE_SAVE_DELAY);
  }

  // Grab keyboard focus on the PRESS, before the browser tries to focus the
  // field — on a floating panel the first click is often consumed just making
  // the window key, so the textarea's own focus event may never fire. Making the
  // panel key + webview first-responder here lets the same click land the caret.
  function onNotePointerDown() {
    void focusPinEdit(label);
  }

  // Tell the backend a text field is focused so the native key monitor stops
  // grabbing ← / → / ESC (otherwise they'd cycle/hide instead of editing).
  function onNoteFocus() {
    void focusPinEdit(label);
    void setTextEditing(true);
  }
  function onNoteBlur() {
    flushNote();
    void setTextEditing(false);
  }

  function showNoteToast(ok: boolean, text: string) {
    setNoteToast({ ok, text });
    if (noteToastTimer.current) window.clearTimeout(noteToastTimer.current);
    // Success disappears quickly; an error lingers so it can be read.
    noteToastTimer.current = window.setTimeout(() => setNoteToast(null), ok ? 2000 : 4500);
  }

  // Enter = save now (and confirm); Shift+Enter = newline. Confirmation shows
  // ONLY after the DB write actually succeeds; a failure shows a red message.
  async function saveNoteNow() {
    if (!view) return;
    if (noteTimer.current) {
      window.clearTimeout(noteTimer.current);
      noteTimer.current = null;
    }
    pendingNote.current = null;
    const id = view.id;
    try {
      await saveImageNote(id, note);
      showNoteToast(true, "✓ Saved to database");
      // Saved: blur the field (caret leaves → a clear "done/saved" state; click
      // the note again to edit) and flash a brief green pulse on it.
      (document.activeElement as HTMLElement | null)?.blur();
      setSavedPulse(true);
      if (savedPulseTimer.current) window.clearTimeout(savedPulseTimer.current);
      savedPulseTimer.current = window.setTimeout(() => setSavedPulse(false), 1100);
    } catch (err) {
      // Keep the text owed so a later blur/autosave can retry (stay in the field).
      pendingNote.current = { id, text: note };
      showNoteToast(false, `⚠ Not saved — ${String(err)}`);
    }
  }

  function onNoteKeyDown(e: React.KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Escape") {
      (e.target as HTMLTextAreaElement).blur();
    } else if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault(); // Enter saves; Shift+Enter makes a newline
      void saveNoteNow();
    }
  }

  function chooseColor(c: string) {
    const next = color === c ? "" : c; // click the active swatch again to clear
    setColor(next);
    if (view) void setImageColor(view.id, next);
  }

  const swatchRow = (
    <div className="swatch-row" title="Color tag">
      {PALETTE.map((c) => (
        <button
          key={c}
          className={`swatch${color === c ? " on" : ""}`}
          style={{ background: c }}
          title={color === c ? "Clear color" : "Set color"}
          onClick={() => chooseColor(c)}
        />
      ))}
      <button
        className={`swatch none${color === "" ? " on" : ""}`}
        title="No color"
        onClick={() => chooseColor("")}
      >
        ⦸
      </button>
    </div>
  );

  // --- single-mode viewer interactions ---------------------------------------

  /** Drag the header to MOVE the viewer window. */
  function onHeadDown(e: React.MouseEvent) {
    if (e.button !== 0) return;
    void focusPin(label);
    if ((e.target as HTMLElement).closest("button, input, textarea")) return;
    void win.startDragging();
  }

  /** Drag the body to PAN the image inside the viewer. */
  function onBodyDown(e: React.MouseEvent) {
    if (e.button !== 0) return;
    void focusPin(label);
    if ((e.target as HTMLElement).closest("button, input, textarea")) return;
    panning.current = { sx: e.clientX, sy: e.clientY, px: pan.x, py: pan.y };
    const move = (ev: MouseEvent) => {
      const p = panning.current;
      if (!p) return;
      setPan({ x: p.px + (ev.clientX - p.sx), y: p.py + (ev.clientY - p.sy) });
    };
    const up = () => {
      panning.current = null;
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }

  // ⌘ + scroll zooms toward the cursor; plain scroll is ignored. Native (not
  // React) listener so it's non-passive — preventDefault stops WKWebView from
  // page-zooming, and it reads zoom/pan from refs to stay current.
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    const onWheelNative = (e: WheelEvent) => {
      if (!e.metaKey) return;
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const cx = e.clientX - rect.left - rect.width / 2;
      const cy = e.clientY - rect.top - rect.height / 2;
      const z = zoomRef.current;
      const nz = clamp(z * (e.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP), ZOOM_MIN, ZOOM_MAX);
      const k = nz / z;
      const p = panRef.current;
      setPan({ x: cx - (cx - p.x) * k, y: cy - (cy - p.y) * k });
      setZoom(nz);
    };
    el.addEventListener("wheel", onWheelNative, { passive: false });
    return () => el.removeEventListener("wheel", onWheelNative);
  }, [view?.mode]);

  /** Zoom from center (toolbar −/+ buttons). */
  function zoomBy(factor: number) {
    const nz = clamp(zoom * factor, ZOOM_MIN, ZOOM_MAX);
    const k = nz / zoom;
    setPan((p) => ({ x: p.x * k, y: p.y * k }));
    setZoom(nz);
  }

  function resetView() {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }

  // Closing a pin permanently removes that image from the saved session, so
  // confirm first — this is the main way images were getting lost. In the
  // Favorites view ✕ only removes it from Favorites (the original is kept), so
  // it's a lighter, non-destructive action — no confirm needed there.
  async function confirmClose(imageId: number) {
    if (view?.favoritesView) {
      void closeImage(imageId);
      return;
    }
    const ok = await ask("Remove this image from the session? It won't be saved.", {
      title: "PinShot",
      kind: "warning",
    });
    if (ok) void closeImage(imageId);
  }

  if (!view) return <div className="pin empty" />;

  const single = view.mode === "single";

  // Collapsed thumbnail (all-mode only).
  if (collapsed && !single) {
    return (
      <div className="pin collapsed" onMouseDown={onDragStart} title="Click ⤢ to expand · drag to move">
        <img src={view.dataUrl} alt="pinned" style={{ opacity }} draggable={false} />
        <div className="chip-bar">
          <button className="ic" title="Expand" onClick={toggleCollapsed}>
            ⤢
          </button>
          <button
            className={`ic star${favorite ? " on" : ""}`}
            title={favorite ? "Remove from Favorites" : "Add to Favorites"}
            onClick={toggleFavorite}
          >
            {favorite ? "★" : "☆"}
          </button>
          <button className="ic" title={view.favoritesView ? "Remove from Favorites" : "Close"} onClick={() => void confirmClose(view.id)}>
            ✕
          </button>
        </div>
      </div>
    );
  }

  // Single mode: one fixed viewer rectangle. Header moves it, body pans,
  // ⌘+scroll zooms. Differently-sized images stay framed in the same spot.
  if (single) {
    return (
      <div
        className={`pin viewer${color ? " colored" : ""}`}
        style={color ? ({ ["--pc" as string]: color } as React.CSSProperties) : undefined}
      >
        {noteToast && (
          <div className={`note-toast${noteToast.ok ? " ok" : " err"}`}>{noteToast.text}</div>
        )}
        <div className="viewer-head" onMouseDown={onHeadDown}>
          {color && <span className="pin-color-dot" style={{ background: color }} />}
          {view.total > 1 && (
            <button className="vh-btn" title="Previous (←)" onClick={() => void deckStep(-1)}>
              ‹
            </button>
          )}
          <span className="vh-count">
            {view.index} / {view.total}
          </span>
          {view.total > 1 && (
            <button className="vh-btn" title="Next (→)" onClick={() => void deckStep(1)}>
              ›
            </button>
          )}
          <span className="vh-spacer" />
          <button
            className={`ic star${favorite ? " on" : ""}`}
            title={favorite ? "Remove from Favorites" : "Add to Favorites"}
            onClick={toggleFavorite}
          >
            {favorite ? "★" : "☆"}
          </button>
          <button className="ic" title="Reset view (fit)" onClick={resetView}>
            ⤢
          </button>
          <button
            className={`ic${showTools ? " on" : ""}`}
            title={showTools ? "Hide controls" : "Show controls"}
            onClick={() => setShowTools((v) => !v)}
          >
            ⚙
          </button>
          <button
            className="ic close"
            title={view.favoritesView ? "Remove from Favorites" : "Close pin"}
            onClick={() => void confirmClose(view.id)}
          >
            ✕
          </button>
        </div>

        <div
          className="viewer-body"
          ref={bodyRef}
          onMouseDown={onBodyDown}
          title="Drag to pan · ⌘+scroll to zoom"
        >
          <img
            src={view.dataUrl}
            alt="pinned"
            draggable={false}
            style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`, opacity }}
          />
        </div>

        {/* Per-image note + color row at the bottom of the rectangle. */}
        <div className="viewer-footer">
          <textarea
            className={`viewer-note${savedPulse ? " saved" : ""}`}
            value={note}
            placeholder="Add a note…  (Enter to save · Shift+Enter = new line)"
            spellCheck={false}
            onMouseDown={onNotePointerDown}
            onChange={(e) => onNoteChange(e.target.value)}
            onFocus={onNoteFocus}
            onBlur={onNoteBlur}
            onKeyDown={onNoteKeyDown}
          />
          {swatchRow}
        </div>

        {showTools && (
          <div className="toolbar open viewer-tools">
            <button className="ic" title="Zoom out (⌘+scroll)" onClick={() => zoomBy(1 / ZOOM_STEP)}>
              −
            </button>
            <span className="pct" title="Reset to fit" onClick={resetView}>
              {Math.round(zoom * 100)}%
            </span>
            <button className="ic" title="Zoom in (⌘+scroll)" onClick={() => zoomBy(ZOOM_STEP)}>
              +
            </button>
            <span className="sep" />
            <input
              className="opacity"
              type="range"
              min={OPACITY_MIN}
              max={1}
              step={0.05}
              value={opacity}
              title="Opacity"
              onChange={(e) => changeOpacity(parseFloat(e.target.value))}
            />
            <span className="sep" />
            <button
              className={`ic star${favorite ? " on" : ""}`}
              title={favorite ? "Remove from Favorites" : "Add to Favorites"}
              onClick={toggleFavorite}
            >
              {favorite ? "★" : "☆"}
            </button>
            <button
              className="ic"
              title="Replace with the current clipboard image"
              onClick={() => void replaceImage(view.id)}
            >
              ⟳
            </button>
            <button
              className={`ic${clickThrough ? " on" : ""}`}
              title="Click-through (mouse passes through). Press ⌥⌘C to turn off."
              onClick={toggleClickThrough}
            >
              👆
            </button>
          </div>
        )}
      </div>
    );
  }

  // All mode: each image is its own window, sized to the image.
  return (
    <div
      className={`pin${color ? " colored" : ""}`}
      style={color ? ({ ["--pc" as string]: color } as React.CSSProperties) : undefined}
      onMouseDown={onDragStart}
      onWheel={onWheel}
    >
      <img src={view.dataUrl} alt="pinned" style={{ opacity }} draggable={false} />

      {noteToast && (
        <div className={`note-toast${noteToast.ok ? " ok" : " err"}`}>{noteToast.text}</div>
      )}

      {color && <span className="pin-color-dot corner" style={{ background: color }} />}

      {/* Per-image note: a caption overlay at the bottom. Shown when there's a
          note, or while the toolbar is open (so an empty note is editable). */}
      {(note || showTools) && (
        <textarea
          className={`pin-note${savedPulse ? " saved" : ""}`}
          value={note}
          placeholder="Add a note…  (Enter to save)"
          spellCheck={false}
          onMouseDown={onNotePointerDown}
          onChange={(e) => onNoteChange(e.target.value)}
          onFocus={onNoteFocus}
          onBlur={onNoteBlur}
          onKeyDown={onNoteKeyDown}
        />
      )}

      {/* Top-right toggle: reveal/hide the toolbar so it doesn't cover content. */}
      <button
        className={`tools-toggle${showTools ? " on" : ""}`}
        title={showTools ? "Hide controls" : "Show controls"}
        onClick={() => setShowTools((v) => !v)}
      >
        ⚙
      </button>

      <div className={`toolbar${showTools ? " open" : ""}`}>
        <button className="ic" title="Collapse" onClick={toggleCollapsed}>
          ⤡
        </button>
        <button className="ic" title="Zoom out (scroll down)" onClick={() => applyZoom(scale / ZOOM_STEP)}>
          −
        </button>
        <span className="pct" title="Reset to fit" onClick={() => applyZoom(1)}>
          {Math.round(scale * 100)}%
        </span>
        <button className="ic" title="Zoom in (scroll up)" onClick={() => applyZoom(scale * ZOOM_STEP)}>
          +
        </button>

        <span className="sep" />

        <input
          className="opacity"
          type="range"
          min={OPACITY_MIN}
          max={1}
          step={0.05}
          value={opacity}
          title="Opacity"
          onChange={(e) => changeOpacity(parseFloat(e.target.value))}
        />

        <span className="sep" />

        {swatchRow}

        <span className="sep" />

        <button
          className={`ic star${favorite ? " on" : ""}`}
          title={favorite ? "Remove from Favorites" : "Add to Favorites"}
          onClick={toggleFavorite}
        >
          {favorite ? "★" : "☆"}
        </button>
        <button
          className="ic"
          title="Replace with the current clipboard image"
          onClick={() => void replaceImage(view.id)}
        >
          ⟳
        </button>
        <button
          className={`ic${clickThrough ? " on" : ""}`}
          title="Click-through (mouse passes through to the app below). Press ⌥⌘C to turn off."
          onClick={toggleClickThrough}
        >
          👆
        </button>
        <button className="ic close" title={view.favoritesView ? "Remove from Favorites" : "Close pin"} onClick={() => void confirmClose(view.id)}>
          ✕
        </button>
      </div>
    </div>
  );
}
