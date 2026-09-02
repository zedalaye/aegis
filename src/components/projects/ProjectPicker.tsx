/**
 * Naming a project after a folder has been picked.
 *
 * The "+" on the Projects heading opens the native dialog; this form is the
 * second step, shown only once a folder is chosen. The name is the only part
 * of a project the user can choose, so it is offered before anything is
 * written — there is no way to rename a project afterwards.
 *
 * Draws nothing when idle: the heading already holds the action.
 */

import { useProjects } from "../../state/projects";

import WorkspaceBadge from "./WorkspaceBadge";

export default function ProjectPicker() {
  const pending = useProjects((s) => s.pending);
  const busy = useProjects((s) => s.busy);
  const renamePending = useProjects((s) => s.renamePending);
  const cancelPending = useProjects((s) => s.cancelPending);
  const confirmPending = useProjects((s) => s.confirmPending);

  if (pending === null) {
    return null;
  }

  // A folder with an all-whitespace name would be stored under the folder's
  // own name by the runtime; refusing to submit it is clearer than silently
  // substituting something the user did not type.
  const nameIsUsable = pending.name.trim().length > 0;

  return (
    <form
      className="picker"
      onSubmit={(event) => {
        event.preventDefault();
        void confirmPending();
      }}
    >
      <WorkspaceBadge path={pending.path} exists maxLength={32} />

      <label className="picker__label" htmlFor="project-name">
        Project name
      </label>
      <input
        id="project-name"
        className="picker__input"
        value={pending.name}
        onChange={(event) => renamePending(event.target.value)}
        disabled={busy}
        autoFocus
        spellCheck={false}
      />

      <div className="picker__actions">
        <button
          type="submit"
          className="button button--primary"
          disabled={busy || !nameIsUsable}
        >
          Add project
        </button>
        <button
          type="button"
          className="button"
          onClick={cancelPending}
          disabled={busy}
        >
          Cancel
        </button>
      </div>
    </form>
  );
}
