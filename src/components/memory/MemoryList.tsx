/**
 * What an identity has learned (PLAN 7.3, Phase 14; `COS.md` *Memory*).
 *
 * Under Identities in Settings, one identity at a time. The only place to
 * forget a memory. Rows show kind, text, source (none reads as a hypothesis)
 * and last confirmation.
 */

import { useEffect, useState } from "react";

import type { Memory, MemoryDraft, MemoryKind } from "../../ipc/bindings";
import { DEFAULT_AGENT_ID, useAgents } from "../../state/agents";
import { useMemories } from "../../state/memories";
import { formatTimestamp } from "../../lib/format";

/** What each kind is for, in the fewest words that distinguish it. */
const KIND_SUMMARY: Record<MemoryKind, string> = {
  preference: "how someone likes things done",
  exception: "where the usual rule does not apply",
  convention: "how it is done here",
};

const KINDS: readonly MemoryKind[] = [
  "preference",
  "exception",
  "convention",
];

/** An empty form. */
function blankDraft(): MemoryDraft {
  return { kind: "convention", text: "", source: null };
}

/** The refusal that belongs under `field`, if the last save produced one. */
function useFieldError(field: string): string | null {
  const error = useMemories((s) => s.error);
  return error?.field === field ? error.message : null;
}

/** One memory. */
function Row({
  memory,
  agentId,
}: {
  readonly memory: Memory;
  readonly agentId: string;
}) {
  const forget = useMemories((s) => s.forget);
  const [confirming, setConfirming] = useState(false);

  return (
    <li className="memory">
      <div className="memory__head">
        <span className={`memory__kind memory__kind--${memory.kind}`}>
          {memory.kind}
        </span>
        <time className="memory__at" dateTime={memory.updated_at}>
          {formatTimestamp(memory.updated_at)}
        </time>
      </div>

      <p className="memory__text">{memory.text}</p>

      {memory.source === null ? (
        <p className="memory__source memory__source--none">
          Nothing behind it, so it is read as a hypothesis.
        </p>
      ) : (
        <p className="memory__source" title={memory.source}>
          {memory.source}
        </p>
      )}

      <div className="memory__actions">
        {confirming ? (
          <>
            <span className="memory__confirm">Forget this? There is no undo.</span>
            <button
              type="button"
              className="link"
              onClick={() => {
                setConfirming(false);
                void forget(agentId, memory.id);
              }}
            >
              Forget
            </button>
            <button
              type="button"
              className="link"
              onClick={() => setConfirming(false)}
            >
              Keep
            </button>
          </>
        ) : (
          <button
            type="button"
            className="link"
            // Said on the control: this store is the only copy, and a row that
            // vanished on one click with an undo nobody notices is worse than
            // a second click.
            title="Removes this memory. The identity stops carrying it into new turns."
            onClick={() => setConfirming(true)}
          >
            Forget
          </button>
        )}
      </div>
    </li>
  );
}

