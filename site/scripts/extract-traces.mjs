// Slices real windows out of the fixture corpus for the charts on the landing page.
//
// The page plots the *same* signal the detector votes on, not raw accelerometer
// magnitude. Raw magnitude is dominated by the gravity vector swinging as the lid moves,
// which is large, slow and says nothing about the impact — a slap reads 0.52 g raw and
// 0.08 g once gravity is stripped. Plotting raw would overstate slaps by 6x and quietly
// contradict every number the detector reports.
//
// So this mirrors `HighPass` from crates/yamete-dsp/src/filters.rs exactly:
//   y[n] = a·(y[n-1] + x[n] − x[n-1]),  a = exp(−2π·fc/fs),  fc = 5 Hz
// primed from the first sample so it does not emit a step while charging against 1 g.
//
// The output is checked against `yamete analyze` in verify() below — if the ports ever
// diverge, this fails the build rather than shipping a chart that lies.
//
// Regenerate with:  bun run extract

import { gunzipSync } from "node:zlib";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURES = join(HERE, "../../fixtures");
const OUT = join(HERE, "../src/data/traces.json");

const HIGHPASS_HZ = 5.0;
// Asymmetric on purpose. A short run-up establishes the noise floor, and the long tail is
// the chassis ringing after the hit — which is the whole reason a cooldown exists, so it
// is worth seeing rather than cropping.
const LEAD_S = 0.05;
const TAIL_S = 0.2;
const POINTS = 180; // after downsampling

/** One-pole DC blocker. Port of yamete-dsp `HighPass`. */
function makeHighPass(cutoffHz, rateHz) {
  const a = Math.exp((-2 * Math.PI * cutoffHz) / rateHz);
  let prevIn = 0,
    prevOut = 0,
    primed = false;
  return (x) => {
    if (!primed) {
      prevIn = x;
      primed = true;
      return 0;
    }
    const y = a * (prevOut + x - prevIn);
    prevIn = x;
    prevOut = y;
    return y;
  };
}

