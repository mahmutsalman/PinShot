import { useEffect, useRef, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { listen } from "@tauri-apps/api/event";
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
} from "../lib/ipc";

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
};

export default function Control() {
  const [deck, setDeck] = useState<DeckSummary>(EMPTY);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [name, setName] = useState("");
  const editing = useRef(false);

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

  const active = sessions.find((s) => s.active);

  // Keep the rename field synced with the active session — but not while the
  // user is actively typing in it.
  useEffect(() => {
    if (!editing.current) setName(active?.name ?? "");
  }, [active?.id, active?.name]);

  const showAll = deck.mode === "all";
  const single = deck.mode === "single";

  // ← / → cycle pins while the control panel is focused (Single mode, >1 pin).
  // Scoped to this focused window → never captures arrow keys from other apps.
  useEffect(() => {
    if (!single || deck.count <= 1) return;
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
  }, [single, deck.count]);

  function commitName() {
    editing.current = false;
    const n = name.trim();
    if (active && n && n !== active.name) void renameSession(active.id, n);
  }

  return (
    <div className="control" onMouseDown={onDragStart}>
      <div className="titlebar">
        <span className="brand">📌 PinShot</span>
        <div className="titlebtns">
          <button className="ic" title="Hide (⌥⌘P)" onClick={() => void toggleControl()}>
            –
          </button>
          <button className="ic" title="Quit" onClick={() => void quitApp()}>
            ✕
          </button>
        </div>
      </div>

      {/* Sessions: pasted images persist per session in SQLite. Switch / rename
          / new / delete. Switching shows that session's pins immediately. */}
      <div className="session-row">
        <select
          className="session-select"
          title="Switch session"
          value={active?.id ?? ""}
          onChange={(e) => void switchSession(Number(e.target.value))}
        >
          {sessions.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name} ({s.count})
            </option>
          ))}
        </select>
        <button
          className="ic"
          title="New session"
          onClick={() => void createSession(`Session ${sessions.length + 1}`)}
        >
          ＋
        </button>
        <button
          className="ic"
          title="Delete this session"
          disabled={sessions.length <= 1}
          onClick={() => active && void deleteSession(active.id)}
        >
          🗑
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

      <button className="primary" onClick={() => void createPin()}>
        📷 Paste a new pin
      </button>

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
        </p>
      )}

      {deck.count > 0 && !deck.revealed && (
        <button className="primary" onClick={() => void revealPins()}>
          👁 Show {deck.count} pin{deck.count === 1 ? "" : "s"}
        </button>
      )}

      {deck.count > 0 && (
        <button className="ghost" onClick={() => void closeAllPins()}>
          Close all ({deck.count})
        </button>
      )}

      <p className="hint">
        <kbd>⌥⌘V</kbd> pin · <kbd>⌥⌘C</kbd> click-through · <kbd>⌥⌘P</kbd> panel
      </p>
    </div>
  );
}
