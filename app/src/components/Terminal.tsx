import { useEffect, useRef, type MutableRefObject } from "react";
import { Terminal as XTerm, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api, type WatchMessage } from "../api";

/** Campbell, as Windows Terminal draws it, so an agent's colours match the window around them. */
const CAMPBELL: ITheme = {
  background: "#0c0c0c",
  foreground: "#cccccc",
  cursor: "#cccccc",
  cursorAccent: "#0c0c0c",
  selectionBackground: "#cccccc",
  selectionForeground: "#0c0c0c",
  black: "#0c0c0c",
  red: "#c50f1f",
  green: "#13a10e",
  yellow: "#c19c00",
  blue: "#0037da",
  magenta: "#881798",
  cyan: "#3a96dd",
  white: "#cccccc",
  brightBlack: "#767676",
  brightRed: "#e74856",
  brightGreen: "#16c60c",
  brightYellow: "#f9f1a5",
  brightBlue: "#3b78ff",
  brightMagenta: "#b4009e",
  brightCyan: "#61d6d6",
  brightWhite: "#f2f2f2",
};

const FONT_FAMILY = '"Argus Mono", "Cascadia Mono", Consolas, monospace';
const FONT_SIZE = 14;
/** After the stream breaks without the session ending, try again this soon. */
const RETRY_MS = 1000;

/** What the rest of the window can ask of the terminal. */
export interface TerminalControl {
  /** Put the keyboard in the terminal. Set while one is mounted. */
  focus?: () => void;
  /** Focus as soon as the terminal is ready, for "zoom and type" in one key. */
  wantFocus: boolean;
}

interface Props {
  id: string;
  control: MutableRefObject<TerminalControl>;
  /** The keyboard went into the terminal, or left it. */
  onTyping: (typing: boolean) => void;
  /** The prefix key was pressed, or the key after it was. */
  onPrefix: (armed: boolean) => void;
  /** The key pressed after the prefix. */
  onCommand: (key: string) => void;
  onError: (error: unknown) => void;
}

/**
 * A session's real terminal, live and typeable: the zoomed pane's view.
 *
 * It starts from the engine's repaint and then writes every byte the agent
 * prints, so it shows exactly what the agent drew -- colours, cursor, full
 * redraws -- rather than the text-only screen the tiles poll for. The
 * session's own terminal is resized to fit, the way tmux sizes a window to
 * its client.
 *
 * Everything typed goes to the agent, Esc and Ctrl+C included, so getting
 * back to Argus needs a key no agent uses. That is tmux's answer too: a
 * prefix, Ctrl+B, then a command key -- `d` to stop typing, `z` to tile back.
 * Ctrl+B twice sends a real Ctrl+B.
 */
