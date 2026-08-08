import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { Scope } from "./scope";
import type { Action, DaemonConfig, Slap, Status, Telemetry } from "./types";
import * as actions from "./actions";
import type { ActionId } from "./actions";

const $ = <T extends HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`missing element #${id}`);
  return el as T;
};

const scope = new Scope($<HTMLCanvasElement>("scope"));
let config: DaemonConfig | null = null;
let dirty = false;

/** Debounced config push, so dragging a slider doesn't write the file 60 times a second. */
let pushTimer: number | undefined;
function pushConfig() {
  dirty = true;
  clearTimeout(pushTimer);
  pushTimer = setTimeout(async () => {
    if (!config || !dirty) return;
    dirty = false;
    try {
      config = await invoke<DaemonConfig>("set_config", { config });
      render();
    } catch (e) {
      toast(String(e));
    }
  }, 180) as unknown as number;
}

function toast(message: string) {
  const el = $("toast");
  el.textContent = message;
  el.hidden = false;
  setTimeout(() => (el.hidden = true), 4000);
}

// --- rendering ---

function render() {
  if (!config) return;
  const d = config.detector;

  $<HTMLInputElement>("sensitivity").value = String(d.sensitivity);
  $("sensitivity-out").textContent = d.sensitivity.toFixed(2);
  $("sensitivity-hint").textContent = describeSensitivity(d.sensitivity);

  $<HTMLInputElement>("cooldown").value = String(d.cooldown_s);
  $("cooldown-out").textContent = `${d.cooldown_s.toFixed(2)} s`;

  const sound = config.actions.find((a) => a.kind.type === "sound");
  const scaling = sound?.kind.type === "sound" ? sound.kind.scale_with_intensity : false;
  const swing = sound?.kind.type === "sound" ? sound.kind.intensity_range_pct : 40;
  $<HTMLInputElement>("scale-volume").checked = scaling;
  $<HTMLInputElement>("volume-range").value = String(swing);
  $("volume-range-out").textContent = `± ${swing.toFixed(0)}%`;
  $("range-wrap").hidden = !scaling;
  $("scale-hint").textContent = scaling
    ? "A mid-strength slap plays at your system volume; gentler is quieter, harder is louder."
    : "Off: sounds play at your system volume.";

  $<HTMLInputElement>("lowpower").checked = config.report_interval_us >= 2000;

  $("toggle").textContent = config.enabled ? "Pause detection" : "Resume detection";

  scope.setTiers(d.tiers);
  renderActionPages();

}

/** Plain-language summary, from the measured sweep over the recorded corpus. */
function describeSensitivity(value: number): string {
  if (value <= 0.25) return "Only a deliberate whack registers.";
  if (value <= 0.4) return "Firm slaps only — never fires by accident.";
  if (value <= 0.55) return "Balanced. Catches most slaps, ignores typing and the desk.";
  if (value <= 0.7) return "Catches everything, including knocks on the desk.";
  return "Twitchy — anything that shakes the desk counts.";
}

/** Formats the decoder understands; anything else would fail to load. */
const AUDIO_EXTENSIONS = [
  "wav",
  "mp3",
  "ogg",
  "flac",
  "aiff",
  "aif",
  "aifc",
  "caf",
  "m4a",
  "aac",
];

async function pick(
  title: string,
  filters?: { name: string; extensions: string[] }[],
): Promise<string | null> {
  const picked = await openDialog({
    multiple: false,
    directory: false,
    title,
    ...(filters ? { filters } : {}),
  });
  return typeof picked === "string" ? picked : null;
}

/// Fire one action, refusing when it is not configured enough to do anything.
///
/// A webhook with no URL is a half-finished edit rather than an error the daemon should
/// reject, but silently doing nothing would read as a broken Test button.
async function testAction(id: ActionId) {
  if (!config) return;
  const action = actions.find(config, id);
  if (!action) return;
  const problem = actions.incompleteReason(action);
  if (problem) {
    toast(`${actions.ACTION_LABELS[id]}: ${problem}`);
    return;
  }
  try {
    await invoke("test_action", { id, intensity: 0.85 });
  } catch (e) {
    toast(String(e));
  }
}

/** UI-only preference; the daemon has no opinion about what the user is allowed to see. */
function advancedUnlocked(): boolean {
  return localStorage.getItem("yamete.advanced") === "1";
}

function setAdvanced(on: boolean) {
  localStorage.setItem("yamete.advanced", on ? "1" : "0");
  $("exec-row").hidden = !on;
  // Turning the gate off does not disable a command that is already running — that would
  // be a surprising thing for a visibility switch to do — but it does hide its editor.
  renderNav();
}

