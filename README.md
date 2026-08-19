# yamete

やめて — *stop it*.

Spank detection for Apple Silicon MacBooks. Hit the laptop, it makes a noise.

The interesting part is that it works at all: the sensor it reads is an undocumented
Bosch IMU behind the Sensor Processing Unit, invisible to CoreMotion, reachable only as
vendor-usage-page HID devices. Telling a spank apart from someone thumping the desk turns
out to need the gyroscope, which is not where you would first look.

## Requirements

An Apple Silicon **laptop**, macOS 13 or later. The sensor exists on M2 and later, plus
M1 Pro and M1 Max. It is absent from the M1 MacBook Pro (2020), the M1 Air, every desktop
Mac, and every Intel Mac — `yamete probe` says so plainly rather than hanging.

The first launch asks for **Input Monitoring**. macOS gates all HID access behind it,
including a sensor that is not an input device in any useful sense.

## What's here

| | |
|---|---|
| `crates/yamete-sensor` | IOKit HID access to the accelerometer and gyroscope, ~805 Hz |
| `crates/yamete-dsp` | The detector. No platform or I/O dependencies, so it can be replayed against recordings in a test |
| `crates/yamete-proto` | Wire types for the control socket |
| `crates/yamete` | The daemon, its actions, and the tuning tools |
| `app/` | Yamete, the Tauri menu bar app |
| `fixtures/` | 40 annotated slaps and 150 s of things that must stay silent |

## How it fits together

The daemon does everything real: reads the sensor, runs the detector, fires actions. The
app is a controller that attaches over a unix socket.

The app **bundles the daemon as a sidecar and owns its lifetime** — opening Yamete starts
detection, quitting it stops detection. The daemon is spawned with a stdin pipe and
`--exit-with-parent`, so the pipe closing kills it however the app dies, including a force
quit. If a daemon is already running (a LaunchAgent, or one started from a terminal) the
app attaches to it and leaves it alone on exit.

| | |
|---|---|
| socket | `~/Library/Application Support/com.chipcolate.yamete/yamete.sock` |
| config | `…/com.chipcolate.yamete/config.json` |
| logs | `~/Library/Logs/yamete/yamete.log` (previous run kept as `.log.1`) |

The protocol is newline-delimited JSON, so the daemon can be driven by hand:

```sh
nc -U ~/Library/Application\ Support/com.chipcolate.yamete/yamete.sock
{"cmd":"get_status"}
{"cmd":"subscribe","spanks":true}
```

## Building

```sh
./build-app.sh
```

Produces a signed `.app` and `.dmg`, then reports whether the signature is valid, whether
a notarisation ticket is stapled, and whether Gatekeeper would accept it. It deliberately
does **not** install or launch anything — install by opening the DMG and dragging across.

The script stages the daemon into the app bundle before building it. That order matters:
the app spawns the bundled `yamete`, so building against a stale copy produces a bundle
whose daemon rejects arguments the app passes it, and fails silently because the child
exits before it can log anything.

```sh
cargo test --workspace      # unit tests plus a replay of the whole fixture corpus
cargo clippy --workspace --all-targets -- -D warnings
cd app && bun run tauri dev # the app against a live frontend, for UI work
```

CI runs those on every PR against `main`, plus an unsigned bundle build — bundling is the
only thing that exercises `tauri.conf.json`, the capability set, the icon list and the
sidecar staging, none of which the compiler sees.

Tagging `v*` builds, signs, notarises and publishes a GitHub Release with the DMG and its
checksum. That needs six repository secrets: `APPLE_CERTIFICATE` (base64 of the Developer
ID `.p12`), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_API_ISSUER`,
`APPLE_API_KEY` and `APPLE_API_KEY_CONTENT` (the `.p8` file's contents). Without them the
release still builds, just unsigned — and so it will not open on anyone else's Mac. The
tag has to match the version in both `Cargo.toml` and `tauri.conf.json`.

## Signing and notarisation

A signing identity is picked from the keychain automatically, preferring a
`Developer ID Application` certificate over `Apple Development`. Override with
`APPLE_SIGNING_IDENTITY`.

Signing matters beyond Gatekeeper: the Input Monitoring grant that lets the daemon read
the sensor is tied to the signing identity, and an ad-hoc signature changes every build —
so macOS can re-prompt for permission after each rebuild.

To produce something that opens on someone else's Mac you need a Developer ID certificate
**and** notarisation credentials — a Developer ID signature alone is not enough, since
Gatekeeper has also required a notarisation ticket from macOS 10.15 onwards. Create the
certificate at
[developer.apple.com → Certificates](https://developer.apple.com/account/resources/certificates)
(*Developer ID Application*, G2 Sub-CA), download it, and double-click to install.

Then either an App Store Connect API key:

```sh
export APPLE_API_ISSUER=...   # the issuer UUID
export APPLE_API_KEY=...      # the key ID
export APPLE_API_KEY_PATH=~/.appstoreconnect/private_keys/AuthKey_XXXXXXXXXX.p8
```

or an Apple ID with an [app-specific password](https://appleid.apple.com):

```sh
export APPLE_ID=you@example.com
export APPLE_PASSWORD=abcd-efgh-ijkl-mnop
export APPLE_TEAM_ID=XXXXXXXXXX
```

The build then notarises and staples, and the verification step should report
`Gatekeeper: accepted`. Both the app and the DMG are submitted, so that is two round trips
to Apple; queue times vary from minutes to considerably longer.

## The daemon on its own

`yamete` runs happily without the app, which is how the detector is developed.

```sh
yamete probe            # is the sensor there, at what rate, decoding correctly
yamete watch --scores   # live detections, with the five detector scores
yamete status           # what the running daemon thinks
yamete listen           # stream detections as they happen
yamete install --copy   # run it permanently as a LaunchAgent, independent of the app
```

A LaunchAgent is only needed if you want detection without the app running. It is a *user*
agent rather than a system daemon, which is not merely simpler but necessary: Input
Monitoring is a per-user GUI consent, and a root daemon has no login session to prompt in.

## Tuning

Thresholds are derived from recordings, not guessed. The obvious-looking values do not
survive contact with the hardware: a 0.005 g micro-shock floor sits around the 95th
percentile of an *idle* laptop, so a detector built on it fires on nothing but noise.

```sh
yamete record-suite                                # record the corpus, with a metronome
yamete analyze fixtures/idle.fixture.gz            # what the detectors read when quiet
yamete analyze fixtures/slap-*.gz --at-detections  # and at a real slap
yamete sweep fixtures/*.gz --knob gyro-ratio       # score a threshold against everything
yamete replay fixtures/*.gz -v                     # what the current settings would do
```

`cargo test` replays the whole corpus and holds the detector to a measured envelope:
everyday activity silent, false positives within budget, recall above target. The suite
skips rather than fails when `fixtures/` is empty, so a fresh clone still passes.

`--at-detections` is the one worth knowing about. Percentiles over a whole recording are
dominated by the quiet 98 % of it and say nothing about whether a threshold is *reachable*;
sampling the detectors at the moment one fires is what tells you whether a statistic
contributes at all.

## License

Apache License 2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).

Copyright 2026 CHIPCOLATE SRL.
