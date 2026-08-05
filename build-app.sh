#!/usr/bin/env bash
# Build Spank.
#
# The sidecar staging order matters: the app spawns the bundled `spankd`, so building the
# app against a stale copy produces a bundle whose daemon rejects arguments the app passes
# it — and fails silently, because the child exits before it can log anything.
#
# This deliberately does not install or launch anything. Installing is done from the DMG,
# by hand; anything that copies into /Applications or opens the app behind the user's back
# fights whatever they are in the middle of doing.
set -euo pipefail
cd "$(dirname "$0")"

TRIPLE=$(rustc -vV | awk '/host:/{print $2}')

# Which certificate to sign with. Tauri reads this; keeping it out of tauri.conf.json
# means the committed config carries no personal identity, and switching to a Developer ID
# for distribution is one variable rather than an edit.
#
# Signing matters beyond Gatekeeper: the Input Monitoring grant that lets the daemon read
# the sensor is tied to the signing identity, so an ad-hoc signature — which changes every
# build — means macOS can re-prompt for permission after each rebuild.
if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
  # Prefer Developer ID when one exists, since that is the only kind that notarizes.
  # `|| true` on each: grep exits 1 when a certificate kind is absent, and under
  # `set -euo pipefail` that aborts the whole script before it can print anything.
  APPLE_SIGNING_IDENTITY=$(
    security find-identity -v -p codesigning 2>/dev/null |
      grep -oE '"Developer ID Application: [^"]+"' | head -1 | tr -d '"' || true
  )
  if [ -z "$APPLE_SIGNING_IDENTITY" ]; then
    APPLE_SIGNING_IDENTITY=$(
      security find-identity -v -p codesigning 2>/dev/null |
        grep -oE '"Apple Development: [^"]+"' | head -1 | tr -d '"' || true
    )
  fi
fi

if [ -n "$APPLE_SIGNING_IDENTITY" ]; then
  export APPLE_SIGNING_IDENTITY
  echo "==> signing as: $APPLE_SIGNING_IDENTITY"
else
  echo "==> no signing identity found; the bundle will be ad-hoc signed"
fi

# Notarisation. Tauri runs notarytool and staples automatically when it finds credentials,
# so this only has to establish whether they are present and say what is missing if not.
#
# Two accepted forms, checked in Apple's order of preference:
#   App Store Connect API key — APPLE_API_ISSUER, APPLE_API_KEY, APPLE_API_KEY_PATH
#   Apple ID                  — APPLE_ID, APPLE_PASSWORD (app-specific), APPLE_TEAM_ID
NOTARIZE=no
case "$APPLE_SIGNING_IDENTITY" in
  "Developer ID Application:"*)
    if [ -n "${APPLE_API_ISSUER:-}" ] && [ -n "${APPLE_API_KEY:-}" ]; then
      NOTARIZE=yes
      echo "==> notarising with an App Store Connect API key"
    elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ]; then
      NOTARIZE=yes
      echo "==> notarising with an Apple ID"
    else
      echo "==> Developer ID found but no notarisation credentials; skipping notarisation"
      echo "    set APPLE_API_ISSUER + APPLE_API_KEY + APPLE_API_KEY_PATH,"
      echo "    or APPLE_ID + APPLE_PASSWORD + APPLE_TEAM_ID"
    fi
    ;;
  *)
    # Only Developer ID certificates can be notarised at all; anything else would have
    # the submission rejected, so there is nothing to warn about.
    ;;
esac

echo "==> daemon"
cargo build --release -p spankd

echo "==> staging sidecar for $TRIPLE"
mkdir -p app/src-tauri/binaries
cp target/release/spankd "app/src-tauri/binaries/spankd-$TRIPLE"
# Anything Tauri copied next to a previous build would shadow the fresh one.
rm -f app/src-tauri/target/release/spankd app/src-tauri/target/debug/spankd

# bundle_dmg.sh fails outright if a volume it wants is already attached. Two ways that
# happens: a Spank volume left mounted after installing a previous build, and scratch
# `dmg.*` volumes orphaned by an interrupted or failed bundle run. The second kind does
# not appear in Finder and accumulates, so each failure makes the next one likelier.
detach_stale_volumes() {
  for vol in /Volumes/Spank /Volumes/dmg.*; do
    if [ -d "$vol" ]; then
      echo "==> detaching stale volume $vol"
      hdiutil detach "$vol" -force >/dev/null 2>&1 || true
    fi
  done
}

echo "==> app"
# A failed bundle run leaves its own scratch volume attached, which then fails the next
# attempt — so clear and retry once rather than making this a two-command ritual.
if ! (cd app && bun run tauri build); then
  echo "==> bundling failed, clearing volumes and retrying"
  detach_stale_volumes
  sleep 2
  (cd app && bun run tauri build)
fi

APP=app/src-tauri/target/release/bundle/macos/Spank.app
DMG=$(ls -t app/src-tauri/target/release/bundle/dmg/*.dmg 2>/dev/null | head -1 || true)

echo
echo "==> verifying"
# --deep --strict walks into the bundled daemon too; signing the outer app while leaving
# the sidecar unsigned passes a shallow check and fails on another machine.
if codesign --verify --deep --strict "$APP" 2>/dev/null; then
  echo "    signature valid (app and sidecar)"
else
  echo "    SIGNATURE INVALID" >&2
fi

if [ "$NOTARIZE" = "yes" ]; then
  if xcrun stapler validate "$APP" >/dev/null 2>&1; then
    echo "    notarisation stapled"
  else
    echo "    NOT STAPLED — the ticket did not attach" >&2
  fi
fi

# The real question for distribution: would Gatekeeper let someone else open this?
if spctl -a -t exec "$APP" >/dev/null 2>&1; then
  echo "    Gatekeeper: accepted — this will open on any Mac"
else
  echo "    Gatekeeper: rejected — fine for local use, blocked if downloaded elsewhere"
fi

echo
if [ -n "$DMG" ]; then
  echo "Built $DMG"
  echo
  echo "Quit any running Spank, then install with:"
  # Printed as a command rather than a bare path: pasting a path into a shell tries to
  # execute it, which fails with a permission error that says nothing useful.
  echo "  open $DMG"
else
  echo "No DMG was produced." >&2
  exit 1
fi