/** Rebuild the sidebar sub-items for whichever actions are switched on. */
function renderNav() {
  const host = $("nav-actions");
  host.replaceChildren();
  if (!config) return;

  for (const id of actions.ACTION_IDS) {
    if (!actions.isEnabled(config, id)) continue;
    if (actions.IS_ADVANCED[id] && !advancedUnlocked()) continue;

    const item = document.createElement("button");
    item.className = "nav-item";
    item.dataset.section = id;
    item.textContent = actions.ACTION_LABELS[id];
    item.addEventListener("click", () => showSection(id));
    host.append(item);
  }
  // The active class lives on nav items that are rebuilt here, so restore it.
  markActiveNav(currentSection);
}

/** Controls every action shares: which severities fire it, how hard, how late. */
function renderCommon(id: ActionId) {
  if (!config) return;
  const host = document.querySelector<HTMLElement>(`.common[data-action="${id}"]`);
  if (!host) return;
  const action = actions.ensure(config, id);
  host.replaceChildren();

  const tierRow = document.createElement("div");
  tierRow.className = "tier-picker";
  for (const tier of actions.ALL_TIERS) {
    const label = document.createElement("label");
    const box = document.createElement("input");
    box.type = "checkbox";
    // An empty list means "all tiers", which is the daemon's convention.
    box.checked = action.tiers.length === 0 || action.tiers.includes(tier);
    box.addEventListener("change", () => {
      const chosen = actions.ALL_TIERS.filter((t) => {
        const el = tierRow.querySelector<HTMLInputElement>(`input[data-tier="${t}"]`);
        return el?.checked ?? false;
      });
      // All three selected is the same as no filter; store it that way so the config
      // stays meaningful if new tiers ever appear.
      action.tiers = chosen.length === actions.ALL_TIERS.length ? [] : chosen;
      pushConfig();
    });
    box.dataset.tier = tier;
    label.append(box, document.createTextNode(tier));
    tierRow.append(label);
  }
  const tierLabel = document.createElement("label");
  tierLabel.className = "field-label";
  tierLabel.textContent = "Fires on";
  host.append(tierLabel, tierRow);

  host.append(
    slider({
      label: "Minimum force",
      value: action.min_intensity,
      min: 0,
      max: 1,
      step: 0.05,
      format: (v) => (v === 0 ? "any" : v.toFixed(2)),
      onInput: (v) => {
        action.min_intensity = v;
        pushConfig();
      },
    }),
  );

  host.append(
    slider({
      label: "Delay",
      value: action.delay_ms,
      min: 0,
      max: 600,
      step: 10,
      format: (v) => `${v} ms`,
      onInput: (v) => {
        action.delay_ms = v;
        pushConfig();
      },
    }),
  );
}

interface SliderSpec {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  format: (v: number) => string;
  onInput: (v: number) => void;
}

function slider(spec: SliderSpec): HTMLElement {
  const wrap = document.createElement("div");
  const row = document.createElement("div");
  row.className = "row";
  const name = document.createElement("span");
  name.textContent = spec.label;
  const out = document.createElement("output");
  out.textContent = spec.format(spec.value);
  row.append(name, out);

  const input = document.createElement("input");
  input.type = "range";
  input.min = String(spec.min);
  input.max = String(spec.max);
  input.step = String(spec.step);
  input.value = String(spec.value);
  input.addEventListener("input", () => {
    const v = Number(input.value);
    out.textContent = spec.format(v);
    spec.onInput(v);
  });

  wrap.append(row, input);
  return wrap;
}

