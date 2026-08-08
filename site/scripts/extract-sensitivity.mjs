// Replays the whole corpus at each slider position and records what the detector actually
// did, so the table on the page is a measurement rather than a transcription.
//
// The table in crates/yamete-dsp/src/config.rs is a hand-written comment and has already
// drifted from the code it documents — at 0.50 it claims 36/40 with one false positive
// where the detector now scores 35/40 with none. Copying it onto a landing page would
// publish that drift. This runs the detector instead.
//
// Needs the release binary:  cargo build --release
// Regenerate with:           bun run sensitivity

import { execFileSync } from "node:child_process";
import { existsSync, writeFileSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "../..");
const BIN = join(ROOT, "target/release/yamete");
const FIXTURES = join(ROOT, "fixtures");
const OUT = join(HERE, "../src/data/sensitivity.json");

const STEPS = [
  { at: 0.2, character: "Only a deliberate whack." },
  { at: 0.35, character: "Never fires by accident." },
  { at: 0.5, character: "The default." },
  { at: 0.65, character: "Catches almost everything, desk bumps too." },
  { at: 0.8, character: "Twitchy." },
  { at: 1.0, character: "Anything that shakes the desk." },
];

if (!existsSync(BIN)) {
  console.error(`missing ${BIN}\nbuild it first:  cargo build --release`);
  process.exit(1);
}

const files = readdirSync(FIXTURES)
  .filter((f) => f.endsWith(".fixture.gz"))
  .map((f) => join(FIXTURES, f));

const rows = STEPS.map(({ at, character }) => {
  const stdout = execFileSync(BIN, ["replay", ...files, "--sensitivity", String(at)], {
    encoding: "utf8",
  });
  // "35/40 slaps detected (88% recall), 0 false positive(s)."
  const m = stdout.match(/(\d+)\/(\d+) slaps detected \((\d+)% recall\), (\d+) false positive/);
  if (!m) {
    console.error(`could not read the summary line for sensitivity ${at}:\n${stdout.slice(-400)}`);
    process.exit(1);
  }
  return {
    at,
    caught: +m[1],
    total: +m[2],
    recall: +m[3],
    false_positives: +m[4],
    character,
  };
});

// The corpus these numbers describe, counted rather than asserted.
const silent = files.filter((f) => !/slap-/.test(f));
const corpus = { slaps: rows[0].total, quiet_seconds: silent.length * 30, quiet_files: silent.length };

mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, JSON.stringify({ corpus, rows }, null, 2) + "\n");

for (const r of rows) {
  console.log(
    `${r.at.toFixed(2)}  ${String(r.caught).padStart(2)}/${r.total} caught  ${String(r.false_positives).padStart(2)} false positive(s)`,
  );
}
console.log(`\ncorpus: ${corpus.slaps} slaps, ${corpus.quiet_seconds}s that must stay silent`);
console.log(`wrote ${OUT}`);
