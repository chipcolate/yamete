# yamete

やめて — *stop it*.

Slap detection for Apple Silicon MacBooks. Hit the laptop, it makes a noise.

The interesting part is that it works at all: the sensor it reads is an undocumented
Bosch IMU behind the Sensor Processing Unit, invisible to CoreMotion, reachable only as
vendor-usage-page HID devices. Telling a slap apart from someone thumping the desk turns
out to need the gyroscope, which no comparable project uses.

## What's here

| | |
|---|---|
| `crates/yamete-sensor` | IOKit HID access to the accelerometer and gyroscope, ~805 Hz |
| `crates/yamete-dsp` | The detector. No platform or I/O dependencies, so it can be replayed against recordings in a test |
| `crates/yamete-proto` | Wire types for the control socket |
| `crates/yamete` | The daemon, its actions, and the tuning tools |
| `app/` | Yamete, the Tauri menu bar app |
| `fixtures/` | 40 annotated slaps and 150 s of things that must stay silent |

## Building

```sh
./build-app.sh
```

Produces a signed `.app` and `.dmg`, then reports whether the signature is valid and
whether Gatekeeper would accept it. Install by opening the DMG and dragging across.

The script stages the daemon into the app bundle before building it. That order matters:
the app spawns the bundled `yamete`, so building against a stale copy produces a bundle
whose daemon rejects arguments the app passes it, and fails silently because the child
exits before it can log anything.

## Signing and notarisation

A signing identity is picked from the keychain automatically, preferring a
`Developer ID Application` certificate over `Apple Development`. Override with
`APPLE_SIGNING_IDENTITY`.

Signing matters beyond Gatekeeper: the Input Monitoring grant that lets the daemon read
the sensor is tied to the signing identity, and an ad-hoc signature changes every build —
so macOS can re-prompt for permission after each rebuild.

To produce something that opens on someone else's Mac you need a Developer ID certificate
**and** notarisation credentials. Create the certificate at
[developer.apple.com → Certificates](https://developer.apple.com/account/resources/certificates)
(*Developer ID Application*), download it, and double-click to install.

Then either an App Store Connect API key:

```sh
export APPLE_API_ISSUER=...        # the issuer UUID
export APPLE_API_KEY=...           # the key ID, e.g. ABC123DEF4
export APPLE_API_KEY_PATH=~/private_keys/AuthKey_ABC123DEF4.p8
```

or an Apple ID with an [app-specific password](https://appleid.apple.com):

```sh
export APPLE_ID=you@example.com
export APPLE_PASSWORD=abcd-efgh-ijkl-mnop
export APPLE_TEAM_ID=HNF5BY9XXV
```

`./build-app.sh` then notarises and staples, and the verification step reports
`Gatekeeper: accepted`. Notarisation adds a few minutes to the build.

## The daemon on its own

`yamete` runs happily without the app, which is how the detector is developed.

```sh
yamete probe            # is the sensor there, at what rate, decoding correctly
yamete watch --scores   # live detections, with the five detector scores
yamete status           # what the running daemon thinks
yamete listen           # stream detections as they happen
yamete install --copy   # run it permanently as a LaunchAgent, independent of the app
```

The app bundles its own copy and manages its lifetime, so a LaunchAgent is only needed if
you want detection without the app running.

## Tuning

Thresholds are derived from recordings, not guessed. The workflow:

```sh
yamete record-suite                      # record the corpus, with a metronome for slaps
yamete analyze fixtures/idle.fixture.gz  # what the detectors read on a quiet machine
yamete analyze fixtures/slap-*.gz --at-detections   # and at a real slap
yamete sweep fixtures/*.gz --knob gyro-ratio        # score a threshold against everything
yamete replay fixtures/*.gz -v           # what the current settings would do
```

`cargo test` replays the whole corpus and holds the detector to a measured envelope:
everyday activity silent, false positives within budget, recall above target.

The published thresholds from comparable projects all sit **below** this machine's noise
floor — their 0.005 g micro-shock floor is around the 95th percentile of an idle laptop —
which is why none of them were adopted as-is.
