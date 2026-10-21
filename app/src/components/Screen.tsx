import type { CSSProperties } from "react";
import type { ScreenView } from "../api";

interface Props {
  view: ScreenView;
  /** Draw the terminal cursor. Only the current pane has one, as in tmux. */
  cursor: boolean;
}

/**
 * An agent's emulated screen, cell for cell: every row, blank ones included,
 * at whatever size makes the whole grid fit its pane (see `.screen` in the
 * CSS). A tile shows the full screen in miniature; zoomed, the same grid comes
 * up to reading size. Nothing is cropped or reflowed, because the screen is the
 * evidence for the status above it.
 */
export function Screen({ view, cursor }: Props) {
  const size = { "--cols": view.cols, "--rows": view.rows } as CSSProperties;
  const rows = Array.from({ length: view.rows }, (_, r) => view.lines[r] ?? "");

  return (
    <div className="screen-fit">
      <pre className="screen" style={size} aria-label="Screen">
        {rows.map((line, r) => (
          <div className="screen-row" key={r}>
            {cursor && r === view.cursor_row ? withCursor(line, view.cursor_col) : line}
          </div>
        ))}
      </pre>
    </div>
  );
}

/** Split a row around the cursor cell, padding if the cursor sits past the text. */
function withCursor(line: string, col: number) {
  // Code points, not UTF-16 units, so a character outside the BMP is one cell.
  const cells = Array.from(line);
  while (cells.length <= col) cells.push(" ");
  return (
    <>
      {cells.slice(0, col).join("")}
      <span className="cursor">{cells[col]}</span>
      {cells.slice(col + 1).join("")}
    </>
  );
}
