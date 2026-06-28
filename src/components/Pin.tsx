import { useEffect, useRef, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { listen } from "@tauri-apps/api/event";
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
  deckStep,
  focusPin,
} from "../lib/ipc";

const win = getCurrentWebviewWindow();
const label = win.label;

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
  if ((e.target as HTMLElement).closest("button, input")) return;
  void win.startDragging();
}

export default function Pin() {
  const [view, setView] = useState<PinView | null>(null);
  // Live, locally-driven values (seeded from the view, persisted back to Rust).
  const [scale, setScale] = useState(1);
  const [opacity, setOpacity] = useState(1);
  const [collapsed, setCollapsed] = useState(false);
  const [clickThrough, setClickThrough] = useState(false);
  // Toolbar is hidden by default (it covered the image) — revealed by the
  // top-right ⚙ toggle instead of on hover.
  const [showTools, setShowTools] = useState(false);
  const idRef = useRef<number | null>(null);

  // Receive this window's view (render / cycle / replace) and on (re)mount.
  useEffect(() => {
    void getPinView(label).then((v) => v && setView(v));
    const un = listen<PinView>(`pin-view:${label}`, (e) => setView(e.payload));
    return () => {
      void un.then((f) => f());
    };
  }, []);

  // Seed local state whenever the shown image changes (cycle / replace / mode).
  useEffect(() => {
    if (!view) return;
    setScale(view.scale);
    setOpacity(view.opacity);
    setCollapsed(view.collapsed);
    setClickThrough(view.clickThrough);
    idRef.current = view.id;
  }, [view?.id, view?.dataUrl]);

  // Keep the native window sized to the current view. Collapse pins top-left;
  // zoom grows from center. (Position itself is owned by Rust.)
  useEffect(() => {
    if (!view) return;
    if (collapsed) {
      const ar = view.fitW / view.fitH;
      const [w, h] =
        ar >= 1 ? [COLLAPSED_MAX, COLLAPSED_MAX / ar] : [COLLAPSED_MAX * ar, COLLAPSED_MAX];
      void resizePin(label, w, h, false);
    } else {
      void resizePin(label, view.fitW * scale, view.fitH * scale, false);
    }
  }, [view, collapsed, scale]);

  // ← / → cycle the carousel while THIS pin window is focused. PinShot's panels
  // never grab focus globally (non-activating), so this only fires after you
  // click the pin — it never steals arrow keys from other apps. Single mode +
  // more than one image only.
  useEffect(() => {
    if (view?.mode !== "single" || (view?.total ?? 0) <= 1) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        void deckStep(-1);
      } else if (e.key === "ArrowRight") {
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

  if (!view) return <div className="pin empty" />;

  const single = view.mode === "single";

  if (collapsed) {
    return (
      <div className="pin collapsed" onMouseDown={onDragStart} title="Click ⤢ to expand · drag to move">
        <img src={view.dataUrl} alt="pinned" style={{ opacity }} draggable={false} />
        <div className="chip-bar">
          <button className="ic" title="Expand" onClick={toggleCollapsed}>
            ⤢
          </button>
          <button className="ic" title="Close" onClick={() => void closeImage(view.id)}>
            ✕
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="pin" onMouseDown={onDragStart} onWheel={onWheel}>
      <img src={view.dataUrl} alt="pinned" style={{ opacity }} draggable={false} />

      {/* Top-right toggle: reveal/hide the toolbar so it doesn't cover content. */}
      <button
        className={`tools-toggle${showTools ? " on" : ""}`}
        title={showTools ? "Hide controls" : "Show controls"}
        onClick={() => setShowTools((v) => !v)}
      >
        ⚙
      </button>

      {single && view.total > 1 && (
        <>
          <button className="nav prev" title="Previous (← or click)" onClick={() => void deckStep(-1)}>
            ‹
          </button>
          <button className="nav next" title="Next (→ or click)" onClick={() => void deckStep(1)}>
            ›
          </button>
        </>
      )}

      <div className={`toolbar${showTools ? " open" : ""}`}>
        {single && <span className="count">{view.index} / {view.total}</span>}
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
        <button className="ic close" title="Close pin" onClick={() => void closeImage(view.id)}>
          ✕
        </button>
      </div>
    </div>
  );
}
