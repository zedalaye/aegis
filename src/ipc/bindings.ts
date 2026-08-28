/**
 * IPC payload types — the shapes Rust sends across the boundary.
 *
 * Hand-written for now, field for field against the structs in
 * `src-tauri/src/store.rs` and the contract in `PLAN.md` § 2.1. Phase 5
 * replaces this file with `ts-rs` output generated from those same structs;
 * until then, changing a payload in Rust means changing it here in the same
 * commit. A Rust test asserts the serialized field names, so drift shows up as
 * a failing test rather than as `undefined` in the UI.
 *
 * Field names are `snake_case` because that is what crosses the wire.
 */

/** A project: a workspace folder, a name, and when it was last used. */
export type Project = {
  /** UUID v4, stable for the life of the project. */
  readonly id: string;
  /** Display name. Defaults to the workspace folder's own name. */
  readonly name: string;
  /** Canonical absolute path, free of Windows verbatim (`\\?\`) prefixes. */
  readonly workspace_path: string;
  /** RFC3339, UTC. */
  readonly created_at: string;
  /** RFC3339, UTC. `null` until the project has been opened once. */
  readonly last_opened_at: string | null;
  /**
   * Whether the workspace folder is present right now.
   *
   * Measured on every read rather than stored, so it is honest about folders
   * on removable drives, network shares and paths the user has since moved.
   */
  readonly workspace_exists: boolean;
};

/** Lifecycle of a session. Meaningful from Phase 5. */
export type SessionState = "idle" | "running" | "awaiting_approval" | "error";

/** One row of a project's session list. */
export type SessionSummary = {
  readonly id: string;
  readonly project_id: string;
  readonly title: string;
  readonly created_at: string;
  readonly updated_at: string;
  readonly message_count: number;
  readonly state: SessionState;
};

/**
 * What opening a project yields.
 *
 * `sessions` is newest first, and is always empty until Phase 5 introduces
 * sessions.
 */
export type ProjectDetail = {
  readonly project: Project;
  readonly sessions: readonly SessionSummary[];
};
