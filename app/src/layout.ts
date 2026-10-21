// Which pane goes where. Pure functions over the session list, so the window's
// arrangement can be reasoned about (and compared) without rendering anything.

import { flushSync } from "react-dom";
import type { SessionInfo } from "./api";
import { byUrgency } from "./format";

/** What the window is showing: every session, the current pane, the zoomed one. */
export interface View {
  list: SessionInfo[];
  current?: string;
  zoomed?: string;
}

export type Action =
  /** A fresh session list from the engine. `hold` keeps the current pane where it is. */
  | { type: "data"; list: SessionInfo[]; hold: boolean }
  | { type: "select"; id: string }
  | { type: "cycle"; by: number }
  | { type: "zoom"; id: string }
  | { type: "unzoom" };

export function reduceView(view: View, action: Action): View {
  switch (action.type) {
    case "data": {
      const { list } = action;
      const had = (id?: string) => id !== undefined && view.list.some((s) => s.id === id);
      const has = (id?: string) => id !== undefined && list.some((s) => s.id === id);

      // A current pane that has not arrived yet (just spawned) is kept; one
      // that was listed and has gone is replaced.
      let current = has(view.current) || !had(view.current) ? view.current : undefined;
      const zoomed = has(view.zoomed) ? view.zoomed : undefined;

      // The bell moves you to the pane that rang -- unless you are zoomed into
      // another one, typing at a prompt, or already looking at a blocked pane.
      const wasBlocked = new Set(view.list.filter(blocked).map((s) => s.id));
      const rang = list.filter((s) => blocked(s) && !wasBlocked.has(s.id));
      const currentBlocked = list.some((s) => s.id === current && blocked(s));
      if (rang.length > 0 && !zoomed && !action.hold && !currentBlocked) {
        current = byUrgency(rang)[0].id;
      }

      if (current === undefined && list.length > 0) current = list[0].id;
      return { list, current, zoomed };
    }
    case "select":
      // Selecting another pane while zoomed moves the zoom with it, as tmux does.
      return { ...view, current: action.id, zoomed: view.zoomed ? action.id : undefined };
    case "cycle": {
      const n = view.list.length;
      if (n === 0) return view;
      const at = view.list.findIndex((s) => s.id === view.current);
      const id = view.list[(((at + action.by) % n) + n) % n].id;
      return reduceView(view, { type: "select", id });
    }
    case "zoom":
      return { ...view, current: action.id, zoomed: action.id };
    case "unzoom":
      return view.zoomed ? { ...view, zoomed: undefined } : view;
  }
}

export function blocked(s: SessionInfo): boolean {
  return s.status === "needs_you";
}

/**
 * The arrangement, named after the tmux layouts it copies.
 *
 * - `tiled`: nothing needs you, so every pane gets an even share.
 * - `main-vertical`: something does, and it takes the left 60%. Need decides size.
 * - `zoomed`: one pane fills the window, at reading size.
 */
export type Layout =
  | { kind: "empty" }
  | { kind: "zoomed"; pane: SessionInfo }
  | { kind: "tiled"; rows: SessionInfo[][] }
  | { kind: "main-vertical"; main: SessionInfo; rows: SessionInfo[][] };

export function layout(view: View): Layout {
  const { list } = view;
  if (list.length === 0) return { kind: "empty" };

  const zoomed = list.find((s) => s.id === view.zoomed);
  if (zoomed) return { kind: "zoomed", pane: zoomed };

  const waiting = list.filter(blocked);
  if (waiting.length > 0 && list.length > 1) {
    // The current pane keeps the main tile if it is blocked; otherwise it goes
    // to whichever has waited longest.
    const main = waiting.find((s) => s.id === view.current) ?? byUrgency(waiting)[0];
    const rest = list.filter((s) => s.id !== main.id);
    return { kind: "main-vertical", main, rows: rowsOf(rest, rest.length > 4 ? 2 : 1) };
  }

  return { kind: "tiled", rows: rowsOf(list, Math.ceil(Math.sqrt(list.length))) };
}

/** Panes in creation order, `cols` to a row; a short last row is stretched. */
function rowsOf(panes: SessionInfo[], cols: number): SessionInfo[][] {
  const rows: SessionInfo[][] = [];
  for (let i = 0; i < panes.length; i += cols) rows.push(panes.slice(i, i + cols));
  return rows;
}

/**
 * Everything about a layout that moves a pane: which layout, which pane is main
 * or zoomed, and the order. Ages and screen contents are deliberately absent --
 * when only those change, the window is redrawn in place, not morphed.
 */
export function layoutKey(view: View): string {
  const l = layout(view);
  switch (l.kind) {
    case "empty":
      return "empty";
    case "zoomed":
      return `zoomed:${l.pane.id}`;
    case "tiled":
      return `tiled:${l.rows.map((r) => r.map((s) => s.id).join(",")).join("/")}`;
    case "main-vertical":
      return `main:${l.main.id}:${l.rows.map((r) => r.map((s) => s.id).join(",")).join("/")}`;
  }
}

/**
 * For buttons that act on a pane: a click must not leave them focused, or the
 * next bare Enter would press the button again instead of zooming. Keyboard
 * users still reach them with Tab.
 */
export function keepFocus(e: { preventDefault: () => void }) {
  e.preventDefault();
}

/**
 * Apply a state change as a view transition: the zoom morph, and the bell pane
 * swelling into the main tile. Each pane carries its own transition name, so
 * the browser animates each from its old box to its new one. Skipped when the
 * system asks for reduced motion, or where view transitions do not exist.
 */
export function morph(update: () => void) {
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (reduce || typeof document.startViewTransition !== "function") {
    update();
    return;
  }
  document.startViewTransition(() => flushSync(update));
}
