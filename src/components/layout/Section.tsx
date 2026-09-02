/**
 * A collapsible section of the rail.
 *
 * The rail stacks four things — projects, shared files, the world, sessions —
 * and on a folder with a world laid down that is more than fits on a laptop
 * screen at once. Which of them matters depends entirely on what somebody is
 * doing: the world is the thing to watch while founding one and noise for the
 * next month, and the project list is the reverse.
 *
 * So each carries its own disclosure, and the choice sticks. The heading is a
 * button rather than a separate caret to hit — the title is the target, which
 * is what makes it usable in a rail this narrow — and the caret is decorative,
 * because the button already announces its state. An optional action (`+`)
 * sits at the end of the row as a sibling: a button nested in the disclosure
 * would be invalid HTML and would fold the section when it meant to add.
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
