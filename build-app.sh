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

echo "==> daemon"
cargo build --release -p spankd

echo "==> staging sidecar for $TRIPLE"
mkdir -p app/src-tauri/binaries
cp target/release/spankd "app/src-tauri/binaries/spankd-$TRIPLE"
# Anything Tauri copied next to a previous build would shadow the fresh one.
rm -f app/src-tauri/target/release/spankd app/src-tauri/target/debug/spankd

# bundle_dmg.sh fails outright if a volume it wants is already attached. Two ways that
# happens: a Spank volume left mounted after installing a previous build, and scratch
# `dmg.*` volumes orphaned by an interrupted bundle run. The second kind does not appear
# in Finder and accumulates, so each failure makes the next one likelier.
for vol in /Volumes/Spank /Volumes/dmg.*; do
  if [ -d "$vol" ]; then
    echo "==> detaching stale volume $vol"
    hdiutil detach "$vol" -force >/dev/null 2>&1 || true
  fi
done

echo "==> app"
(cd app && bun run tauri build)

DMG=$(ls -t app/src-tauri/target/release/bundle/dmg/*.dmg 2>/dev/null | head -1 || true)
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
