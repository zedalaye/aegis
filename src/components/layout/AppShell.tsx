/**
 * The window's frame: header, project rail, and the work area beside it.
 *
 * Two responsibilities beyond layout.
 *
 * It owns the subscription to the runtime's event stream — one listener set
 * for the whole app, attached on mount and detached on unmount. Attaching per
 * component would mean a delta applied once per mounted listener. It also does
 * the one-off loads that several panes depend on: the projects, the provider
 * settings, and the identities.
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

import { useAgents } from "../../state/agents";
import { attachRoutineEvents, useRoutines } from "../../state/routines";
import { useProjects } from "../../state/projects";
import { attachApprovalEvents, useApprovals } from "../../state/approvals";
import { attachAuditEvents } from "../../state/audit";
import { attachBoardEvents, useBoard } from "../../state/board";
import { attachSessionEvents, useSessions } from "../../state/sessions";
import { attachSettingsEvents, useSettings } from "../../state/settings";
import { useSkills } from "../../state/skills";
import { useWorkspace } from "../../state/workspace";

import AuditDrawer from "../audit/AuditDrawer";
import BoardPanel from "../board/BoardPanel";
import ChatPane from "../chat/ChatPane";
import SettingsPanel from "../settings/SettingsPanel";
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

/**
 * The last failure from any store, dismissible, above the panes.
 *
 * The audit drawer is deliberately absent: a log that would not read is a fact
 * about that one panel, and the panel is on screen with a place to say it.
 */
function ErrorBanner() {
  const projectError = useProjects((s) => s.error);
  const sessionError = useSessions((s) => s.error);
  const approvalError = useApprovals((s) => s.error);
  const settingsError = useSettings((s) => s.error);
  const workspaceError = useWorkspace((s) => s.error);
  const agentError = useAgents((s) => s.error);
  const dismissProject = useProjects((s) => s.dismissError);
  const dismissSession = useSessions((s) => s.dismissError);
  const dismissApproval = useApprovals((s) => s.dismissError);
  const dismissSettings = useSettings((s) => s.dismissError);
  const dismissWorkspace = useWorkspace((s) => s.dismissError);
  const dismissAgents = useAgents((s) => s.dismissError);

  // The most recent one wins. Stacking banners pushes the thing the user was
  // looking at off the screen, and the later ones are usually a consequence of
  // the first. Approvals come first because a refused click is the one the user
  // is waiting on an answer to; settings next, because that panel is in front
  // of the user when it fails.
  const error =
    approvalError ??
    settingsError ??
    agentError ??
    workspaceError ??
    sessionError ??
    projectError;
  if (error === null || error === undefined) {
    return null;
  }

  return (
    <div className="banner" role="alert">
      <span className="banner__message">{error.message}</span>
      {/* The runtime already decides whether an identical retry could
          plausibly work — a timeout or a 503 can, a path outside the workspace
          never will. Saying so is the difference between a user retrying and a
          user guessing. */}
      {error.retryable ? (
        <span className="banner__hint">worth trying again</span>
      ) : null}
      <span className="banner__code">{error.code}</span>
      <button
        type="button"
        className="banner__dismiss"
        onClick={() => {
          dismissApproval();
          dismissSettings();
          dismissAgents();
          dismissWorkspace();
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
  const loadShared = useWorkspace((s) => s.loadFor);
  const loadSkills = useSkills((s) => s.loadFor);
  const resetSessions = useSessions((s) => s.reset);
  const sessionId = useSessions((s) => s.detail?.session.id ?? null);
  const syncApprovals = useApprovals((s) => s.syncFor);
  const loadSettings = useSettings((s) => s.load);
  const settingsOpen = useSettings((s) => s.open);
  const loadAgents = useAgents((s) => s.load);
  const loadRoutines = useRoutines((s) => s.load);
  const boardOpen = useBoard((s) => s.open);
  // The *set* of projects, as a value that only changes when one is added or
  // removed — not on every refetch, which hands back a new array each time.
  const projectIds = useProjects((s) =>
    s.projects.map((project) => project.id).join(" "),
  );

  // One load on mount. Under StrictMode this runs twice in development: the
  // only write it performs is re-stamping `last_opened_at` on the project it
  // just opened, which lands on the same project and leaves the same state, so
  // no guard is needed — and adding one would mask a real double-render later.
  useEffect(() => {
    void load();
    // Loaded on mount rather than when the panel is first opened: the title
    // bar has nothing to say about the provider yet, but a fresh install with
    // no key is a thing to know before the first message rather than after it
    // fails.
    void loadSettings();
    // Identities are loaded on mount too, and for a stronger reason than the
    // provider: the session picker in the rail and the badge in the chat header
    // both need them before anything is opened, and a session created without
    // the list would silently be created as the built-in identity.
    void loadAgents();
  }, [load, loadSettings, loadAgents]);

  // The routines are loaded on mount for the reason the identities are — the
  // clock is already running in the runtime whether or not this window is open,
  // and a panel that only learned what was scheduled when somebody opened
  // Settings would be the last place to find out a routine had paused itself.
  //
  // And they are re-measured whenever the set of projects changes, because a
  // routine belongs to one: deleting a project deletes its routines in the
  // runtime (`commands/project.rs`), and nothing announces that. Without this
  // the panel keeps drawing a clock that no longer exists — and one that cannot
  // even be opened, since the project it named is gone from the picker.
  useEffect(() => {
    void loadRoutines();
  }, [projectIds, loadRoutines]);

  // One listener set per store for the app. The attach is asynchronous, so the
  // cleanup has to wait for it rather than assume it has finished —
  // `subscribe` detaches anything that arrives after cancellation.
  useEffect(() => {
    const attaching = [
      attachSessionEvents(),
      attachApprovalEvents(),
      attachSettingsEvents(),
      attachAuditEvents(),
      attachRoutineEvents(),
      attachBoardEvents(),
    ];
    return () => {
      for (const pending of attaching) {
        void pending.then((detach) => detach());
      }
    };
  }, []);

  // The session store follows the open project, and so does the shared-file
  // panel — the convention is a fact about the folder, measured when the folder
  // changes rather than kept in step by hand.
  useEffect(() => {
    if (projectId === null) {
      resetSessions();
    } else {
      void loadSessions(projectId);
    }
    void loadShared(projectId);
    // And so does the skill catalog, for a reason the shared-file panel does
    // not have: half of it comes from the project's own `skills/` folder, and
    // the identity form offers those names as things to grant. `null` is not
    // "clear it" — the library half is still worth listing with nothing open.
    void loadSkills(projectId);
  }, [projectId, loadSessions, resetSessions, loadShared, loadSkills]);

  // And the approval queue follows the open session. Refetched rather than
  // carried over: a window that was closed while a turn was waiting missed the
  // event that raised the dialog, and the runtime is the only thing that knows
  // what is still answerable.
  useEffect(() => {
    void syncApprovals(sessionId);
  }, [sessionId, syncApprovals]);

  return (
    <div className="shell">
      <TitleBar />
      <ErrorBanner />
      <div className="shell__body">
        <Sidebar />
        <main className="shell__main">
          {/* Settings first: it is reachable with no project open, and a
              board is about one. The board then wins over the transcript,
              because opening it is a deliberate act and the chat is where the
              window returns when it is closed. */}
          {settingsOpen ? (
            <SettingsPanel />
          ) : boardOpen ? (
            <BoardPanel />
          ) : projectId === null ? (
            <NoProject />
          ) : (
            <ChatPane />
          )}
        </main>
        <AuditDrawer />
      </div>
    </div>
  );
}
