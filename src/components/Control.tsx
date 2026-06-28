import { useEffect, useRef, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { listen } from "@tauri-apps/api/event";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  type DeckSummary,
  type SessionInfo,
  createPin,
  closeAllPins,
  toggleControl,
  quitApp,
  setMode,
  deckStep,
  focusPin,
  getDeckSummary,
  listSessions,
  createSession,
  switchSession,
  renameSession,
  deleteSession,
  revealPins,
  hidePins,
  arrangePins,
  resizePin,
} from "../lib/ipc";

const CONTROL_WIDTH = 232;
const MINI_WIDTH = 220;

/** Drag the panel except from real controls. Any primary click first grabs
 *  keyboard focus so ← / → reach the panel. */
function onDragStart(e: React.MouseEvent) {
  if (e.button !== 0) return;
  const w = getCurrentWebviewWindow();
  void focusPin(w.label);
  if ((e.target as HTMLElement).closest("button, input, select")) return;
  void w.startDragging();
}

const EMPTY: DeckSummary = {
  count: 0,
  mode: "all",
  current: 0,
  anyClickThrough: false,
  sessionId: 0,
  revealed: true,
  poolSize: 12,
};

export default function Control() {
  const [deck, setDeck] = useState<DeckSummary>(EMPTY);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [name, setName] = useState("");
  const [pane, setPane] = useState<"main" | "sessions">("main");
  const [mini, setMini] = useState(() => {
    try {
      return localStorage.getItem("pinshot.mini") === "1";
    } catch {
      return false;
    }
  });
  const [toast, setToast] = useState<string | null>(null);
  const editing = useRef(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const toastTimer = useRef<number | null>(null);

  // Remember collapsed/expanded across restarts.
  useEffect(() => {
    try {
      localStorage.setItem("pinshot.mini", mini ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, [mini]);

  // Confirmation toast on every successful paste (button, ⌥⌘V, or tray) — the
  // backend emits "pin-saved" with the new count.
  useEffect(() => {
    const un = listen<number>("pin-saved", (e) => {
      const n = e.payload;
      setToast(`✓ Saved to database (${n} pin${n === 1 ? "" : "s"})`);
      if (toastTimer.current) window.clearTimeout(toastTimer.current);
      toastTimer.current = window.setTimeout(() => setToast(null), 2200);
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  // Keep the native window exactly as tall as the panel content (so the bottom
  // hint is never clipped). A ResizeObserver re-fits on every content change —
  // pane switches, count changes, show/hide, etc.
  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const fit = () => {
      // Fixed width per mode (content flexes inside it); measure only height so
      // the window never clips the bar. Use scrollHeight to capture full content.
      const w = mini ? MINI_WIDTH : CONTROL_WIDTH;
      const h = Math.max(el.scrollHeight, Math.ceil(el.getBoundingClientRect().height));
      if (h > 0) void resizePin("control", w, h, false);
    };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(el);
    return () => ro.disconnect();
  }, [mini]);

  useEffect(() => {
    void getDeckSummary().then((s) => s && setDeck(s));
    void listSessions().then(setSessions);
    const unDeck = listen<DeckSummary>("deck-changed", (e) => setDeck(e.payload));
    const unSess = listen<SessionInfo[]>("sessions-changed", (e) => setSessions(e.payload));
    return () => {
      void unDeck.then((f) => f());
      void unSess.then((f) => f());
    };
  }, []);

  // Trust the live deck summary's sessionId for "which session is active" (it's
  // always fresh), falling back to the list's flag — so the panel can never show
  // the wrong active session even if the list is momentarily stale.
  const active = sessions.find((s) => s.id === deck.sessionId) ?? sessions.find((s) => s.active);

  // Whenever the active session changes, refresh the list so names/counts and
  // the active flag stay in sync with the backend.
  useEffect(() => {
    void listSessions().then(setSessions);
  }, [deck.sessionId]);

  async function confirmCloseAll() {
    const ok = await ask(
      `Close all ${deck.count} pin${deck.count === 1 ? "" : "s"} in "${active?.name ?? "this session"}"? This removes the images from the session.`,
      { title: "PinShot", kind: "warning" }
    );
    if (ok) void closeAllPins();
  }

  async function confirmDeleteSession(s: SessionInfo) {
    const ok = await ask(
      `Delete session "${s.name}" and its ${s.count} image${s.count === 1 ? "" : "s"}? This can't be undone.`,
      { title: "PinShot", kind: "warning" }
    );
    if (ok) void deleteSession(s.id);
  }

  // Keep the rename field synced with the active session — but not while the
  // user is actively typing in it.
  useEffect(() => {
    if (!editing.current) setName(active?.name ?? "");
  }, [active?.id, active?.name]);

  const showAll = deck.mode === "all";
  const single = deck.mode === "single";

  // ← / → cycle pins (Single mode, >1) and ESC hides pins — while the control
  // panel is focused.
  useEffect(() => {
    const canCycle = pane === "main" && single && deck.count > 1;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (deck.count > 0 && deck.revealed) {
          e.preventDefault();
          void hidePins();
        }
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
  }, [pane, single, deck.count, deck.revealed]);

  function commitName() {
    editing.current = false;
    const n = name.trim();
    if (active && n && n !== active.name) void renameSession(active.id, n);
  }

  const toastEl = toast ? <div className="toast">{toast}</div> : null;

  const titlebar = (
    <div className="titlebar">
      <span className="brand">📌 PinShot</span>
      <div className="titlebtns">
        <button className="ic" title="Collapse to mini bar" onClick={() => setMini(true)}>
          ⊟
        </button>
        <button className="ic" title="Hide (⌥⌘P)" onClick={() => void toggleControl()}>
          –
        </button>
        <button className="ic" title="Quit" onClick={() => void quitApp()}>
          ✕
        </button>
      </div>
    </div>
  );

  // --- Collapsed mini bar: just the two most-used actions, smaller, same colors.
  if (mini) {
    return (
      <div className="control mini" ref={rootRef} onMouseDown={onDragStart}>
        {toastEl}
        <div className="mini-row">
          <span className="mini-grip" title="PinShot — drag to move">
            📌
          </span>
          <button
            className="btn-paste mini-btn"
            title="Paste a new pin (⌥⌘V)"
            onClick={() => void createPin()}
          >
            📷 Paste
          </button>
          {deck.count > 0 && (
            <button
              className={`btn-visibility mini-btn${deck.revealed ? "" : " off"}`}
              title={deck.revealed ? "Hide pins" : "Show pins"}
              onClick={() => (deck.revealed ? void hidePins() : void revealPins())}
            >
              {deck.revealed ? "🙈" : `👁 ${deck.count}`}
            </button>
          )}
          <button className="ic" title="Expand panel" onClick={() => setMini(false)}>
            ⤢
          </button>
        </div>
      </div>
    );
  }

  // --- Sessions pane: a second "page" (no native popup — that doesn't render in
  // a transparent non-activating panel). Pick a session to switch to it. --------
  if (pane === "sessions") {
    return (
      <div className="control" ref={rootRef} onMouseDown={onDragStart}>
        {toastEl}
        {titlebar}
        <div className="pane-head">
          <span className="pane-title">Sessions</span>
          <button className="ic" title="Back" onClick={() => setPane("main")}>
            ✕
          </button>
        </div>
        <div className="session-list">
          {sessions.map((s) => (
            <div key={s.id} className={`session-item${s.active ? " on" : ""}`}>
              <button
                className="session-pick"
                title={`Switch to ${s.name}`}
                onClick={() => {
                  void switchSession(s.id);
                  setPane("main");
                }}
              >
                <span className="session-itemname">{s.name}</span>
                <span className="session-itemcount">{s.count}</span>
              </button>
              <button
                className="ic"
                title="Delete session"
                disabled={sessions.length <= 1}
                onClick={() => void confirmDeleteSession(s)}
              >
                🗑
              </button>
            </div>
          ))}
        </div>
        <button
          className="primary"
          onClick={() => {
            void createSession(`Session ${sessions.length + 1}`);
            setPane("main");
          }}
        >
          ＋ New session
        </button>
      </div>
    );
  }

  // --- Main pane ---------------------------------------------------------------
  return (
    <div className="control" ref={rootRef} onMouseDown={onDragStart}>
      {toastEl}
      {titlebar}

      {/* Session bar: open the list to switch; rename the active one inline.
          Images persist per session in SQLite. */}
      <div className="session-row">
        <button
          className="session-open"
          title="Switch session"
          onClick={() => setPane("sessions")}
        >
          <span className="session-openname">{active?.name ?? "Session"}</span>
          <span className="session-itemcount">{active?.count ?? 0}</span>
          <span className="caret">▾</span>
        </button>
        <button
          className="ic"
          title="New session"
          onClick={() => void createSession(`Session ${sessions.length + 1}`)}
        >
          ＋
        </button>
      </div>
      <input
        className="session-name"
        value={name}
        title="Rename session (Enter to save)"
        placeholder="Session name"
        onFocus={() => (editing.current = true)}
        onChange={(e) => setName(e.target.value)}
        onBlur={commitName}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
      />

      {/* The two most-used actions, color-coded and grouped: cyan = Paste,
          violet = visibility. */}
      <button className="btn-paste" title="Paste the clipboard image as a new pin (⌥⌘V)" onClick={() => void createPin()}>
        📷 Paste a new pin
      </button>

      {deck.count > 0 && (
        <button
          className={`btn-visibility${deck.revealed ? "" : " off"}`}
          title={deck.revealed ? "Hide the pins (kept in the session)" : "Show the pins on screen"}
          onClick={() => (deck.revealed ? void hidePins() : void revealPins())}
        >
          {deck.revealed ? "🙈 Hide pins" : `👁 Show ${deck.count} pin${deck.count === 1 ? "" : "s"}`}
        </button>
      )}

      <div className="divider" />

      {/* View mode: show every image at once, or one at a time + navigation. */}
      <div className="mode-toggle" title="Show every pin at once, or one at a time">
        <button className={showAll ? "on" : ""} onClick={() => void setMode(true)}>
          Show all
        </button>
        <button className={single ? "on" : ""} onClick={() => void setMode(false)}>
          Single
        </button>
      </div>

      {deck.count === 0 ? (
        <p className="empty">
          Copy a screenshot (⌃⇧⌘4), then click <b>Paste</b> or press <kbd>⌥⌘V</kbd>.
        </p>
      ) : single ? (
        <div className="nav-row">
          <button className="mini" title="Previous (←)" onClick={() => void deckStep(-1)}>
            ‹
          </button>
          <span className="count" title="Use ← / → to navigate when focused">
            {deck.current} / {deck.count}
          </span>
          <button className="mini" title="Next (→)" onClick={() => void deckStep(1)}>
            ›
          </button>
        </div>
      ) : (
        <p className="count-line">
          {deck.count} pin{deck.count === 1 ? "" : "s"}
          {deck.count > deck.poolSize && ` · showing ${deck.poolSize} (Single mode for all)`}
        </p>
      )}

      {/* Tidy every image onto the screen, sized so all are readable. */}
      {deck.count > 1 && (
        <button className="ghost" title="Lay all pins out on screen, no overlap" onClick={() => void arrangePins()}>
          ▦ Arrange all
        </button>
      )}

      {deck.count > 0 && (
        <button className="btn-danger" title="Permanently remove every image in this session" onClick={() => void confirmCloseAll()}>
          🗑 Close all ({deck.count})
        </button>
      )}

      <p className="hint">
        <kbd>⌥⌘V</kbd> pin · <kbd>⌥⌘C</kbd> click-through · <kbd>⌥⌘P</kbd> panel
      </p>
    </div>
  );
}
