#!/usr/bin/env bash
# Build and install Spank.
#
# The sidecar staging order matters: the app spawns the bundled `spankd`, so building the
# app against a stale copy produces a bundle whose daemon rejects arguments the app passes
# it — and fails silently, because the child exits before it can log anything.
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

# bundle_dmg.sh fails outright if a volume of the same name is already attached, which
# happens routinely after installing from a previous build.
if [ -d /Volumes/Spank ]; then
  echo "==> detaching a mounted Spank volume"
  hdiutil detach /Volumes/Spank -force >/dev/null 2>&1 || true
  sleep 1
fi

echo "==> app"
(cd app && bun run tauri build)

if [ "${1:-}" = "--install" ]; then
  echo "==> installing"
  pkill -f 'Spank.app/Contents/MacOS/spank-app' 2>/dev/null || true
  sleep 1
  rm -rf /Applications/Spank.app
  cp -R app/src-tauri/target/release/bundle/macos/Spank.app /Applications/
  # Keep the build copy out of Launch Services so only one Spank is discoverable.
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -u "$PWD/app/src-tauri/target/release/bundle/macos/Spank.app" 2>/dev/null || true
  rm -f "$HOME/Library/Application Support/com.chipcolate.spank/app.sock"
  echo "installed to /Applications/Spank.app"
fi