function renderActionPages() {
  if (!config) return;

  // Sound
  const sound = actions.ensure(config, "sound");
  if (sound.kind.type === "sound") {
    renderSoundList(sound.kind.paths);
    // With one sound there is nothing to order, so the choice is noise.
    $("order-wrap").hidden = sound.kind.paths.length < 2;
    $<HTMLSelectElement>("sound-order").value = sound.kind.order;
    $<HTMLInputElement>("scale-volume").checked = sound.kind.scale_with_intensity;
    $<HTMLInputElement>("volume-range").value = String(sound.kind.intensity_range_pct);
    $("volume-range-out").textContent = `± ${sound.kind.intensity_range_pct.toFixed(0)}%`;
    $("range-wrap").hidden = !sound.kind.scale_with_intensity;
    $("scale-hint").textContent = sound.kind.scale_with_intensity
      ? "A mid-strength slap plays at your system volume; gentler is quieter, harder is louder."
      : "Off: sounds play at your system volume.";
  }

  // Webhook
  const webhook = actions.ensure(config, "webhook");
  if (webhook.kind.type === "webhook") {
    $<HTMLInputElement>("webhook-url").value = webhook.kind.url;
    $<HTMLSelectElement>("webhook-method").value = webhook.kind.method;
    $<HTMLTextAreaElement>("webhook-headers").value = actions.formatHeaders(
      webhook.kind.headers,
    );
    $<HTMLTextAreaElement>("webhook-body").value = webhook.kind.body ?? "";
  }

  // Exec
  const exec = actions.ensure(config, "exec");
  if (exec.kind.type === "exec") {
    $("exec-path").textContent = exec.kind.program || "—";
    $<HTMLTextAreaElement>("exec-args").value = exec.kind.args.join("\n");
    $<HTMLInputElement>("exec-stdin").checked = exec.kind.stdin_json;
  }

  for (const id of actions.ACTION_IDS) {
    $<HTMLInputElement>(`on-${id}`).checked = actions.isEnabled(config, id);
    renderCommon(id);
  }
  renderNav();
}

function renderSoundList(paths: string[]) {
  const host = $("sound-list");
  host.replaceChildren();

  if (paths.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "No sounds yet — add one.";
    host.append(li);
    return;
  }

  paths.forEach((path, index) => {
    const li = document.createElement("li");

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = path;
    name.title = path;

    const drop = document.createElement("button");
    drop.className = "drop";
    drop.textContent = "×";
    drop.title = "Remove";
    drop.addEventListener("click", () => {
      if (!config) return;
      const action = actions.ensure(config, "sound");
      if (action.kind.type !== "sound") return;
      action.kind.paths.splice(index, 1);
      pushConfig();
      renderActionPages();
    });

    li.append(name, drop);
    host.append(li);
  });
}

/** Render a label/value grid. */
function facts(host: HTMLElement, rows: Array<[string, string]>) {
  host.replaceChildren();
  for (const [label, value] of rows) {
    const dt = document.createElement("dt");
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.textContent = value;
    host.append(dt, dd);
  }
}

function renderStatus(status: Status) {
  $("status-text").textContent = status.enabled ? "detecting" : "paused";

  facts($("facts"), [
    ["Slaps", String(status.slaps)],
    [
      "State",
      status.warming_up ? "warming up" : status.enabled ? "detecting" : "paused",
    ],
  ]);

  facts($("about-facts"), [
    ["Version", status.version],
    ["Sensor", `${status.rate_hz.toFixed(0)} Hz`],
    ["Gyroscope", status.has_gyro ? "present" : "not found"],
    ["Uptime", `${Math.floor(status.uptime_s / 60)}m ${Math.floor(status.uptime_s % 60)}s`],
    ["Slaps", String(status.slaps)],
    ["Telemetry", `${status.telemetry_subscribers} subscriber(s)`],
  ]);
}

// --- sections ---

/// Telemetry only matters while the scope is actually on screen, so switching away from
/// Monitor stops the daemon generating frames just as hiding the window does.
let currentSection = "monitor";

function markActiveNav(name: string) {
  for (const item of document.querySelectorAll<HTMLElement>(".nav-item")) {
    item.classList.toggle("is-active", item.dataset.section === name);
  }
}

function showSection(name: string) {
  currentSection = name;
  markActiveNav(name);
  for (const page of document.querySelectorAll<HTMLElement>(".page")) {
    page.classList.toggle("is-active", page.dataset.page === name);
  }
  if (name === "monitor") {
    // The canvas was display:none, so it has no useful size until now.
    requestAnimationFrame(() => scope.resize());
  }
  invoke("set_scope_visible", { visible: name === "monitor" }).catch(() => {});
}

for (const item of document.querySelectorAll<HTMLElement>(".nav-item")) {
  item.addEventListener("click", () => showSection(item.dataset.section ?? "monitor"));
}

$("quit").addEventListener("click", () => {
  invoke("quit_app").catch((e) => toast(String(e)));
});

// --- wiring ---

$("sensitivity").addEventListener("input", (e) => {
  if (!config) return;
  const value = Number((e.target as HTMLInputElement).value);
  config.detector.sensitivity = value;
  $("sensitivity-out").textContent = value.toFixed(2);
  $("sensitivity-hint").textContent = describeSensitivity(value);
  pushConfig();
});

