# Working on yamete

Slap detection for Apple Silicon MacBooks. See `README.md` for what it is and how to
build it. This file is the accumulated "you will get this wrong otherwise" — most of it
was learned by measuring something that contradicted a reasonable assumption.

## Commands

```sh
cargo test --workspace                 # 135 tests, includes replaying the fixture corpus
./build-app.sh                         # signed .app + .dmg; does not install or launch
cd app && bun run tauri dev            # app with live frontend reload
cd app && bunx tsc --noEmit            # typecheck the frontend
yamete probe                           # prove the sensor works on this machine
```

## Working preferences

- **First principles over convenience.** Given a choice between a clean design that costs
  short-term pain and a compatible one that leaves a wart, take the clean one. Do not
  assume the user would rather avoid redoing setup — they would not.
- **Never install or launch the app.** `build-app.sh` builds and prints the install
  command. Copying into `/Applications` or running `open -a` fights whatever the user is
  doing, and a running app blocks replacing its own bundle.
- **Solve it in the right layer.** When something is needed by one action kind, ask
  whether it belongs to all of them. The response delay started as a sound setting and
  belongs on the action.
- **Do not do things unprompted that make noise or take over.** Playing a sound when a
  file is added, opening a DMG, launching a window.

## The sensor

Everything here is undocumented and was established by experiment.

- **Root is not required.** Published projects that read this sensor enforce
  `geteuid() == 0` and document sudo as necessary. macOS does not require it — verified
  streaming at 807 Hz as an ordinary user with no entitlements. A *user* LaunchAgent is
  also strictly better than a root daemon, because HID access is gated by the Input
  Monitoring TCC grant, which is a per-user GUI consent a root daemon cannot prompt for.
- **Wake `AppleSPUHIDDriver`, not the `IOHIDDevice`, and do it before opening.** Setting
  `SensorPropertyReportingState`, `SensorPropertyPowerState` and `ReportInterval` via
  `IOHIDDeviceSetProperty` is silently ignored. The failure mode is an open that succeeds
  followed by a callback that never fires once.
- **Match on class + usage page + usage + report size.** The product ID varies by model.
  Usage page `0xFF00`, usage 3 is the accelerometer and 9 the gyroscope, both 22 bytes.
  Usage page and usage alone match two devices on a Mac16,5.
- **Report layout**: `u16` sequence counter at 0, `i32` LE x/y/z at 6/10/14 as IOFixed
  16.16 (divide by 65536), and bytes 18..22 are **die temperature, not a timestamp** — it
  is non-monotonic and bit-identical between the two devices at the same instant.
- Never seize the device; that would steal reports from the system's own consumer.

## The detector

- **Measure thresholds, never assume them.** Published constants for this problem all sit
  below this machine's noise floor. `yamete analyze` on a fixture annotated `expect=0`
  prints each statistic's idle maximum and flags any threshold underneath one.
- **A statistic is only useful if it separates the two populations.** Measured at the
  moment of a real slap, CUSUM reads *lower* than during idle noise, and kurtosis is a
  coin flip. Use `analyze --at-detections`, because whole-fixture percentiles are
  dominated by the quiet 98 % and say nothing about reachability.
- **The gyroscope is the discriminator, not amplitude.** In the committed corpus a desk
  bump peaks at 0.73 g where a hard slap is 0.52 g — the rejected impact is larger — while
  rotation goes the other way (≈15 °/s vs ≈48 °/s). Striking the lid applies a torque about
  the hinge; a knock through the desk mostly translates the machine. Gate on
  `gyro_ratio_min` ∪ `gyro_peak_min` (defaults 175 °/s per g and 15 °/s); re-measure with
  `yamete analyze` / `sweep` rather than trusting a published °/s-per-g band.
- **Severity is amplitude; votes are confidence.** Gating tiers on how many detectors
  agreed reported a 0.62 g whack as a micro shock. They answer different questions.
- **The sensitivity slider must scale the gyro gate too.** Scaling only the amplitude
  thresholds leaves it nearly inert, because the gyro gate is what rejects marginal hits.
- **Lowering thresholds to make more detectors vote is a trap.** It was measured and it
  cost a false positive on trackpad clicks; strict thresholds answer "impact or noise?"
  better, and severity no longer depends on the count.
