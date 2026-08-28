/**
 * Typed `invoke` wrappers — one function per Rust command.
 *
 * Components never call `invoke` directly: the command name and the argument
 * shape are spelled out exactly once, here, and every rejection is normalized
 * to an {@link IpcError}. Payload *types* move the other way — from Phase 5
 * they are generated into `./bindings.ts` by `ts-rs`, so this file imports
 * them rather than restating them.
 *
 * Commands land with their phases; Phase 1 covers the window and application
 * lifecycle (PLAN 2.1, "Window / tray").
 */

import { invoke } from "@tauri-apps/api/core";
import type { InvokeArgs } from "@tauri-apps/api/core";

import { toIpcError } from "../lib/errors";

async function call<T>(command: string, args?: InvokeArgs): Promise<T> {
  try {
    return args === undefined
      ? await invoke<T>(command)
      : await invoke<T>(command, args);
  } catch (cause) {
    throw toIpcError(cause, command);
  }
}

/**
 * Shows the main window if it is hidden or unfocused, hides it otherwise —
 * the same toggle the tray icon performs.
 */
export function windowToggle(): Promise<void> {
  return call<void>("window_toggle");
}

/** Hides the main window to the tray. The process keeps running. */
export function windowHide(): Promise<void> {
  return call<void>("window_hide");
}

/**
 * Quits Aegis.
 *
 * Resolves only if the runtime rejects the request; a successful quit tears
 * the WebView down before the promise settles, so callers must not rely on
 * anything running after this.
 */
export function appQuit(): Promise<void> {
  return call<void>("app_quit");
}