function parseFixture(name) {
  const raw = readFileSync(join(FIXTURES, `${name}.fixture.gz`));
  const text = gunzipSync(raw).toString("utf8");
  const meta = {};
  const accel = [];
  const gyro = [];
  for (const line of text.split("\n")) {
    if (line.startsWith("#")) {
      const m = line.match(/^#\s*(\w+)=(.*)$/);
      if (m) meta[m[1]] = m[2].trim();
      continue;
    }
    if (!line || line.startsWith("s,")) continue;
    const p = line.split(",");
    if (p.length !== 5) continue;
    const row = [+p[1], +p[2], +p[3], +p[4]];
    (p[0] === "a" ? accel : gyro).push(row);
  }
  return { meta, accel, gyro, rate: +(meta.rate_hz ?? 804.7) };
}

/** Run the three axes through the high-pass and return the magnitude per sample. */
function envelope(rows, rate) {
  const hp = [
    makeHighPass(HIGHPASS_HZ, rate),
    makeHighPass(HIGHPASS_HZ, rate),
    makeHighPass(HIGHPASS_HZ, rate),
  ];
  return rows.map(([t, x, y, z]) => {
    const a = hp[0](x),
      b = hp[1](y),
      c = hp[2](z);
    return [t, Math.hypot(a, b, c)];
  });
}

/**
 * Downsample by max-pooling, never averaging. These are impulses a few samples wide;
 * an averaging decimator flattens the very spike the chart exists to show.
 */
function pool(series, points) {
  if (series.length <= points) return series.map(([, v]) => v);
  const out = [];
  const step = series.length / points;
  for (let i = 0; i < points; i++) {
    const lo = Math.floor(i * step);
    const hi = Math.min(series.length, Math.floor((i + 1) * step));
    let m = 0;
    for (let j = lo; j < hi; j++) m = Math.max(m, series[j][1]);
    out.push(m);
  }
  return out;
}

function sliceAround(series, tPeak) {
  return series.filter(([t]) => t >= tPeak - LEAD_S && t <= tPeak + TAIL_S);
}

function round(values, dp) {
  const k = 10 ** dp;
  return values.map((v) => Math.round(v * k) / k);
}

/**
 * Build one trace. `at` is the event time in seconds; when null we centre on the
 * loudest moment in the recording.
 */
function build(name, { id, title, at, verdict, note }) {
  const fx = parseFixture(name);
  const accelEnv = envelope(fx.accel, fx.rate);
  const gyroEnv = envelope(fx.gyro, fx.rate);

  let centre = at;
  if (centre == null) {
    let best = 0;
    for (const [t, v] of accelEnv) if (v > best) (best = v), (centre = t);
  }

  const aWin = sliceAround(accelEnv, centre);
  const gWin = sliceAround(gyroEnv, centre);
  const peakG = Math.max(...aWin.map(([, v]) => v));
  const gyroPeak = Math.max(...gWin.map(([, v]) => v));

  return {
    id,
    title,
    note,
    verdict,
    fixture: name,
    at: +centre.toFixed(3),
    peak_g: +peakG.toFixed(4),
    gyro_peak: +gyroPeak.toFixed(2),
    gyro_ratio: +(gyroPeak / peakG).toFixed(1),
    accel: round(pool(aWin, POINTS), 5),
    gyro: round(pool(gWin, POINTS), 3),
  };
}

/**
 * The whole point of porting the filter is that it agrees with the Rust one.
 *
 * Every trace is centred on its recording's loudest moment, so the peak of the window we
 * emit must equal the envelope maximum `yamete analyze` reports for that fixture. If the
 * two ever disagree the port has drifted and the build stops.
 *
 *   for f in slap-hard desk-bump typing trackpad; do
 *     ./target/release/yamete analyze fixtures/$f.fixture.gz | grep envelope
 *   done
 */
const ENVELOPE_MAX_G = {
  "slap-hard": 0.52231,
  "desk-bump": 0.7285,
  typing: 0.03246,
  trackpad: 0.09271,
};

function verify(traces) {
  const errors = [];
  for (const t of traces) {
    const want = ENVELOPE_MAX_G[t.fixture];
    if (want == null) continue;
    if (Math.abs(t.peak_g - want) > 0.002) {
      errors.push(
        `${t.fixture}: window peak is ${t.peak_g} g, but yamete analyze reports an envelope max of ${want} g`,
      );
    }
  }
  if (errors.length) {
    console.error("the JS high-pass has drifted from crates/yamete-dsp/src/filters.rs:");
    for (const e of errors) console.error("  " + e);
    process.exit(1);
  }
}

// Each trace is centred on the loudest moment of its recording — for the slap that is the
// impact itself, ~25 ms after the detector actually fires. The detector deliberately fires
// on the leading edge (a 6 ms peak-hold) rather than waiting for the maximum, so the
// number it reports for an event is smaller than the peak you can see here. The chart
// shows the impact; `votes` and the verdict are what the detector made of it.
const traces = [
  build("slap-hard", {
    id: "slap",
    title: "Slap the lid",
    verdict: "fires",
    note: "Striking the lid torques it about the hinge.",
    at: null,
  }),
  build("desk-bump", {
    id: "desk-bump",
    title: "Bump the desk",
    verdict: "ignored",
    note: "A knock through the desk mostly translates the machine.",
    at: null,
  }),
  build("typing", { id: "typing", title: "Typing", verdict: "ignored", at: null }),
  build("trackpad", { id: "trackpad", title: "Trackpad click", verdict: "ignored", at: null }),
];

verify(traces);

mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, JSON.stringify({ generated_from: "fixtures/", traces }, null, 2) + "\n");

for (const t of traces) {
  console.log(
    `${t.id.padEnd(12)} peak ${t.peak_g.toFixed(4)} g  gyro ${String(t.gyro_peak).padStart(7)} deg/s  ratio ${String(t.gyro_ratio).padStart(7)}  ${t.verdict}`,
  );
}
console.log(`\nwrote ${OUT}`);