export function Terminal({ id, control, onTyping, onPrefix, onCommand, onError }: Props) {
  const host = useRef<HTMLDivElement>(null);
  // The terminal is built once per session, but these change every render.
  const callbacks = useRef({ onTyping, onPrefix, onCommand, onError });
  callbacks.current = { onTyping, onPrefix, onCommand, onError };

  useEffect(() => {
    const element = host.current;
    if (!element) return;

    let disposed = false;
    let watch: number | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let refit: ReturnType<typeof setTimeout> | undefined;

    const term = new XTerm({
      fontFamily: FONT_FAMILY,
      fontSize: FONT_SIZE,
      theme: CAMPBELL,
      cursorBlink: false,
      cursorInactiveStyle: "outline",
      drawBoldTextInBrightColors: false,
      scrollback: 2000,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);

    // ---- Typing ------------------------------------------------------------
    //
    // One request at a time, in order. Each keystroke is its own engine call,
    // and two calls in flight at once could arrive the wrong way round. While
    // one is in flight, plain text queues up and is sent together; anything
    // with a control character in it -- Enter, an arrow, a paste -- is sent on
    // its own, so a TUI never reads Enter as part of a paste and swallows it.
    const queue: string[] = [];
    let sending = false;
    const plain = (text: string) => !/[\x00-\x1f\x7f]/.test(text);
    const type = (data: string) => {
      const last = queue[queue.length - 1];
      if (last !== undefined && plain(last) && plain(data)) queue[queue.length - 1] = last + data;
      else queue.push(data);
      void pump();
    };
    const pump = async () => {
      if (sending) return;
      sending = true;
      while (queue.length > 0 && !disposed) {
        const text = queue.shift()!;
        try {
          await api.sendText(id, text);
        } catch (error) {
          queue.length = 0;
          callbacks.current.onError(error);
        }
      }
      sending = false;
    };
    term.onData(type);

    // ---- Keys Argus keeps for itself --------------------------------------
    let armed = false;
    const disarm = () => {
      if (!armed) return;
      armed = false;
      callbacks.current.onPrefix(false);
    };
    term.attachCustomKeyEventHandler((event) => {
      if (event.type !== "keydown") return !armed;
      const ctrl = event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
      const key = event.key.toLowerCase();

      if (armed) {
        if (["control", "shift", "alt", "meta"].includes(key)) return false;
        event.preventDefault();
        disarm();
        if (ctrl && key === "b") type("\x02");
        else callbacks.current.onCommand(event.key);
        return false;
      }
      if (ctrl && key === "b") {
        event.preventDefault();
        armed = true;
        callbacks.current.onPrefix(true);
        return false;
      }
      // Windows Terminal's habits: Ctrl+C copies when something is selected
      // and is a real Ctrl+C otherwise; Ctrl+V is left to the browser's paste,
      // which the terminal then types (bracketed, if the agent asked for that).
      if (ctrl && key === "c" && term.hasSelection()) {
        event.preventDefault();
        navigator.clipboard.writeText(term.getSelection()).catch(() => {});
        term.clearSelection();
        return false;
      }
      if (ctrl && key === "v") return false;
      return true;
    });

    // ---- Size ---------------------------------------------------------------
    let cols = 0;
    let rows = 0;
    const fitToPane = () => {
      if (disposed) return;
      const proposed = fit.proposeDimensions();
      if (!proposed || proposed.cols < 2 || proposed.rows < 2) return;
      fit.fit();
      if (term.cols === cols && term.rows === rows) return;
      cols = term.cols;
      rows = term.rows;
      api.resize(id, cols, rows).catch((error) => callbacks.current.onError(error));
    };
    const observer = new ResizeObserver(() => {
      clearTimeout(refit);
      refit = setTimeout(fitToPane, 120);
    });

    // ---- The stream ---------------------------------------------------------
    const onMessage = (message: WatchMessage) => {
      if (disposed) return;
      switch (message.event) {
        case "repaint":
          term.reset();
          term.write(message.data);
          break;
        case "output":
          term.write(message.data);
          break;
        case "ended":
          // The pane switches to the session's last screen once the list
          // says it is done; nothing to do here.
          break;
        case "disconnected":
          retry = setTimeout(start, RETRY_MS);
          break;
      }
    };
    const start = async () => {
      try {
        const n = await api.watch(id, onMessage);
        if (disposed) void api.unwatch(n);
        else watch = n;
      } catch {
        // Usually the engine restarting, or the session ending as this pane
        // opened. Either the list catches up and this pane goes, or a retry
        // gets through.
        if (!disposed) retry = setTimeout(start, RETRY_MS);
      }
    };

    void (async () => {
      // xterm measures its cell size once, from the font it is given, so the
      // font has to be there first or every column is the wrong width.
      await document.fonts.load(`${FONT_SIZE}px "Argus Mono"`).catch(() => {});
      if (disposed) return;
      term.open(element);
      term.textarea?.addEventListener("focus", () => callbacks.current.onTyping(true));
      term.textarea?.addEventListener("blur", () => {
        disarm();
        callbacks.current.onTyping(false);
      });
      // Sized before watching, so the first repaint is already at this size.
      fitToPane();
      observer.observe(element);
      control.current.focus = () => term.focus();
      if (control.current.wantFocus) {
        control.current.wantFocus = false;
        term.focus();
      }
      await start();
    })();

    return () => {
      disposed = true;
      observer.disconnect();
      clearTimeout(retry);
      clearTimeout(refit);
      if (watch !== undefined) void api.unwatch(watch).catch(() => {});
      control.current.focus = undefined;
      callbacks.current.onPrefix(false);
      callbacks.current.onTyping(false);
      term.dispose();
    };
  }, [id, control]);

  return (
    <div
      ref={host}
      className="terminal-host"
      // A double-click in a terminal selects a word; it must not tile the pane.
      onDoubleClick={(event) => event.stopPropagation()}
    />
  );
}
