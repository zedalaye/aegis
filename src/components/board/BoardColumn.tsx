/**
 * One column of the board: attention, in flight, or blocked.
 *
 * Lines interleave file and runtime sources, each with a source badge. An
 * empty column shows only its name (PLAN 7.2 row 9).
 */

import type { BoardItem, BoardSource } from "../../ipc/bindings";
import { formatTimeOfDay, formatTimestamp } from "../../lib/format";

/** What each source is called on the badge. */
const SOURCE: Record<BoardSource, string> = {
  status: "STATUS.md",
  approval: "waiting",
  session: "session",
  routine: "routine",
  run: "run",
  parked: "parked",
};

export default function BoardColumn({
  title,
  note,
  items,
  onOpenSession,
  onTrace,
}: {
  readonly title: string;
  /** One line saying what belongs in this column. */
  readonly note: string;
  readonly items: readonly BoardItem[];
  /** Opens the conversation a line is about. */
  readonly onOpenSession: (sessionId: string) => void;
  /** Opens the replay of the run a line is about. */
  readonly onTrace: (item: BoardItem) => void;
}) {
  return (
    <section className="boardcol">
      <h3 className="boardcol__title">
        {title}
        <span className="boardcol__count">{items.length}</span>
      </h3>
      <p className="boardcol__note">{note}</p>

      {items.length === 0 ? (
        <p className="boardcol__empty">—</p>
      ) : (
        <ul className="boardcol__list">
          {items.map((item, index) => {
            const clickable = item.run !== null || item.session_id.length > 0;
            const body = (
              <>
                <span className="boarditem__text">{item.text}</span>
                {item.detail.length > 0 ? (
                  <span className="boarditem__detail">{item.detail}</span>
                ) : null}
                <span className="boarditem__meta">
                  <span
                    className={`boarditem__source boarditem__source--${item.source}`}
                  >
                    {SOURCE[item.source]}
                  </span>
                  {item.at.length > 0 ? (
                    <time
                      className="boarditem__time"
                      dateTime={item.at}
                      title={formatTimestamp(item.at)}
                    >
                      {formatTimeOfDay(item.at)}
                    </time>
                  ) : null}
                </span>
              </>
            );

            return (
              // The index is part of the key on purpose: two lines of a
              // hand-written `STATUS.md` can read identically, and a file is
              // allowed to repeat itself.
              <li className="boarditem" key={`${item.source}-${index}-${item.text}`}>
                {clickable ? (
                  <button
                    type="button"
                    className="boarditem__open"
                    onClick={() => {
                      if (item.run !== null) {
                        onTrace(item);
                      } else {
                        onOpenSession(item.session_id);
                      }
                    }}
                  >
                    {body}
                  </button>
                ) : (
                  <div className="boarditem__plain">{body}</div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
