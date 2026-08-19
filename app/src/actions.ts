//! The three things a spank can do, and their editors.
//!
//! The daemon's model is a free-form list of actions, which is the right shape for a
//! config file and the wrong shape for a settings panel. The UI presents exactly one of
//! each kind, identified by a stable id, and creates it on demand the first time it is
//! switched on. Anything hand-written into the config beyond those three still runs — it
//! simply is not editable here.

import type { Action, ActionKind, DaemonConfig, Tier } from "./types";

/** Stable ids, so an action survives being switched off and on again. */
export const ACTION_IDS = ["sound", "webhook", "exec"] as const;
export type ActionId = (typeof ACTION_IDS)[number];

export const ACTION_LABELS: Record<ActionId, string> = {
  sound: "Sound",
  webhook: "HTTP request",
  exec: "Command",
};

/** Whether this kind is only offered once the advanced switch is on. */
export const IS_ADVANCED: Record<ActionId, boolean> = {
  sound: false,
  webhook: false,
  exec: true,
};

function defaultKind(id: ActionId): ActionKind {
  switch (id) {
    case "sound":
      return {
        type: "sound",
        paths: ["/System/Library/Sounds/Sosumi.aiff"],
        order: "sequential",
        volume_db: 0,
        scale_with_intensity: false,
        intensity_range_pct: 40,
        playback_rate: 1,
      };
    case "webhook":
      return {
        type: "webhook",
        url: "",
        method: "POST",
        headers: {},
        body: null,
        timeout_ms: 5000,
      };
    case "exec":
      return { type: "exec", program: "", args: [], stdin_json: true };
  }
}

function defaultAction(id: ActionId): Action {
  return {
    id,
    name: ACTION_LABELS[id],
    // Created disabled; the caller switches it on. That keeps "create" and "enable"
    // separate, so a half-configured webhook cannot start firing the moment it exists.
    enabled: false,
    tiers: [],
    min_intensity: 0,
    // 200 ms measured by ear: detection beats the sound of the spank itself.
    delay_ms: 200,
    kind: defaultKind(id),
  };
}

export function find(config: DaemonConfig, id: ActionId): Action | undefined {
  return config.actions.find((a) => a.id === id);
}

/** Get the action, creating it if this is the first time it has been used. */
export function ensure(config: DaemonConfig, id: ActionId): Action {
  const existing = find(config, id);
  if (existing) return existing;
  const created = defaultAction(id);
  config.actions.push(created);
  return created;
}

export function isEnabled(config: DaemonConfig, id: ActionId): boolean {
  return find(config, id)?.enabled ?? false;
}

/**
 * Whether an action has enough configuration to do anything.
 *
 * A webhook with no URL or a command with no program is not an error the daemon should
 * reject — it is a half-finished edit — but it is worth saying so rather than letting the
 * user think it is armed.
 */
export function incompleteReason(action: Action): string | null {
  switch (action.kind.type) {
    case "sound":
      return action.kind.paths.length === 0 ? "no sounds added" : null;
    case "webhook":
      if (action.kind.url.trim() === "") return "no URL set";
      return /^https?:\/\//.test(action.kind.url) ? null : "URL must start with http";
    case "exec":
      return action.kind.program.trim() === "" ? "no program chosen" : null;
  }
}

export const ALL_TIERS: Tier[] = ["micro", "medium", "major"];

/** Parse a "Key: Value" per line textarea into a header map. */
export function parseHeaders(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const at = trimmed.indexOf(":");
    if (at <= 0) continue;
    out[trimmed.slice(0, at).trim()] = trimmed.slice(at + 1).trim();
  }
  return out;
}

export function formatHeaders(headers: Record<string, string>): string {
  return Object.entries(headers)
    .map(([k, v]) => `${k}: ${v}`)
    .join("\n");
}

/** Split a per-line textarea into arguments, dropping blanks. */
export function parseLines(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l !== "");
}
