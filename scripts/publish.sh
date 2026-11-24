#!/bin/bash
# Upload one platform's signed updater bundle to eng-dash and deploy it to a track.
#
#   scripts/publish.sh <track> <platform> <bundle> [notes]
#   e.g. scripts/publish.sh internal darwin-aarch64 target/release/bundle/macos/Pebble.app.tar.gz
#
# <bundle>.sig must sit next to the bundle (Tauri writes it with createUpdaterArtifacts).
# Used by scripts/release.sh (signer Mac) and .github/workflows/release.yml (CI).
set -euo pipefail
cd "$(dirname "$0")/.."
: "${DESKTOP_RELEASE_SECRET:?}"
ENG_DASH="${ENG_DASH_URL:-https://dash.repebble.com}"
TRACK="${1:?usage: scripts/publish.sh <track> <platform> <bundle> [notes]}"
PLATFORM="${2:?}"
BUNDLE="${3:?}"
NOTES="${4:-}"
VERSION=$(jq -r .version src-tauri/tauri.conf.json)
SHA=$( (sha256sum "$BUNDLE" 2>/dev/null || shasum -a 256 "$BUNDLE") | cut -d' ' -f1)

publish() {
  curl -sS --fail-with-body -X POST "$ENG_DASH/api/desktop/releases" \
    -H "Authorization: Bearer $DESKTOP_RELEASE_SECRET" -H 'Content-Type: application/json' -d "$1"
}
BODY=$(jq -n --arg v "$VERSION" --arg t "$TRACK" --arg n "$NOTES" --arg p "$PLATFORM" --arg s "$SHA" \
  --rawfile sig "$BUNDLE.sig" '{version:$v, track:$t, notes:$n, artifact:{platform:$p, sha256:$s, signature:$sig}}')
RESP=$(publish "$BODY")
URL=$(jq -r '.upload_url // empty' <<<"$RESP")
if [[ -n "$URL" ]]; then
  curl -fsS -X PUT --upload-file "$BUNDLE" "$URL"
  RESP=$(publish "$BODY") # eng-dash hashes the upload, then deploys
fi
echo "$RESP"
jq -e '.status == "deployed"' <<<"$RESP" >/dev/null
