# yamete.app

The landing page. Astro, static, no UI framework — the same no-framework stance as the app
frontend. Deployed to GitHub Pages by `.github/workflows/pages.yml` on every push to
`main` that touches `site/`.

```sh
bun install
bun run dev        # localhost:4321
bun run build      # → dist/
bun run check      # astro check, 0 errors expected
```

## Everything on the page is measured

No number on this page was typed by hand. Three generators produce the data and the
artwork, and their output is committed so the site builds without Rust, Python or the
fixture corpus.

```sh
bun run extract      # fixtures/*.fixture.gz  → src/data/traces.json
bun run sensitivity  # replays the corpus     → src/data/sensitivity.json   (needs the binary)
bun run logotype     # やめて                  → src/assets/yamete-logotype.svg
bun run og           # app artwork            → public/og.png, public/favicon.png
```

`bun run sensitivity` shells out to the detector, so build it first:

```sh
cargo build --release
```

### The traces are the same signal the detector votes on

`extract-traces.mjs` ports the 5 Hz DC-blocker from `crates/yamete-dsp/src/filters.rs`
into JavaScript, because plotting raw accelerometer magnitude would be misleading — it is
dominated by the gravity vector swinging as the lid moves, and reads a slap at 0.52 g
where the detector sees 0.08 g.

A port that silently drifts from the original would be worse than no port at all. The
script keeps two checks: JS peaks must match golden envelope maxima captured from
`yamete analyze`, and when `target/release/yamete` is present those goldens are
re-checked live against analyze (so a stale golden cannot hide a drifted filter). Pages
CI only has the golden check — the workspace does not build off macOS. CI also fails if
the committed `traces.json` is stale relative to the fixtures.

### The sensitivity table is replayed, not transcribed

The table in `crates/yamete-dsp/src/config.rs` is a hand-written comment and has drifted
from the code it documents — at 0.50 it claims 36/40 with one false positive where the
detector now scores 35/40 with none. `extract-sensitivity.mjs` runs the detector instead,
so the page cannot repeat that drift.

### やめて is outlines, not a webfont

The Zen Maru Gothic Japanese subset is 1.4 MB at weight 700, for three characters used
once. `make-logotype.py` extracts them as SVG paths (2.9 KB) that inherit `currentColor`.
Only Latin subsets are loaded as fonts.

## DNS

`public/CNAME` claims `yamete.app`. Because that is an apex domain it needs **A records**,
not a CNAME:

```
A     yamete.app    185.199.108.153
A     yamete.app    185.199.109.153
A     yamete.app    185.199.110.153
A     yamete.app    185.199.111.153
AAAA  yamete.app    2606:50c0:8000::153
AAAA  yamete.app    2606:50c0:8001::153
AAAA  yamete.app    2606:50c0:8002::153
AAAA  yamete.app    2606:50c0:8003::153
```

Add `CNAME www.yamete.app → chipcolate.github.io` if you want the `www` host to redirect.
Then enable Pages for the repository (Settings → Pages → Source: GitHub Actions) and tick
"Enforce HTTPS" once the certificate is issued.

The repository must be public for Pages to serve on a free plan.

## The download button 404s until there is a release

The download CTAs point at `/releases/latest` (the release page). Checksums point at
`/releases/latest/download/SHA256SUMS`, the fixed asset name `release.yml` publishes.
Both resolve as soon as a `v*` tag is pushed. Nothing on the page needs editing at
release time.
