/**
 * One row of the session list.
 *
 * The title is editable in place: renaming is a rare action, and a dialog for
 * it would be more chrome than the action deserves. Double-click, or the
 * rename button, turns the label into an input; Enter commits, Escape
 * abandons, and blur commits — losing an edit because the user clicked
 * elsewhere is the more annoying of the two failure modes.
 */

import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";

import type { SessionSummary } from "../../ipc/bindings";

/** What each live state says, and how the badge reads. */
const STATE_LABEL: Record<SessionSummary["state"], string | null> = {
  idle: null,
  running: "running",
  awaiting_approval: "waiting on you",
  error: "failed",
};

export default function SessionItem({
  session,
  open,
  onOpen,
  onRename,
  onRemove,
}: {
  readonly session: SessionSummary;
  readonly open: boolean;
  readonly onOpen: () => void;
  readonly onRename: (title: string) => void;
  readonly onRemove: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(session.title);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (editing) {
      inputRef.current?.select();
    }
  }, [editing]);

  const commit = () => {
    setEditing(false);
    const title = draft.trim();
    if (title !== "" && title !== session.title) {
      onRename(title);
    } else {
      setDraft(session.title);
    }
  };

  const abandon = () => {
    setEditing(false);
    setDraft(session.title);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter") {
      event.preventDefault();
      commit();
    } else if (event.key === "Escape") {
      event.preventDefault();
      abandon();
    }
  };

  const badge = STATE_LABEL[session.state];
  const delegated = session.delegated !== null;

  if (editing) {
    return (
      <li className="session session--editing">
        <input
          ref={inputRef}
          className="session__input"
          value={draft}
          aria-label={`Rename ${session.title}`}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onKeyDown}
          onBlur={commit}
        />
      </li>
    );
  }

  return (
    <li
      className={`session${open ? " session--open" : ""}${
        delegated ? " session--delegated" : ""
      }`}
    >
      <button
        type="button"
        className="session__open"
        aria-current={open ? "true" : undefined}
        onClick={onOpen}
        onDoubleClick={() => setEditing(true)}
      >
        <span className="session__title">{session.title}</span>
        <span className="session__meta">
          {delegated ? (
            <span
              className="session__badge session__badge--delegated"
              title="Opened by a brief from another session. It ran under its own identity."
            >
              brief
            </span>
          ) : null}
          {session.message_count === 0
            ? "empty"
            : `${session.message_count} message${session.message_count === 1 ? "" : "s"}`}
          {badge === null ? null : (
            <span className={`session__badge session__badge--${session.state}`}>
              {badge}
            </span>
          )}
        </span>
      </button>

      <button
        type="button"
        className="session__action"
        title={`Rename ${session.title}`}
        aria-label={`Rename session ${session.title}`}
        onClick={() => setEditing(true)}
      >
        ✎
      </button>
      <button
        type="button"
        className="session__action"
        title={`Delete ${session.title} and its transcript`}
        aria-label={`Delete session ${session.title}`}
        onClick={onRemove}
      >
        ×
      </button>
    </li>
  );
}
