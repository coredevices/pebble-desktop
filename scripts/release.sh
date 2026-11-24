#!/bin/bash
# Build, Developer ID-sign, notarize and publish Pebble desktop to an eng-dash track.
#
#   scripts/release.sh <track> [notes]        e.g. scripts/release.sh internal "Menu bar reminders"
#   scripts/release.sh promote <version> <track>
#   PUBLISH=0 scripts/release.sh <track>     build, sign, notarize and verify only
#
# Tracks are eng-dash's firmware tracks (nightly, internal, alpha, beta, production).
# Required env (see README "Releases"):
#   TAURI_SIGNING_PRIVATE_KEY (+ TAURI_SIGNING_PRIVATE_KEY_PASSWORD)  updater minisign key
#   APPLE_SIGNING_IDENTITY   "Developer ID Application: <Team> (<TEAMID>)", in the login keychain
#   APPLE_API_ISSUER, APPLE_API_KEY, APPLE_API_KEY_PATH            notarization (App Store Connect key)
#   DESKTOP_RELEASE_SECRET   eng-dash publish secret
set -euo pipefail
cd "$(dirname "$0")/.."
# A pip-installed xattr shadows the system one and breaks `tauri build`.
export PATH="/usr/bin:$PATH"
# Cargo's release debuginfo strip corrupts proc-macro dylibs on this macOS
# ("mis-aligned LINKEDIT string pool"); keep symbols until that is fixed.
export CARGO_PROFILE_RELEASE_STRIP=false
ENG_DASH="${ENG_DASH_URL:-https://dash.repebble.com}"

publish() { # json body; prints response
  curl -sS --fail-with-body -X POST "$ENG_DASH/api/desktop/releases" \
    -H "Authorization: Bearer $DESKTOP_RELEASE_SECRET" -H 'Content-Type: application/json' -d "$1"
}

: "${DESKTOP_RELEASE_SECRET:?}"
if [[ "${1:-}" == promote ]]; then
  publish "$(jq -n --arg v "$2" --arg t "$3" '{version:$v, track:$t}')"; echo
  exit
fi
TRACK="${1:?usage: scripts/release.sh <track> [notes]}"
NOTES="${2:-}"
: "${TAURI_SIGNING_PRIVATE_KEY:?}" "${APPLE_SIGNING_IDENTITY:?}" "${APPLE_API_ISSUER:?}" "${APPLE_API_KEY:?}" "${APPLE_API_KEY_PATH:?}"

VERSION=$(jq -r .version src-tauri/tauri.conf.json)
ARCH=$(uname -m); [[ "$ARCH" == arm64 ]] && ARCH=aarch64
PLATFORM="darwin-$ARCH"

# Release-only overrides: the checked-in config stays ad-hoc signed so plain
# `npm run build` works without any keys.
npx tauri build --bundles app --config "$(jq -n --arg id "$APPLE_SIGNING_IDENTITY" \
  '{bundle:{createUpdaterArtifacts:true, macOS:{signingIdentity:$id}}}')"

BUNDLE="${CARGO_TARGET_DIR:-target}/release/bundle/macos"
codesign --verify --deep --strict "$BUNDLE/Pebble.app"
codesign -d --entitlements - "$BUNDLE/Pebble.app" 2>/dev/null | grep -q allow-jit
xcrun stapler validate "$BUNDLE/Pebble.app"
spctl --assess --type execute "$BUNDLE/Pebble.app"

TARBALL="$BUNDLE/Pebble.app.tar.gz"
if [[ "${PUBLISH:-1}" == 0 ]]; then echo "Built and verified $TARBALL (PUBLISH=0, not uploaded)"; exit; fi
scripts/publish.sh "$TRACK" "$PLATFORM" "$TARBALL" "$NOTES"