/** The form that records a new memory, on the user's own account. */
function Compose({ agentId }: { readonly agentId: string }) {
  const save = useMemories((s) => s.save);
  const [draft, setDraft] = useState<MemoryDraft>(blankDraft);
  const [open, setOpen] = useState(false);

  const textError = useFieldError("text");
  const sourceError = useFieldError("source");

  if (!open) {
    return (
      <button
        type="button"
        className="button"
        onClick={() => {
          setDraft(blankDraft());
          setOpen(true);
        }}
      >
        Add a memory
      </button>
    );
  }

  return (
    <form
      className="memory__form"
      onSubmit={(event) => {
        event.preventDefault();
        void save(agentId, null, draft).then((saved) => {
          if (saved) {
            setDraft(blankDraft());
            setOpen(false);
          }
        });
      }}
    >
      <div className="field">
        <span className="field__label">Kind</span>
        <div className="memory__kinds">
          {KINDS.map((kind) => (
            <label className="memory__kindChoice" key={kind}>
              <input
                type="radio"
                name="memory-kind"
                value={kind}
                checked={draft.kind === kind}
                onChange={() => setDraft({ ...draft, kind })}
              />
              <span>
                <code>{kind}</code> — {KIND_SUMMARY[kind]}
              </span>
            </label>
          ))}
        </div>
      </div>

      <div className="field">
        <label className="field__label" htmlFor="memory-text">
          What to remember
        </label>
        <textarea
          id="memory-text"
          className="field__input"
          rows={2}
          value={draft.text}
          aria-invalid={textError !== null}
          onChange={(event) => setDraft({ ...draft, text: event.target.value })}
          placeholder="This client wants everything in French."
        />
        <p className="field__hint">
          One sentence, written so it still makes sense months from now with no
          conversation around it.
        </p>
        {textError === null ? null : (
          <p className="field__error" role="alert">
            {textError}
          </p>
        )}
      </div>

      <div className="field">
        <label className="field__label" htmlFor="memory-source">
          What it rests on <span className="field__optional">optional</span>
        </label>
        <input
          id="memory-source"
          className="field__input"
          value={draft.source ?? ""}
          aria-invalid={sourceError !== null}
          onChange={(event) =>
            setDraft({
              ...draft,
              source: event.target.value.length === 0 ? null : event.target.value,
            })
          }
          placeholder="decisions/DECISIONS.md"
        />
        <p className="field__hint">
          A path, a ticket, or who said it. A memory with nothing behind it is
          shown to the model as a hypothesis.
        </p>
        {sourceError === null ? null : (
          <p className="field__error" role="alert">
            {sourceError}
          </p>
        )}
      </div>

      <div className="memory__formActions">
        <button type="submit" className="button button--primary">
          Remember it
        </button>
        <button
          type="button"
          className="button"
          onClick={() => setOpen(false)}
        >
          Cancel
        </button>
      </div>
    </form>
  );
}

export default function MemoryList() {
  const agents = useAgents((s) => s.agents);
  const agentId = useMemories((s) => s.agentId);
  const memories = useMemories((s) => s.memories);
  const status = useMemories((s) => s.status);
  const loadFor = useMemories((s) => s.loadFor);

  // The built-in identity by default: it is what a session gets when nobody
  // chooses, so it is the identity most likely to have learned something.
  const selected = agentId ?? DEFAULT_AGENT_ID;

  useEffect(() => {
    if (agentId === null) {
      void loadFor(DEFAULT_AGENT_ID);
    }
  }, [agentId, loadFor]);

  return (
    <>
      <p className="settings__note">
        What an identity has learned: a <code>preference</code>, an{" "}
        <code>exception</code>, or a <code>convention</code>. These reach the
        top of every request that identity makes, so they are the things worth
        not having to say twice — not facts about a project, which belong in a
        workspace file, and not procedures, which belong in a skill. The model
        can record one, and you are asked before it does. It cannot delete one:
        correcting a memory is yours, and this is where it happens.
      </p>

      <div className="memory__picker">
        <label className="field__label" htmlFor="memory-agent">
          Identity
        </label>
        <select
          id="memory-agent"
          className="field__input"
          value={selected}
          onChange={(event) => void loadFor(event.target.value)}
        >
          {agents.map((agent) => (
            <option key={agent.id} value={agent.id}>
              {agent.name}
            </option>
          ))}
        </select>
      </div>

      {status === "loading" && memories.length === 0 ? (
        <p className="settings__note">Reading what it knows…</p>
      ) : memories.length === 0 ? (
        <p className="settings__note">
          This identity has not learned anything yet. It will remember something
          when you approve a <code>memory_write</code>, or when you add one
          here.
        </p>
      ) : (
        <ul className="memory__list">
          {memories.map((memory) => (
            <Row key={memory.id} memory={memory} agentId={selected} />
          ))}
        </ul>
      )}

      <div className="memory__foot">
        <Compose agentId={selected} />
      </div>
    </>
  );
}
