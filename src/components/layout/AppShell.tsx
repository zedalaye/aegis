/**
 * The window's frame: header, project rail, and the work area beside it.
 *
 * The work area holds the open project's detail for now and the chat panel
 * from Phase 5. Errors surface here, once, above everything — a failure in the
 * project store is not local to the control that triggered it, and the shell
 * is the only place both the sidebar and the work area can be seen to be
 * affected by it.
 */

import { useEffect } from "react";

import { useProjects } from "../../state/projects";
import { formatOptionalTimestamp, formatTimestamp } from "../../lib/format";

import Sidebar from "./Sidebar";
import TitleBar from "./TitleBar";

/** The open project, or the reason there is nothing to show. */
function WorkArea() {
  const detail = useProjects((s) => s.detail);
  const status = useProjects((s) => s.status);

  if (detail === null) {
    return (
      <section className="work work--empty">
        <h1 className="work__title">
          {status === "loading" ? "Loading projects…" : "No project open"}
        </h1>
        <p className="work__body">
          A project is a workspace folder Aegis is allowed to work in. Add one
          from the sidebar to get started.
        </p>
      </section>
    );
  }

  const { project, sessions } = detail;

  return (
    <section className="work">
      <h1 className="work__title">{project.name}</h1>

      <dl className="facts">
        <dt>Workspace</dt>
        <dd className="facts__path" title={project.workspace_path}>
          {project.workspace_path}
        </dd>

        <dt>Added</dt>
        <dd>{formatTimestamp(project.created_at)}</dd>

        <dt>Last opened</dt>
        <dd>{formatOptionalTimestamp(project.last_opened_at)}</dd>
      </dl>

      {project.workspace_exists ? null : (
        <p className="work__warning" role="status">
          This folder is not there any more. It may be on a drive that is not
          mounted, or it may have been moved or renamed. The project is kept so
          you can find it again; nothing can run inside it until the folder is
          back.
        </p>
      )}

      <h2 className="work__subtitle">Sessions</h2>
      {sessions.length === 0 ? (
        <p className="work__body">
          No sessions yet. Chat, the agent loop and the approval gate arrive in
          later phases — see <code>PLAN.md</code> § 6.
        </p>
      ) : (
        <ul>
          {sessions.map((session) => (
            <li key={session.id}>{session.title}</li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** The last failure, dismissible, above the panes it affected. */
function ErrorBanner() {
  const error = useProjects((s) => s.error);
  const dismissError = useProjects((s) => s.dismissError);

  if (error === null) {
    return null;
  }

  return (
    <div className="banner" role="alert">
      <span className="banner__message">{error.message}</span>
      <span className="banner__code">{error.code}</span>
      <button
        type="button"
        className="banner__dismiss"
        onClick={dismissError}
        aria-label="Dismiss this error"
      >
        ×
      </button>
    </div>
  );
}

export default function AppShell() {
  const load = useProjects((s) => s.load);

  // One load on mount. Under StrictMode this runs twice in development: the
  // only write it performs is re-stamping `last_opened_at` on the project it
  // just opened, which lands on the same project and leaves the same state, so
  // no guard is needed — and adding one would mask a real double-render later.
  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="shell">
      <TitleBar />
      <ErrorBanner />
      <div className="shell__body">
        <Sidebar />
        <main className="shell__main">
          <WorkArea />
        </main>
      </div>
    </div>
  );
}