- CUSUM needs a leak term. `max(0, S + z − k)` is a rectified random walk and drifts
  without bound on a one-sided vibration envelope — past 1000 in six seconds of silence.

## The daemon

- **Idle CPU is dominated by the sensor, not the DSP.** Reading two HID devices at 805 Hz
  is ~3.15 % of a core; the entire five-detector ensemble, socket server and action layer
  add ~0.35 %. `report_interval_us` is the only knob that meaningfully changes power.
- **Never hold the audio device open.** An open CoreAudio output stream runs its render
  callback continuously whether or not anything is playing — 4.2 % of a core for silence.
  It is opened on demand and released after 30 s idle.
- **Opening the audio device costs 70–380 ms** depending on what else holds it, so a
  delayed sound pre-warms it *during* the delay rather than paying it afterwards.
- Telemetry is opt-in and reference-counted. With nobody watching, the daemon does no
  telemetry work at all; the count is in `yamete status`, which is the fastest way to tell
  "nobody asked" from "asked and lost".
- Validation rejects **malformed** config, not **incomplete** config. An action switched
  on but not filled in is a half-finished edit — rejecting the whole config for it means
  you cannot type a URL into a field that refuses to save.

## The app

Bugs here consistently presented as "the daemon is broken" and consistently were not.

- **Tauri events have no replay.** The subscription connects before the webview registers
  its listeners, so a `daemon-connection` emitted at startup goes into the void. Query
  state directly at startup rather than waiting for an event that already fired.
- **`document.visibilityState` is not window visibility.** It reflects the web view's
  occlusion and does not fire on a native show/hide. Ask the window system.
- **A read timeout means `read_line` can return mid-frame.** Clearing the line buffer at
  the top of the loop discards bytes already consumed from the socket. This shredded
  590-byte telemetry frames while leaving small, rare slap events pristine.
- **The `hidden` attribute is defeated by any CSS rule setting `display`** — the UA rule
  `[hidden] { display: none }` has the same specificity as a class selector, so a later
  stylesheet wins. There is a global `[hidden] { display: none !important }` for this.
- **A template image is drawn from its alpha channel alone**, so the colour app icon
  cannot be the tray icon; it would render as a featureless blob. `icons/tray@2x.png` is a
  separate monochrome glyph.
- Startup runs each step independently guarded. A single `async` IIFE prefixed with `void`
  swallows a rejection and silently skips everything after it.

## macOS traps

- **Overwriting a running binary corrupts it.** `cp` over a running Mach-O rewrites its
  inode, invalidating the code signature; macOS then SIGKILLs every exec of that path,
  with no output at all. Unlink first. `yamete install --copy` does.
- **`launchctl bootout` is asynchronous.** Bootstrapping immediately after fails with
  `Bootstrap failed: 5: Input/output error`, which says nothing about the cause. Poll
  until the job is gone.
- **`KeepAlive` must be a dict**, not `<true/>`. A bare true restarts after a *clean* exit
  too, so a Mac without the sensor respawns forever. `NoSensor` therefore exits **zero**.
- **`bundle_dmg.sh` fails if a volume it wants is attached** — including scratch `dmg.*`
  volumes orphaned by a previous failure, which do not appear in Finder and accumulate.
- **macOS `sed` does not support `\b`.** A word-boundary replacement silently matches
  nothing while reporting success. Use Python for anything careful.
- Notarisation needs a **Developer ID** certificate; Apple Development signs validly and
  Gatekeeper still refuses it. Both the app and the DMG are submitted separately.

## Fixtures

`fixtures/*.fixture.gz` is the only evidence the detector works, and every threshold came
from it. Committed gzipped, ~3.4 MB. The reader sniffs the magic bytes, so plain and
gzipped both load, and it still accepts the pre-rename `# spank-fixture v1` header.

Recorded on a Mac16,5. They contain lighter slaps (0.04–0.07 g) than typical live hits
(0.28–0.62 g), so the upper tiers are under-exercised by them.

## Not ours

The detector statistics (STA/LTA, CUSUM, kurtosis, peak/MAD, envelope) are classic impact /
seismic-style measures. Thresholds, the gyro corroboration gate, and the shipping defaults
were measured on this hardware. Other projects in the Mac slap-detection space were
measured against this machine; their published thresholds sat below the noise floor and
**none of their thresholds or code were adopted**. Do not add attributions implying
otherwise.
