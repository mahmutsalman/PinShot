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

  // Seed local state whenever the shown image changes (cycle / replace / mode).
  // Resetting zoom/pan here is the "reset to fit on switch" behavior.
  useEffect(() => {
    if (!view) return;
    setScale(view.scale);
    setOpacity(view.opacity);
    setCollapsed(view.collapsed);
    setClickThrough(view.clickThrough);
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

  // ← / → cycle the carousel while THIS pin window is focused (single mode, >1).
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

  // --- single-mode viewer interactions ---------------------------------------

  /** Drag the header to MOVE the viewer window. */
  function onHeadDown(e: React.MouseEvent) {
    if (e.button !== 0) return;
    void focusPin(label);
    if ((e.target as HTMLElement).closest("button, input")) return;
    void win.startDragging();
  }

  /** Drag the body to PAN the image inside the viewer. */
  function onBodyDown(e: React.MouseEvent) {
    if (e.button !== 0) return;
    void focusPin(label);
    if ((e.target as HTMLElement).closest("button, input")) return;
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
          <button className="ic" title="Close" onClick={() => void closeImage(view.id)}>
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
      <div className="pin viewer">
        <div className="viewer-head" onMouseDown={onHeadDown}>
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
          <button className="ic close" title="Close pin" onClick={() => void closeImage(view.id)}>
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
