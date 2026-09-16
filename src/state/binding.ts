/**
 * Which provider row and model a session would answer from (PLAN 7.19).
 *
 * Display only: mirrors `store::settings::resolve`, which the runtime runs
 * per turn. Row: session override, else identity, else default. Model: session
 * override, else the row's when the session chose the row, else the
 * identity's, else the row's. A row not on file answers from the default one.
 */

import type {
  Agent,
  MaskedProvider,
  MaskedSettings,
  SessionSummary,
} from "../ipc/bindings";
import { useAgents } from "./agents";
import { DEFAULT_PROVIDER_ID, defaultRow, isConfigured, rowOf, useSettings } from "./settings";

export type ResolvedBinding = {
  /** The row that answers, or `undefined` before the roster has loaded. */
  readonly row: MaskedProvider | undefined;
  /** The model that is sent. */
  readonly model: string;
  /** Whether that pair is a real provider rather than the scripted one. */
  readonly configured: boolean;
  /** Whether the session overrides its identity's pair. */
  readonly overridden: boolean;
};

/** The pure resolution, for a known roster. */
export function resolveBinding(
  settings: MaskedSettings,
  agent: Pick<Agent, "provider_id" | "model"> | undefined,
  session: Pick<SessionSummary, "provider_id" | "model"> | null,
): ResolvedBinding {
  const sessionProvider = nonEmpty(session?.provider_id);
  const sessionModel = nonEmpty(session?.model);
  const overridden = sessionProvider !== null || sessionModel !== null;
  const wanted =
    sessionProvider ?? nonEmpty(agent?.provider_id) ?? DEFAULT_PROVIDER_ID;

  const row = rowOf(settings, wanted);
  if (row === undefined) {
    const fallback = defaultRow(settings);
    const model = fallback?.model ?? "";
    return {
      row: fallback,
      model,
      configured: fallback !== undefined && isConfigured(fallback, model),
      overridden,
    };
  }

  const model =
    sessionModel ??
    (sessionProvider === null ? nonEmpty(agent?.model) : null) ??
    row.model;
  return {
    row,
    model,
    configured: isConfigured(row, model),
    overridden,
  };
}

function nonEmpty(value: string | null | undefined): string | null {
  return value === null || value === undefined || value.length === 0
    ? null
    : value;
}

/** The resolved binding of one session, kept current by both stores. */
export function useBinding(session: SessionSummary | null): ResolvedBinding {
  const settings = useSettings((s) => s.settings);
  const agent = useAgents((s) =>
    session === null
      ? undefined
      : s.agents.find((candidate) => candidate.id === session.agent_id),
  );

  if (settings === null) {
    return { row: undefined, model: "", configured: false, overridden: false };
  }
  return resolveBinding(settings, agent, session);
}
