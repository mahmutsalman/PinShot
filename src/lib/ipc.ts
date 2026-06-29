import { invoke } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";

/** Full render payload for one pin window (matches Rust `PinView`). */
export interface PinView {
  id: number;
  dataUrl: string;
  width: number;
  height: number;
  /** Logical size the image was fitted to — the 100% zoom baseline. */
  fitW: number;
  fitH: number;
  scale: number;
  opacity: number;
  collapsed: boolean;
  mode: "all" | "single";
  index: number; // 1-based position in the deck
  total: number;
  clickThrough: boolean;
  /** Starred for the cross-session Favorites view. */
  favorite: boolean;
  /** This pin belongs to the aggregated Favorites view. */
  favoritesView: boolean;
  /** Per-image free-text note (persisted). */
  note: string;
  /** Per-image color tag — a preset hex string, or "" for none. */
  color: string;
}

export interface DeckSummary {
  count: number;
  mode: "all" | "single";
  current: number; // 1-based, 0 when empty
  anyClickThrough: boolean;
  sessionId: number;
  /** false right after launch — pins are loaded but hidden until revealed. */
  revealed: boolean;
  /** How many images "show all" can display at once (window-pool size). */
  poolSize: number;
  /** The active session is the cross-session Favorites view. */
  favoritesView: boolean;
}

export interface SessionInfo {
  id: number;
  name: string;
  count: number;
  active: boolean;
  /** Unix seconds of last use (paste/switch) — for the mini-bar recent list. */
  lastUsed: number;
  /** Pinned to the mini-bar quick list regardless of recency. */
  starred: boolean;
  /** The special always-present session aggregating favorited images. */
  isFavorites: boolean;
}

/** Run a command, surfacing any error in a native dialog (alerts are no-ops in
 *  the non-activating panel). Returns undefined on failure. */
export async function safeInvoke<T>(
  cmd: string,
  args?: Record<string, unknown>
): Promise<T | undefined> {
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    await message(String(err), { kind: "warning", title: "PinShot" });
    return undefined;
  }
}

const quiet = (cmd: string, args?: Record<string, unknown>) =>
  invoke(cmd, args).catch(() => {});

// control / deck
export const createPin = () => safeInvoke<number>("create_pin");
export const closeAllPins = () => safeInvoke<void>("close_all_pins");
export const toggleControl = () => safeInvoke<void>("toggle_control");
export const quitApp = () => safeInvoke<void>("quit_app");
export const setMode = (all: boolean) => quiet("set_mode", { all });
export const deckStep = (delta: number) => quiet("deck_step", { delta });
/** Make this window the key window so ← / → reach it (deterministic focus). */
export const focusPin = (label: string) => quiet("focus_pin", { label });
export const getDeckSummary = () =>
  invoke<DeckSummary>("get_deck_summary").catch(() => null);

// sessions
export const listSessions = () =>
  invoke<SessionInfo[]>("list_sessions").catch(() => [] as SessionInfo[]);
export const createSession = (name: string) =>
  safeInvoke<number>("create_session", { name });
export const switchSession = (id: number) => quiet("switch_session", { id });
export const renameSession = (id: number, name: string) =>
  quiet("rename_session", { id, name });
export const setSessionStarred = (id: number, starred: boolean) =>
  quiet("set_session_starred", { id, starred });
export const deleteSession = (id: number) => quiet("delete_session", { id });
export const revealPins = () => quiet("reveal_pins");
export const hidePins = () => quiet("hide_pins");
export const arrangePins = () => quiet("arrange_pins");

// per-window / per-image
export const getPinView = (label: string) =>
  invoke<PinView | null>("get_pin_view", { label }).catch(() => null);
export const replaceImage = (id: number) => safeInvoke<void>("replace_image", { id });
export const closeImage = (id: number) => quiet("close_image", { id });
export const setImagePos = (id: number, x: number, y: number) =>
  quiet("set_image_pos", { id, x, y });
export const setImageScale = (id: number, scale: number) =>
  quiet("set_image_scale", { id, scale });
export const setImageOpacity = (id: number, opacity: number) =>
  quiet("set_image_opacity", { id, opacity });
export const setImageCollapsed = (id: number, collapsed: boolean) =>
  quiet("set_image_collapsed", { id, collapsed });
export const setImageClickThrough = (id: number, ignore: boolean) =>
  quiet("set_image_click_through", { id, ignore });
export const setImageFavorite = (id: number, favorite: boolean) =>
  quiet("set_image_favorite", { id, favorite });
/** Background (debounced / on-blur) note save — errors swallowed. */
export const setImageNote = (id: number, note: string) =>
  quiet("set_image_note", { id, note });
/** Explicit note save (Enter) — rejects on failure so the UI can confirm/alert. */
export const saveImageNote = (id: number, note: string) =>
  invoke<void>("set_image_note", { id, note });
export const setImageColor = (id: number, color: string) =>
  quiet("set_image_color", { id, color });
/** Tell the backend a text field is focused so arrow/ESC keys aren't hijacked. */
export const setTextEditing = (editing: boolean) =>
  quiet("set_text_editing", { editing });
export const resizePin = (label: string, width: number, height: number, center: boolean) =>
  quiet("resize_pin", { label, width, height, center });