$("cooldown").addEventListener("input", (e) => {
  if (!config) return;
  const value = Number((e.target as HTMLInputElement).value);
  config.detector.cooldown_s = value;
  $("cooldown-out").textContent = `${value.toFixed(2)} s`;
  pushConfig();
});

$("lowpower").addEventListener("change", (e) => {
  if (!config) return;
  config.report_interval_us = (e.target as HTMLInputElement).checked ? 2000 : 1000;
  pushConfig();
});

$("toggle").addEventListener("click", async () => {
  if (!config) return;
  try {
    config = await invoke<DaemonConfig>("set_enabled", { value: !config.enabled });
    render();
  } catch (e) {
    toast(String(e));
  }
});

// --- action editors ---

for (const id of actions.ACTION_IDS) {
  $(`on-${id}`).addEventListener("change", (e) => {
    if (!config) return;
    const on = (e.target as HTMLInputElement).checked;
    actions.ensure(config, id).enabled = on;
    pushConfig();
    renderNav();
    // Jump straight to the settings for whatever was just switched on — it needs
    // configuring before it can do anything, and hiding that behind a second click is
    // how you end up with an enabled webhook pointing at nothing.
    if (on) showSection(id);
    else if (currentSection === id) showSection("actions");
  });
}

$("advanced").addEventListener("change", (e) => {
  const on = (e.target as HTMLInputElement).checked;
  setAdvanced(on);
  if (!on && currentSection === "exec") showSection("actions");
});

// Sound
$("sound-pick").addEventListener("click", async () => {
  if (!config) return;
  const picked = await openDialog({
    multiple: true,
    directory: false,
    title: "Add sounds",
    filters: [{ name: "Audio", extensions: AUDIO_EXTENSIONS }],
  }).catch(() => null);
  if (!picked) return;

  const chosen = Array.isArray(picked) ? picked : [picked];
  const action = actions.ensure(config, "sound");
  if (action.kind.type !== "sound") return;

  for (const path of chosen) {
    // Adding the same file twice would give it double the odds under random order, and
    // is never what was meant.
    if (!action.kind.paths.includes(path)) action.kind.paths.push(path);
  }
  pushConfig();
  renderActionPages();
});

$("sound-order").addEventListener("change", (e) => {
  if (!config) return;
  const action = actions.ensure(config, "sound");
  if (action.kind.type !== "sound") return;
  action.kind.order = (e.target as HTMLSelectElement).value as "sequential" | "random";
  pushConfig();
});

$("sound-test").addEventListener("click", () => void testAction("sound"));

$("scale-volume").addEventListener("change", (e) => {
  if (!config) return;
  const on = (e.target as HTMLInputElement).checked;
  const action = actions.ensure(config, "sound");
  if (action.kind.type === "sound") action.kind.scale_with_intensity = on;
  $("range-wrap").hidden = !on;
  $("scale-hint").textContent = on
    ? "A mid-strength slap plays at your system volume; gentler is quieter, harder is louder."
    : "Off: sounds play at your system volume.";
  pushConfig();
});

$("volume-range").addEventListener("input", (e) => {
  if (!config) return;
  const value = Number((e.target as HTMLInputElement).value);
  const action = actions.ensure(config, "sound");
  if (action.kind.type === "sound") action.kind.intensity_range_pct = value;
  $("volume-range-out").textContent = `± ${value.toFixed(0)}%`;
  pushConfig();
});

// Webhook
function editWebhook(mutate: (kind: Extract<Action["kind"], { type: "webhook" }>) => void) {
  if (!config) return;
  const action = actions.ensure(config, "webhook");
  if (action.kind.type !== "webhook") return;
  mutate(action.kind);
  pushConfig();
}

$("webhook-url").addEventListener("input", (e) =>
  editWebhook((k) => (k.url = (e.target as HTMLInputElement).value.trim())),
);
$("webhook-method").addEventListener("change", (e) =>
  editWebhook((k) => (k.method = (e.target as HTMLSelectElement).value)),
);
$("webhook-headers").addEventListener("input", (e) =>
  editWebhook(
    (k) => (k.headers = actions.parseHeaders((e.target as HTMLTextAreaElement).value)),
  ),
);
$("webhook-body").addEventListener("input", (e) =>
  editWebhook((k) => {
    const text = (e.target as HTMLTextAreaElement).value;
    // Empty means "send the slap as JSON", which is not the same as sending nothing.
    k.body = text.trim() === "" ? null : text;
  }),
);
$("webhook-test").addEventListener("click", () => void testAction("webhook"));

