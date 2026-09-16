/**
 * Error decoding for the IPC boundary.
 *
 * A rejected `invoke` gives back whatever the Rust `AppError` serialized to —
 * a plain object, not an `Error`. Everything crossing back into the UI is
 * normalized here so components can `catch (e)` and rely on `e.code`, and so
 * a malformed or unexpected rejection still arrives as a real `Error` rather
 * than as `[object Object]` in a toast.
 */

/**
 * Stable failure codes (PLAN 4.4), mirroring `ErrorCode` in `error.rs`.
 * Kept sorted the same way as the Rust enum so the two stay easy to diff.
 */
export const ERROR_CODES = [
  "E_TURN_BUSY",
  "E_NO_WORKSPACE",
  "E_PATH_OUTSIDE_WORKSPACE",
  "E_PATH_INVALID",
  "E_DENIED",
  "E_GRANT_NOT_ALLOWED",
  "E_APPROVAL_STALE",
  "E_TIMEOUT",
  "E_TOOL_FAILED",
  "E_PROVIDER_HTTP",
  "E_PROVIDER_PARSE",
  "E_NO_API_KEY",
  "E_KEYRING_UNAVAILABLE",
  "E_CANCELLED",
  "E_TOO_MANY_TOOL_ROUNDS",
  "E_TOOL_LOOP",
  "E_SCREEN_PERMISSION",
  "E_INVALID_SETTING",
  "E_INTERNAL",
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

/** Code used when a rejection does not carry one we recognize. */
export const UNKNOWN_ERROR_CODE = "E_UNKNOWN";

/** The JSON an `AppError` serializes to. */
export type IpcErrorPayload = {
  readonly code: string;
  readonly message: string;
  readonly retryable: boolean;
  /**
   * The input the failure is about, when it is about one — "base URL",
   * "model", "tools". Absent on every failure that is not about a form field,
   * which is most of them.
   */
  readonly field?: string;
};

/** A failed `invoke`, normalized. */
export class IpcError extends Error {
  /** Stable code, or `E_UNKNOWN` for a rejection that carried none. */
  readonly code: ErrorCode | typeof UNKNOWN_ERROR_CODE;
  /** Whether repeating the identical call could plausibly succeed. */
  readonly retryable: boolean;
  /** The command that failed, for logs and bug reports. */
  readonly command: string;
  /** The form input this failure is about (`E_INVALID_SETTING` only). */
  readonly field: string | null;

  constructor(
    command: string,
    code: ErrorCode | typeof UNKNOWN_ERROR_CODE,
    message: string,
    retryable: boolean,
    field: string | null = null,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "IpcError";
    this.command = command;
    this.code = code;
    this.retryable = retryable;
    this.field = field;
  }
}

function isErrorCode(value: string): value is ErrorCode {
  return (ERROR_CODES as readonly string[]).includes(value);
}

function isIpcErrorPayload(value: unknown): value is IpcErrorPayload {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as Partial<IpcErrorPayload>;
  return (
    typeof candidate.code === "string" &&
    typeof candidate.message === "string" &&
    typeof candidate.retryable === "boolean"
  );
}

/**
 * Turns anything thrown by `invoke` into an {@link IpcError}.
 *
 * Three shapes reach here: the structured payload above; a bare string, which
 * is what Tauri produces when a command panics or a handler is missing; and
 * genuinely unexpected values, which are stringified rather than dropped.
 */
export function toIpcError(cause: unknown, command: string): IpcError {
  if (cause instanceof IpcError) {
    return cause;
  }

  if (isIpcErrorPayload(cause)) {
    const code = isErrorCode(cause.code) ? cause.code : UNKNOWN_ERROR_CODE;
    return new IpcError(
      command,
      code,
      cause.message,
      cause.retryable,
      cause.field ?? null,
      { cause },
    );
  }

  const message = typeof cause === "string" ? cause : String(cause);
  return new IpcError(command, UNKNOWN_ERROR_CODE, message, false, null, {
    cause,
  });
}
