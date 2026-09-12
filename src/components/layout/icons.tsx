/**
 * Title-bar signs (PLAN 7.10).
 *
 * Inline SVG, not an icon font and not Lucide: Linux is WebKitGTK, and a
 * webfont or a Blink-only effect is how a row of buttons goes blank there.
 * Each icon is decorative — the button that holds it already has the name,
 * as `aria-label` and as a tooltip. Sixteen by sixteen, `currentColor`, so
 * they follow the theme the way the workspace badge's `▣` does.
 */

import type { ReactNode } from "react";

function Svg({ children }: { readonly children: ReactNode }) {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.4"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

/** A page with a folded corner: the files themselves, to read, not to edit. */
export function FilesIcon() {
  return (
    <Svg>
      <path d="M4 1.5h5.5L12.5 4.5v10h-8.5z" />
      <path d="M9.5 1.5v3h3" />
      <path d="M6 8h4.5" />
      <path d="M6 10.5h4.5" />
    </Svg>
  );
}

/** Three columns: the board is a read of status, not a spreadsheet. */
export function BoardIcon() {
  return (
    <Svg>
      <rect x="1.5" y="2.5" width="4" height="11" rx="0.8" />
      <rect x="6" y="2.5" width="4" height="11" rx="0.8" />
      <rect x="10.5" y="2.5" width="4" height="11" rx="0.8" />
    </Svg>
  );
}

/** A list: the audit log is lines, newest first. */
export function AuditIcon() {
  return (
    <Svg>
      <rect x="3" y="2" width="10" height="12" rx="1" />
      <path d="M5.5 5.5h5" />
      <path d="M5.5 8h5" />
      <path d="M5.5 10.5h3" />
    </Svg>
  );
}

/** Two sliders: settings is a page of controls, not a gear to decode. */
export function SettingsIcon() {
  return (
    <Svg>
      <path d="M2.5 5.5h11" />
      <circle cx="10.5" cy="5.5" r="1.4" fill="currentColor" stroke="none" />
      <path d="M2.5 10.5h11" />
      <circle cx="5.5" cy="10.5" r="1.4" fill="currentColor" stroke="none" />
    </Svg>
  );
}

/** Down into a shelf: hide is not close, and the shelf is the tray. */
export function HideIcon() {
  return (
    <Svg>
      <path d="M4 6.5 8 10.5 12 6.5" />
      <path d="M3 13h10" />
    </Svg>
  );
}

/** A cross: quit ends the process, the same sign the tray uses for it. */
export function QuitIcon() {
  return (
    <Svg>
      <path d="M4 4l8 8" />
      <path d="M12 4l-8 8" />
    </Svg>
  );
}

/** A folder: the title-bar path's neighbour, not a second way to open a project. */
export function FolderIcon() {
  return (
    <Svg>
      <path d="M2.5 5.5h4l1.2 1.5H13.5v6.5h-11V5.5z" />
      <path d="M2.5 5.5V4.2c0-.4.3-.7.7-.7h3.1L7.5 5.5" />
    </Svg>
  );
}