// Exec
$("exec-pick").addEventListener("click", async () => {
  if (!config) return;
  const path = await pick("Choose a program").catch(() => null);
  if (!path) return;
  const action = actions.ensure(config, "exec");
  if (action.kind.type === "exec") action.kind.program = path;
  $("exec-path").textContent = path;
  pushConfig();
});

$("exec-args").addEventListener("input", (e) => {
  if (!config) return;
  const action = actions.ensure(config, "exec");
  if (action.kind.type === "exec") {
    action.kind.args = actions.parseLines((e.target as HTMLTextAreaElement).value);
  }
  pushConfig();
});

$("exec-stdin").addEventListener("change", (e) => {
  if (!config) return;
  const action = actions.ensure(config, "exec");
  if (action.kind.type === "exec") {
    action.kind.stdin_json = (e.target as HTMLInputElement).checked;
  }
  pushConfig();
});

$("exec-test").addEventListener("click", () => void testAction("exec"));

$("logs").addEventListener("click", () => {
  invoke("open_logs").catch((e) => toast(String(e)));
});

// --- daemon events ---

listen<Telemetry>("telemetry", (event) => {
  $("scope-empty").hidden = true;
  scope.push(event.payload);
});

listen<Status>("status", (event) => renderStatus(event.payload));

listen<Slap>("slap", (event) => {
  const slap = event.payload;
  scope.markSlap(slap.tier);
  $("last-slap").textContent =
    `${slap.tier} · ${slap.peak_g.toFixed(3)} g · ${slap.gyro_peak.toFixed(0)}°/s`;
});

/** Single place that reflects connection state, so events and polling can't disagree. */
function setConnected(connected: boolean) {
  const dot = $("conn");
  dot.classList.toggle("on", connected);
  dot.classList.toggle("off", !connected);
  if (connected) {
    // Never leave the initial placeholder on screen once we know we're connected;
    // renderStatus will replace this with the real state a moment later.
    if ($("status-text").textContent === "connecting…") {
      $("status-text").textContent = "connected";
    }
  } else {
    $("status-text").textContent = "yamete not running";
    $("scope-empty").textContent = "waiting for the daemon…";
    $("scope-empty").hidden = false;
  }
}

listen<boolean>("daemon-connection", (event) => {
  setConnected(event.payload);
  if (event.payload) void refresh();
});

listen<string>("daemon-error", (event) => toast(event.payload));

// --- lifecycle ---

async function refresh() {
  try {
    config = await invoke<DaemonConfig>("get_config");
    render();
    renderStatus(await invoke<Status>("get_status"));
  } catch (e) {
    setConnected(false);
    toast(String(e));
  }
}

// Telemetry is switched on and off natively, from whether the window is actually on
// screen. `document.visibilityState` reflects the web view's occlusion rather than the
// window's visibility and does not fire on a native show/hide, so it cannot be trusted
// for this.

window.addEventListener("resize", () => scope.resize());

function frame() {
  scope.draw();
  requestAnimationFrame(frame);
}

// Surface anything that would otherwise vanish. A rejected promise inside startup used
// to leave the UI sitting on its initial "connecting…" text with no clue why — the event
// listeners kept working, so it looked like a daemon problem rather than a frontend one.
window.addEventListener("unhandledrejection", (e) => {
  toast(`startup: ${e.reason}`);
});
window.addEventListener("error", (e) => toast(`error: ${e.message}`));

/** Run a step without letting its failure abort the rest of startup. */
async function step(name: string, run: () => Promise<void>) {
  try {
    await run();
  } catch (e) {
    toast(`${name}: ${e}`);
  }
}

$<HTMLInputElement>("advanced").checked = advancedUnlocked();
setAdvanced(advancedUnlocked());
showSection("monitor");

void (async () => {
  // The subscription connects before the webview finishes registering its listeners, so
  // the initial `daemon-connection` event is emitted into the void. Ask directly rather
  // than waiting for an event that has already been and gone.
  await step("connect", async () => {
    setConnected(await invoke<boolean>("is_connected"));
  });
  await step("refresh", refresh);

  setInterval(() => {
    void (async () => {
      try {
        const connected = await invoke<boolean>("is_connected");
        setConnected(connected);
        if (connected) {
          renderStatus(await invoke<Status>("get_status"));
          // The daemon may have started after us, so pick up its config once available.
          if (!config) await refresh();
        }
      } catch {
        /* transient; the indicator already covers it */
      }
    })();
  }, 2000);

  requestAnimationFrame(frame);
})();
