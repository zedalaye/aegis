/**
 * The window's frame: header, project rail, and the work area beside it.
 *
 * Two responsibilities beyond layout.
 *
 * It owns the subscription to the runtime's event stream — one listener set
 * for the whole app, attached on mount and detached on unmount. Attaching per
 * component would mean a delta applied once per mounted listener.
 *
 * And it keeps the session store following the open project. The two stores
 * are deliberately separate — a project is a folder, a session is a
 * conversation — so something has to notice when one changes and reload the
 * other. That something is here, where both are already in scope.
 *
 * Errors surface once, above everything. A failure in either store is not
 * local to the control that triggered it, and this is the only place both the
 * sidebar and the work area can be seen to be affected by it.
 */

import { useEffect } from "react";

import { useProjects } from "../../state/projects";
import { attachSessionEvents, useSessions } from "../../state/sessions";

import ChatPane from "../chat/ChatPane";
import Sidebar from "./Sidebar";
import TitleBar from "./TitleBar";

/**
 * What fills the work area before there is a project to chat in.
 *
 * Once one is open the pane belongs to the conversation; the project's own
 * facts are the workspace path and the name, and both are already in the title
 * bar where they stay visible while the transcript scrolls.
 */
function NoProject() {
  const status = useProjects((s) => s.status);

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

/** The last failure from either store, dismissible, above the panes. */
function ErrorBanner() {
  const projectError = useProjects((s) => s.error);
  const sessionError = useSessions((s) => s.error);
  const dismissProject = useProjects((s) => s.dismissError);
  const dismissSession = useSessions((s) => s.dismissError);

  // The most recent one wins. Stacking two banners pushes the thing the user
  // was looking at off the screen, and the second is usually a consequence of
  // the first.
  const error = sessionError ?? projectError;
  if (error === null || error === undefined) {
    return null;
  }

  return (
    <div className="banner" role="alert">
      <span className="banner__message">{error.message}</span>
      <span className="banner__code">{error.code}</span>
      <button
        type="button"
        className="banner__dismiss"
        onClick={() => {
          dismissSession();
          dismissProject();
        }}
        aria-label="Dismiss this error"
      >
        ×
      </button>
    </div>
  );
}

export default function AppShell() {
  const load = useProjects((s) => s.load);
  const projectId = useProjects((s) => s.detail?.project.id ?? null);
  const loadSessions = useSessions((s) => s.loadFor);
  const resetSessions = useSessions((s) => s.reset);

  // One load on mount. Under StrictMode this runs twice in development: the
  // only write it performs is re-stamping `last_opened_at` on the project it
  // just opened, which lands on the same project and leaves the same state, so
  // no guard is needed — and adding one would mask a real double-render later.
  useEffect(() => {
    void load();
  }, [load]);

  // One listener set for the app. The attach is asynchronous, so the cleanup
  // has to wait for it rather than assume it has finished — `subscribe`
  // detaches anything that arrives after cancellation.
  useEffect(() => {
    const pending = attachSessionEvents();
    return () => {
      void pending.then((detach) => detach());
    };
  }, []);

  // The session store follows the open project.
  useEffect(() => {
    if (projectId === null) {
      resetSessions();
    } else {
      void loadSessions(projectId);
    }
  }, [projectId, loadSessions, resetSessions]);

  return (
    <div className="shell">
      <TitleBar />
      <ErrorBanner />
      <div className="shell__body">
        <Sidebar />
        <main className="shell__main">
          {projectId === null ? <NoProject /> : <ChatPane />}
        </main>
      </div>
    </div>
  );
}
