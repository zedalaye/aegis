/**
 * A collapsible section of the rail.
 *
 * The whole heading is the toggle (the caret is decorative); an optional `+`
 * action is a sibling, not nested. Open state persists per section in
 * `localStorage`, with every access guarded, since it can throw.
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
  /** Shown on the heading even when collapsed (e.g. *drifted*, a count). */
  badge = null,
  /**
   * Trailing control on the heading — the "+" that adds a project or a
   * session. A sibling of the disclosure, not a child.
   */
  action = null,
  /**
   * Keep the body open even if the section was folded. The pending "name this
   * project" form has nowhere else to go; hiding it behind a caret would leave
   * a dialog the user already answered with no place to finish.
   */
  pinned = false,
}: {
  readonly id: string;
  readonly title: string;
  readonly className?: string;
  readonly children: ReactNode;
  readonly badge?: ReactNode;
  readonly action?: ReactNode;
  readonly pinned?: boolean;
}) {
  const [collapsed, setCollapsed] = useState(() => remembered(id));

  const toggle = useCallback(() => {
    if (pinned) {
      return;
    }
    setCollapsed((was) => {
      remember(id, !was);
      return !was;
    });
  }, [id, pinned]);

  const expand = useCallback(() => {
    setCollapsed((was) => {
      if (was) {
        remember(id, false);
      }
      return false;
    });
  }, [id]);

  const folded = collapsed && !pinned;
  const bodyId = `rail-${id}`;

  return (
    <section
      className={[
        "rail__section",
        folded ? "rail__section--collapsed" : "",
        className ?? "",
      ]
        .filter(Boolean)
        .join(" ")}
      aria-label={title}
    >
      <div className="rail__head">
        <button
          type="button"
          className="rail__toggle"
          aria-expanded={!folded}
          aria-controls={bodyId}
          onClick={toggle}
        >
          <span aria-hidden="true" className="rail__caret">
            {folded ? "▸" : "▾"}
          </span>
          <span className="sidebar__heading">{title}</span>
        </button>
        {badge}
        {action === null ? null : (
          // Capture so adding while folded opens the section first: a new
          // session that landed in a list nobody can see would look like the
          // click did nothing.
          <div className="rail__action" onClickCapture={expand}>
            {action}
          </div>
        )}
      </div>

      {/* Unmounted rather than hidden. These bodies are lists that follow
          stores, and one kept mounted behind `display: none` would go on
          re-rendering for a panel nobody can see. */}
      {folded ? null : (
        <div className="rail__body" id={bodyId}>
          {children}
        </div>
      )}
    </section>
  );
}
