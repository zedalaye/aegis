/**
 * A collapsible section of the rail.
 *
 * The rail stacks four things — projects, shared files, the world, sessions —
 * and on a folder with a world laid down that is more than fits on a laptop
 * screen at once. Which of them matters depends entirely on what somebody is
 * doing: the world is the thing to watch while founding one and noise for the
 * next month, and the project list is the reverse.
 *
 * So each carries its own disclosure, and the choice sticks. It is a heading
 * that is also a button rather than a separate caret to hit — the whole row is
 * the target, which is what makes it usable in a rail this narrow — and the
 * caret is decorative, because the button already announces its state.
 *
 * What is remembered is one boolean per section, in `localStorage`. It is a
 * preference about a window, worth nothing to anybody else, and losing it costs
 * one click — so it does not go near the stores that hold the user's actual
 * data, and every access is guarded: a WebView with site data blocked throws
 * on the property itself, and a rail that would not render because of that
 * would be a rail broken by a setting that has nothing to do with it.
 */

import { useCallback, useState } from "react";
import type { ReactNode } from "react";

/** Where a section's disclosure is remembered, namespaced to this app. */
function storageKey(id: string): string {
  return `aegis.rail.${id}.collapsed`;
}

/** What was remembered for `id`, or `false` when nothing was. */
function remembered(id: string): boolean {
  try {
    return window.localStorage.getItem(storageKey(id)) === "1";
  } catch {
    // Storage can be unavailable outright — private windows, blocked site
    // data. Open is the honest default: a section nobody has collapsed.
    return false;
  }
}

/** Remembers `collapsed` for `id`, or gives up quietly. */
function remember(id: string, collapsed: boolean): void {
  try {
    window.localStorage.setItem(storageKey(id), collapsed ? "1" : "0");
  } catch {
    // The disclosure still works for this window; it just will not survive a
    // restart. Not worth a line in the shell.
  }
}

export default function Section({
  id,
  title,
  className,
  children,
  /**
   * Drawn on the heading whether or not the section is open.
   *
   * For the one thing a collapsed section still has to be able to say — the
   * world's *drifted*, a count — so that collapsing it does not hide the reason
   * somebody would want to open it.
   */
  badge = null,
}: {
  readonly id: string;
  readonly title: string;
  readonly className?: string;
  readonly children: ReactNode;
  readonly badge?: ReactNode;
}) {
  const [collapsed, setCollapsed] = useState(() => remembered(id));

  const toggle = useCallback(() => {
    setCollapsed((was) => {
      remember(id, !was);
      return !was;
    });
  }, [id]);

  const bodyId = `rail-${id}`;

  return (
    <section
      className={[
        "rail__section",
        collapsed ? "rail__section--collapsed" : "",
        className ?? "",
      ]
        .filter(Boolean)
        .join(" ")}
      aria-label={title}
    >
      <button
        type="button"
        className="rail__toggle"
        aria-expanded={!collapsed}
        aria-controls={bodyId}
        onClick={toggle}
      >
        <span aria-hidden="true" className="rail__caret">
          {collapsed ? "▸" : "▾"}
        </span>
        <span className="sidebar__heading">{title}</span>
        {badge}
      </button>

      {/* Unmounted rather than hidden. These bodies are lists that follow
          stores, and one kept mounted behind `display: none` would go on
          re-rendering for a panel nobody can see. */}
      {collapsed ? null : (
        <div className="rail__body" id={bodyId}>
          {children}
        </div>
      )}
    </section>
  );
}
