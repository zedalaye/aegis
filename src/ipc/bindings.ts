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
/**
 * How a tool call came to run, or not (PLAN 3.1).
 *
 * `auto` is policy allowing something with no prompt; the other three are the
 * answer a user gave to an approval. Mirrors `AuditDecision` in `audit.rs`.
 */
export type AuditDecision = "auto" | "allow_once" | "allow_session" | "deny";

/** How a tool call ended. Mirrors `Outcome` in `audit.rs`. */
export type AuditOutcome = "ok" | "error" | "denied" | "cancelled";

/**
 * One line of the audit log — one tool call, whatever became of it.
 *
 * These are read from `audit.jsonl` on disk, so this type is the on-disk
 * format as much as it is the wire format. A Rust test asserts the field
 * names against the same struct that writes the file.
 *
 * Note what is deliberately absent: the arguments themselves. `args_digest`
 * identifies a call without quoting it, and `args_redacted` keeps the paths —
 * the part worth reading — while replacing file content with its size.
 */
export type AuditEntry = {
  /** RFC3339, UTC, millisecond precision. */
  readonly ts: string;
  readonly session_id: string;
  readonly turn_id: string;
  /** The model's own id for the call. */
  readonly call_id: string;
  /** Tool name: `fs_list`, `fs_read`, `fs_write`, ... */
  readonly tool: string;
  readonly decision: AuditDecision;
  /** Why policy decided that, in the words the user was shown. */
  readonly policy_reason: string;
  /** SHA-256 of the canonical arguments JSON, hex. */
  readonly args_digest: string;
  /** The arguments as JSON: paths kept whole, file content replaced by size. */
  readonly args_redacted: string;
  readonly outcome: AuditOutcome;
  /** Wall-clock duration of the execution itself. */
  readonly duration_ms: number;
  /** Bytes the call carried in — the content of a write. */
  readonly bytes_in: number;
  /** Bytes the call produced, before truncation for the model. */
  readonly bytes_out: number;
  /** The stable code when this failed, `null` otherwise. */
  readonly error_code: string | null;
};
