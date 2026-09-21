/**
 * What one run changed, from its checkpoint, and the button that takes it
 * back (PLAN 7.24).
 *
 * Fetched when the run is opened, never with the board: reading it runs `git`.
 * Restore asks once more before it acts, and says afterwards what it put back,
 * what it removed and what it left alone because somebody changed it since.
 */

import { useCallback, useEffect, useState } from "react";

import type {
  ChangeKind,
  RunCheckpoint as Checkpoint,
  RunRestored,
} from "../../ipc/bindings";
import { boardCheckpoint, boardRestore } from "../../ipc/commands";
import { formatTimestamp } from "../../lib/format";

/** How each change reads, as a word and as the one-letter mark beside it. */
const KIND: Record<ChangeKind, { word: string; mark: string }> = {
  added: { word: "created", mark: "A" },
  modified: { word: "changed", mark: "M" },
  deleted: { word: "removed", mark: "D" },
};

type Read =
  | { readonly state: "loading" }
  | { readonly state: "none" }
  | { readonly state: "failed"; readonly message: string }
  | { readonly state: "ready"; readonly checkpoint: Checkpoint };

function count(n: number, one: string): string {
  return `${n} ${one}${n === 1 ? "" : "s"}`;
}

/** The sentence a finished restore leaves behind. */
function restoredLine(done: RunRestored): string {
  const parts = [
    done.restored.length > 0 ? `${count(done.restored.length, "path")} put back` : "",
    done.removed.length > 0 ? `${count(done.removed.length, "file")} it created removed` : "",
  ].filter((part) => part.length > 0);
  const said = parts.length > 0 ? `${parts.join(", ")}.` : "Nothing needed putting back.";
  return done.kept.length > 0
    ? `${said} Left alone, because they changed after the run: ${done.kept.join(", ")}.`
    : said;
}

export default function RunCheckpoint({
  projectId,
  sessionId,
  explainAbsence,
}: {
  readonly projectId: string;
  /** The run: the session it ran in. */
  readonly sessionId: string;
  /** Whether a run with no checkpoint says so, or shows nothing. */
  readonly explainAbsence: boolean;
}) {
  const [read, setRead] = useState<Read>({ state: "loading" });
  const [confirming, setConfirming] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [outcome, setOutcome] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const checkpoint = await boardCheckpoint(projectId, sessionId);
      setRead(checkpoint === null ? { state: "none" } : { state: "ready", checkpoint });
    } catch (err) {
      setRead({ state: "failed", message: err instanceof Error ? err.message : String(err) });
    }
  }, [projectId, sessionId]);

  useEffect(() => {
    void load();
  }, [load]);

  const restore = async () => {
    setRestoring(true);
    try {
      setOutcome(restoredLine(await boardRestore(projectId, sessionId)));
    } catch (err) {
      setOutcome(err instanceof Error ? err.message : String(err));
    } finally {
      setRestoring(false);
      setConfirming(false);
      void load();
    }
  };

  if (read.state === "loading") {
    return <p className="checkpoint__note">Reading its checkpoint…</p>;
  }
  if (read.state === "failed") {
    return <p className="checkpoint__note">{read.message}</p>;
  }
  if (read.state === "none") {
    return explainAbsence ? (
      <p className="checkpoint__note">
        No checkpoint: the workspace is not a git work tree, the identity could
        not write, or the run is older than what is kept.
      </p>
    ) : null;
  }

  const { checkpoint } = read;
  const finished = checkpoint.after_at.length > 0;

  return (
    <div className="checkpoint">
      <p className="checkpoint__note">
        Snapshot taken {formatTimestamp(checkpoint.before_at)}
        {finished ? <>, and again {formatTimestamp(checkpoint.after_at)}</> : null}
        . Files git ignores are not captured.
      </p>

      {!finished ? (
        <p className="checkpoint__note">
          The run has not finished, so there is nothing to take back yet.
        </p>
      ) : checkpoint.changes.length === 0 ? (
        <p className="checkpoint__note">It changed nothing git can see.</p>
      ) : (
        <>
          <ul className="checkpoint__changes">
            {checkpoint.changes.map((change) => (
              <li
                key={change.path}
                className={`checkpoint__change checkpoint__change--${change.kind}`}
              >
                <span className="checkpoint__mark" title={KIND[change.kind].word}>
                  {KIND[change.kind].mark}
                </span>
                <code>{change.path}</code>
              </li>
            ))}
          </ul>

          <details className="checkpoint__diff">
            <summary>Show the diff</summary>
            <pre className="checkpoint__patch">{checkpoint.patch}</pre>
            {checkpoint.truncated ? (
              <p className="checkpoint__note">
                The diff is cut short here; the file list above is whole.
              </p>
            ) : null}
          </details>

          <div className="checkpoint__actions">
            {confirming ? (
              <>
                <span className="checkpoint__ask">
                  Put back {count(checkpoint.changes.length, "path")} as they
                  were before this run? Anything changed since is left alone.
                </span>
                <button
                  type="button"
                  className="button"
                  disabled={restoring}
                  onClick={() => setConfirming(false)}
                >
                  Cancel
                </button>
                <button
                  type="button"
                  className="button button--danger"
                  disabled={restoring}
                  onClick={() => void restore()}
                >
                  {restoring ? "Restoring…" : "Restore"}
                </button>
              </>
            ) : (
              <button
                type="button"
                className="button"
                title="Writes the files back as they were before this run. Your index, HEAD and branches are not touched."
                onClick={() => {
                  setOutcome(null);
                  setConfirming(true);
                }}
              >
                Restore…
              </button>
            )}
          </div>
        </>
      )}

      {outcome !== null ? (
        <p className="checkpoint__note" role="status">
          {outcome}
        </p>
      ) : null}
    </div>
  );
}
