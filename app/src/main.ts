import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { Scope } from "./scope";
import type { Action, DaemonConfig, Slap, Status, Telemetry } from "./types";

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

  const delay = config.actions[0]?.delay_ms ?? 0;
  $<HTMLInputElement>("delay").value = String(delay);
  $("delay-out").textContent = `${delay} ms`;

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
  renderActions(config.actions);
}

/** Plain-language summary, from the measured sweep over the recorded corpus. */
function describeSensitivity(value: number): string {
  if (value <= 0.25) return "Only a deliberate whack registers.";
  if (value <= 0.4) return "Firm slaps only — never fires by accident.";
  if (value <= 0.55) return "Balanced. Catches most slaps, ignores typing and the desk.";
  if (value <= 0.7) return "Catches everything, including knocks on the desk.";
  return "Twitchy — anything that shakes the desk counts.";
}

function renderActions(actions: Action[]) {
  const host = $("actions");
  host.replaceChildren();

  if (actions.length === 0) {
    const empty = document.createElement("p");
    empty.className = "hint";
    empty.textContent = "No actions configured.";
    host.append(empty);
    return;
  }

  for (const action of actions) {
    const row = document.createElement("div");
    row.className = "action";

    const on = document.createElement("input");
    on.type = "checkbox";
    on.checked = action.enabled;
    on.addEventListener("change", () => {
      action.enabled = on.checked;
      pushConfig();
    });

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = action.name || action.id;

    const kind = document.createElement("span");
    kind.className = "kind";
    kind.textContent = describeAction(action);

    const test = document.createElement("button");
    test.textContent = "Test";
    test.addEventListener("click", async () => {
      try {
        await invoke("test_action", { id: action.id, intensity: 0.85 });
      } catch (e) {
        toast(String(e));
      }
    });

    row.append(on, name, kind);

    // Sounds are the one action kind with a file to choose; exec and webhook are edited
    // in the config file for now.
    if (action.kind.type === "sound") {
      const choose = document.createElement("button");
      choose.textContent = "Choose…";
      choose.addEventListener("click", () => void pickSound(action));
      row.append(choose);
    }

    row.append(test);
    host.append(row);
  }
}

/** Formats the decoder understands. Anything outside this list would fail to load. */
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

async function pickSound(action: Action) {
  if (action.kind.type !== "sound") return;
  try {
    const picked = await openDialog({
      multiple: false,
      directory: false,
      title: "Choose a sound",
      filters: [{ name: "Audio", extensions: AUDIO_EXTENSIONS }],
    });
    if (typeof picked !== "string") return; // cancelled

    action.kind.path = picked;
    pushConfig();
    render();

    // Play it straight away. The daemon decodes on config change, so a file it cannot
    // read shows up here as silence rather than as an error in a log nobody reads.
    setTimeout(() => {
      invoke("test_action", { id: action.id, intensity: 0.85 }).catch((e) =>
        toast(String(e)),
      );
    }, 400);
  } catch (e) {
    toast(String(e));
  }
}

function describeAction(action: Action): string {
  switch (action.kind.type) {
    case "sound":
      return action.kind.path.split("/").pop() ?? "sound";
    case "exec":
      return action.kind.program.split("/").pop() ?? "exec";
    case "webhook":
      try {
        return new URL(action.kind.url).host;
      } catch {
        return "webhook";
      }
  }
}

function renderStatus(status: Status) {
  const parts = [
    `${status.rate_hz.toFixed(0)} Hz`,
    status.has_gyro ? "gyro" : "no gyro",
    `${status.slaps} slap${status.slaps === 1 ? "" : "s"}`,
  ];
  if (status.warming_up) parts.push("warming up");
  $("daemon-meta").textContent = parts.join(" · ");
  $("status-text").textContent = status.enabled ? "detecting" : "paused";
}

// --- wiring ---

$("sensitivity").addEventListener("input", (e) => {
  if (!config) return;
  const value = Number((e.target as HTMLInputElement).value);
  config.detector.sensitivity = value;
  $("sensitivity-out").textContent = value.toFixed(2);
  $("sensitivity-hint").textContent = describeSensitivity(value);
  pushConfig();
});

$("delay").addEventListener("input", (e) => {
  if (!config) return;
  const value = Number((e.target as HTMLInputElement).value);
  // Applies to every action: a webhook driving a light wants the same beat as a sound.
  for (const action of config.actions) action.delay_ms = value;
  $("delay-out").textContent = `${value} ms`;
  pushConfig();
});

$("cooldown").addEventListener("input", (e) => {
  if (!config) return;
  const value = Number((e.target as HTMLInputElement).value);
  config.detector.cooldown_s = value;
  $("cooldown-out").textContent = `${value.toFixed(2)} s`;
  pushConfig();
});

$("scale-volume").addEventListener("change", (e) => {
  if (!config) return;
  const on = (e.target as HTMLInputElement).checked;
  for (const action of config.actions) {
    if (action.kind.type === "sound") action.kind.scale_with_intensity = on;
  }
  $("range-wrap").hidden = !on;
  $("scale-hint").textContent = on
    ? "A mid-strength slap plays at your system volume; gentler is quieter, harder is louder."
    : "Off: sounds play at your system volume.";
  pushConfig();
});

$("volume-range").addEventListener("input", (e) => {
  if (!config) return;
  const value = Number((e.target as HTMLInputElement).value);
  for (const action of config.actions) {
    if (action.kind.type === "sound") action.kind.intensity_range_pct = value;
  }
  $("volume-range-out").textContent = `± ${value.toFixed(0)}%`;
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
    $("status-text").textContent = "spankd not running";
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
