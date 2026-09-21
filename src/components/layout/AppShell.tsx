/**
 * The window's frame: header, project rail, and the work area beside it.
 *
 * Also owns the app's single set of event listeners and shared initial loads,
 * reloads sessions when the open project changes, and shows one error banner.
 */

import { useEffect } from "react";

import { useAgents } from "../../state/agents";
import { attachConnectorEvents } from "../../state/connectors";
import { attachRoutineEvents, useRoutines } from "../../state/routines";
import { useProjects } from "../../state/projects";
import { attachApprovalEvents, useApprovals } from "../../state/approvals";
import { attachAuditEvents } from "../../state/audit";
import { attachBoardEvents, useBoard } from "../../state/board";
import { attachExplorerEvents, useExplorer } from "../../state/explorer";
import { useHosts } from "../../state/hosts";
import { attachProjectDropEvents } from "../../state/intake";
import { attachSessionEvents, useSessions } from "../../state/sessions";
import { attachSettingsEvents, useSettings } from "../../state/settings";
import { useSkills } from "../../state/skills";
import { attachWorkspaceEvents, useWorkspace } from "../../state/workspace";

import AuditDrawer from "../audit/AuditDrawer";
import BoardPanel from "../board/BoardPanel";
import ChatPane from "../chat/ChatPane";
import ExplorerPanel from "../explorer/ExplorerPanel";
import SettingsPanel from "../settings/SettingsPanel";
import Sidebar from "./Sidebar";
import TitleBar from "./TitleBar";

/** What fills the work area before a project is open. */
function NoProject() {
  const status = useProjects((s) => s.status);

  return (
    <section className="work work--empty">
      <h1 className="work__title">
        {status === "loading" ? "Loading projects…" : "No project open"}
      </h1>
      <p className="work__body">
        A project is a workspace folder Aegis is allowed to work in. Add one
        with the + next to Projects.
      </p>
    </section>
  );
}

/**
 * The last failure from any store, dismissible. The audit drawer reports its
 * own errors.
 */
function ErrorBanner() {
  const projectError = useProjects((s) => s.error);
  const sessionError = useSessions((s) => s.error);
  const approvalError = useApprovals((s) => s.error);
  // An answer to a parked ask is an approval given late (PLAN 7.22), and it
  // can be refused — by the routine's door, or by a run that is already
  // going. The board is where that is read.
  const boardError = useBoard((s) => s.error);
  const settingsError = useSettings((s) => s.error);
  const workspaceError = useWorkspace((s) => s.error);
  const hostError = useHosts((s) => s.error);
  const agentError = useAgents((s) => s.error);
  // A tree that would not load. A file that would not preview is the pane's
  // to say, and never reaches this.
  const explorerError = useExplorer((s) => s.error);
  const dismissProject = useProjects((s) => s.dismissError);
  const dismissSession = useSessions((s) => s.dismissError);
  const dismissApproval = useApprovals((s) => s.dismissError);
  const dismissBoard = useBoard((s) => s.dismissError);
  const dismissSettings = useSettings((s) => s.dismissError);
  const dismissWorkspace = useWorkspace((s) => s.dismissError);
  const dismissHosts = useHosts((s) => s.dismissError);
  const dismissAgents = useAgents((s) => s.dismissError);
  const dismissExplorer = useExplorer((s) => s.dismissError);

  // One banner, never stacked; approvals first — including one answered
  // late from the board — then settings.
  const error =
    approvalError ??
    boardError ??
    settingsError ??
    agentError ??
    hostError ??
    explorerError ??
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
          dismissBoard();
          dismissSettings();
          dismissAgents();
          dismissHosts();
          dismissExplorer();
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
  const recountParked = useBoard((s) => s.recount);
  const filesOpen = useExplorer((s) => s.open);
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

  // Routines load on mount and whenever the project set changes: deleting a
  // project silently deletes its routines (`commands/project.rs`).
  useEffect(() => {
    void loadRoutines();
  }, [projectIds, loadRoutines]);

  // What is parked is counted for the open project whether or not the board is
  // showing, because the title bar is where a person learns there is something
  // to answer (PLAN 7.22).
  useEffect(() => {
    void recountParked();
  }, [projectId, recountParked]);

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
      attachConnectorEvents(),
      attachBoardEvents(),
      attachWorkspaceEvents(),
      attachExplorerEvents(),
      attachProjectDropEvents(),
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
              board is about one. The board and Files then win over the
              transcript, because opening either is a deliberate act and the
              chat is where the window returns when it is closed. */}
          {settingsOpen ? (
            <SettingsPanel />
          ) : boardOpen ? (
            <BoardPanel />
          ) : filesOpen ? (
            <ExplorerPanel />
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
