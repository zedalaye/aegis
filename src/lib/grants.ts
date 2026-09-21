/**
 * What identifies a grant, and the tool it applies to. Mirrors `Grant` in
 * `policy/grants.rs`; the scope sentences stay beside the lists that show them.
 */

import type { Grant } from "../ipc/bindings";

/** A stable key for a grant: the variant, plus whatever narrows it. */
export function grantKey(grant: Grant): string {
  switch (grant.kind) {
    case "shell":
      return `shell:${grant.program}`;
    case "fs_write_under":
      return `fs_write_under:${grant.prefix}`;
    case "shell_shape":
      return `shell_shape:${grant.program}\u0000${grant.args.join("\u0000")}`;
    // A connector grant is one tool, so two of them differ by the tool they
    // name — the variant alone would fold every connector approval into one.
    case "connector":
      return `connector:${grant.tool}`;
    case "jev_eval":
      return `jev_eval:${grant.name}`;
    default:
      return grant.kind;
  }
}

/** Whether two grants are the same approval. */
export function sameGrant(one: Grant, other: Grant): boolean {
  return grantKey(one) === grantKey(other);
}

/** The tool a grant can ever apply to. Mirrors `Grant::tool` in Rust. */
export function toolOf(grant: Grant): string {
  switch (grant.kind) {
    case "fs_read_large":
      return "fs_read";
    case "fs_write":
    case "fs_write_under":
    case "world_amend":
      return "fs_write";
    case "shell":
    case "shell_shape":
      return "shell_exec";
    case "connector":
      return grant.tool;
    default:
      return grant.kind;
  }
}

/** `cargo test …`: a command shape as it reads (PLAN 7.23). */
export function shapeLine(grant: { program: string; args: string[] }): string {
  return [grant.program, ...grant.args].join(" ");
}
