# Pebble Desktop

A Tauri 2 macOS app that wraps **Index's existing React/Next.js UI**, with the
byte-verified Rust iMessage activation-key extractor linked directly into it.

## Develop

Keep the sibling `../index-webapp` checkout next to this repository:

```sh
# Terminal 1 — same web app as index.repebble.com
cd ../index-webapp
npm ci
npm run dev -- --hostname localhost --port 3017

# Terminal 2 — this repository
npm ci
npm run dev
```

`npm run build` creates `target/release/bundle/macos/Pebble.app`, loading
`https://index.repebble.com`. Deploy the accompanying index-webapp changes before
shipping it. `npm run build -- --debug` creates a local test app loading port 3017.
This is a remote web wrapper: the first load and sign-in require a connection.
No Node server is embedded in the Mac app.

The app has a normal macOS title bar and Index navigation. Only the macOS
installation exposes the iMessage section at the top of Settings. Regular web
browsers, including browsers on Macs, do not expose it.

## iMessage setup

- On by default, separately for each signed-in account. Preferences survive restarts.
- Sign-in opens the existing Pebble browser authentication flow (Google, GitHub,
  Apple), then returns through a random, short-lived loopback callback. The
  broker currently labels its authorization page "CLI Access".
- A Firebase identity is checked against the existing Pebble `users/{uid}` record
  before sign-in completes or any automatic extraction/upload happens. Missing
  accounts show a warning; network failures are never treated as new accounts.
- Rust extracts locally and uploads using the signed-in user's Firebase ID token.
  There are no embedded service-account credentials. The raw key is never logged.
- One document per app installation/account in
  `chat_activation_keys/{uid}/keys/{installationUUID}` stores `computerName`,
  `macValidationData`, `formatVersion: 1`, `platform: "macOS"`, and `createdAt`
  (a server timestamp). Different Macs get different records. Refreshing does
  not add duplicates; unchanged keys keep their original saved timestamp.
- Turning Off removes this installation's saved key. Failed removals are retried
  after restart and never silently re-enable sharing. Turning Off cannot revoke
  a key that a phone has already copied into its own setup.
- "Show setup key" explicitly reveals the exact key as a QR code and copyable
  string. QR generation is local. Hiding it, navigating away, or signing out
  removes it from component state. Only clipboard writing is permitted; the app
  cannot read the user's clipboard.
- This is one-time setup. The Mac does **not** relay messages and can be turned off.

The native bridge is restricted to the main window and the exact Index HTTPS
origin. Localhost permissions only exist in debug binaries. Other links open in
the system browser and do not gain native permissions.

## Verify

Requires macOS, Xcode command-line tools, Rust, CMake/libclang (for Unicorn), and
Java 21+ for the Firestore emulator.

```sh
cargo test --workspace -j 2
cargo clippy --workspace --all-targets -j 2 -- -D warnings
cargo fmt --all --check
firebase emulators:exec --only firestore --project demo-pebble-desktop \
  'node --test firebase/rules.test.mjs'
# Save from the original Corten ExtractKey Mac app, then compare every byte:
cargo build --release -p pebble-mac-key
python3 scripts/verify-reference.py /path/to/corten-gui.txt
```

`firebase/firestore.rules` is a standalone emulator fixture. **Do not deploy it
as a replacement for the production application's rules.** Merge only the
`chat_activation_keys` match into the current rules, preserving all existing
matches. Tests also accept `PEBBLE_RULES_FILE` to validate a merged rules file.

The CLI `target/release/pebble-extract-key` emits only the base64 token with no
newline, matching the original Corten GUI save exactly. The library entry point
is `pebble_mac_key::extract_key() -> Result<String, pebble_mac_key::Error>`;
Tauri calls it on a blocking worker. No Rosetta, Go, Swift or original app is
required at runtime.

Local bundles use ad-hoc signing for development. Developer ID signing,
notarization and updates are handled by `scripts/release.sh` (see below). See [UPSTREAM.md](UPSTREAM.md) for source provenance
and third-party notices, and [VERIFICATION.md](VERIFICATION.md) for extractor parity.

## Releases and auto-update

Installed apps update themselves through eng-dash (dash.repebble.com), on the
same tracks as firmware: `nightly`, `internal`, `alpha`, `beta`, `production`.

- The app asks `https://dash.repebble.com/api/desktop/update/{target}/{arch}/{version}`
  at launch and every 6 hours (anonymous, so production track), and whenever
  Index calls the `check_for_updates` command with the signed-in user's
  Firebase ID token. eng-dash maps that user's tester channel to a track
  (as firmware does) and offers the newest build on that track or any more
  public one.
- `tauri-plugin-updater` downloads the bundle, checks its minisign signature
  against `plugins.updater.pubkey` in `tauri.conf.json` and replaces the app.
  Pebble then asks to restart, or uses the new version next launch. It only
  moves to a strictly newer version; the server cannot roll users back.
- Debug builds never check.
- Automatic by default: the update downloads in the background, then installs
  and restarts Pebble once no Pebble window is showing or you've stepped away
  (see `presence.rs`). Settings → Updates turns this off (`updates.json` in the
  app config folder).
- eng-dash is the only update endpoint; GitHub releases host first-time
  downloads.

Releases are built and signed on the lab Mac `unicorn-mac-2`, set up from the
unicorn repo (`desktop-signer/`). Bump `version` in `src-tauri/tauri.conf.json`
(eng-dash keeps each version's bytes immutable), then from `~/dev/unicorn`:

```sh
./desktop-signer/release.sh internal "What changed"   # build, sign, notarize, upload, deploy
./desktop-signer/release.sh promote 0.2.0 beta        # move an existing build to another track
```

That copies this tree to the signer and runs `scripts/release.sh` there. The
script builds with Developer ID signing (keeping the `allow-jit` entitlement)
and updater artifacts, notarizes and staples, checks the result with
`codesign`, `stapler` and `spctl`, then uploads `Pebble.app.tar.gz` and its
`.sig` to eng-dash. `PUBLISH=0` stops after the checks. The checked-in config
stays ad-hoc signed, so `npm run build` needs no keys.

### GitHub Actions (macOS, Windows, Linux)

`.github/workflows/release.yml` builds all three. Push a tag `vX.Y.Z` that matches
`version` in `tauri.conf.json`: each platform's signed updater bundle goes to
eng-dash on the `production` track, and a GitHub release gets the installers
(`.dmg` for Apple silicon and Intel, Windows `-setup.exe`, Linux `.AppImage`;
no `.deb`, since a .deb install can't apply AppImage updates). Running the workflow by hand publishes to a tester
track (`internal`, `alpha`, `beta`) with no GitHub release.

macOS builds are Developer ID signed and notarized. Windows and Linux installers
are not code-signed (Windows SmartScreen warns on first run); every platform's
updates are still verified against the updater key.

Repository secrets: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`,
`APPLE_CERTS_P12` (base64 `.p12` holding the Developer ID Application identity; the same bundle CoreApp CI uses), `APPLE_CERTS_P12_PW`,
`APPLE_SIGNING_IDENTITY`, `APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_P8`
(contents of the `.p8`), `DESKTOP_RELEASE_SECRET`.

### Keys and secrets (never commit them)

| What | Where it lives | Script env |
|---|---|---|
| Updater minisign private key and password | Bitwarden; `~/.pebble-desktop-signing/` on Eric's laptop and the signer | `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` |
| Updater public key | `plugins.updater.pubkey` in `src-tauri/tauri.conf.json` (committed) | n/a |
| Developer ID Application certificate (Core Devices LLC) | `pebble-desktop-signing` keychain on the signer | `APPLE_SIGNING_IDENTITY` |
| App Store Connect API key (`.p8`) for notarization | signer `~/.private_keys/` | `APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH` |
| eng-dash publish secret | Bitwarden; pebble-cluster `flux/apps/eng-dash/desktop-release.sops.yaml`; signer | `DESKTOP_RELEASE_SECRET` |

Details and certificate renewal: `desktop-signer/README.md` in the unicorn repo.

**If the updater private key is lost, installed apps can't accept updates
signed with a new one**; users would have to reinstall by hand.

### How the release setup was done (2026-11-18), and how to redo it

One-time setup, so it can be rebuilt:

1. **eng-dash**: tables and the `desktop_deploy` function come from
   `migrations/ota/012_desktop_releases.sql` in the eng-dash repo, applied to
   eng-dash's Supabase project `hbxeqigofhlhaxtqpkly`. eng-dash itself runs on
   pebble-cluster.
2. **Updater key**: made once on Eric's laptop with
   `npx tauri signer generate -w ~/.pebble-desktop-signing/updater.key`, with a
   password saved to `~/.pebble-desktop-signing/updater-key-password`. The
   public half went into `plugins.updater.pubkey`.
3. **Publish secret**: a random value in
   `~/.pebble-desktop-signing/desktop-release-secret`, also stored as the
   `eng-dash-desktop-release` Secret in pebble-cluster
   (`flux/apps/eng-dash/desktop-release.sops.yaml`, sops-encrypted).
4. **Developer ID certificate**: only the Apple Account Holder can create it.
   `setup.sh` made the private key and signing request on the signer. The
   certificate was created at developer.apple.com (Certificates, +, Developer
   ID Application) and has id `HAVSA63YAB`, expiring 2027-12-18.
5. **Signer**: `./desktop-signer/setup.sh` from `~/dev/unicorn` set up
   unicorn-mac-2. It installs the build tools, copies the laptop files over,
   and imports the certificate into its own keychain.

**Back up to Bitwarden.** Make one Secure Note, "Pebble desktop: updater
signing key":

- Note body: the contents of `updater.key`, pasted exactly.
- Hidden field `password`: the contents of `updater-key-password`.
- Hidden field `desktop-release-secret`: the contents of
  `desktop-release-secret`.

To copy each one without printing it, run one at a time, pasting each into
Bitwarden before running the next:

```sh
pbcopy < ~/.pebble-desktop-signing/updater.key
pbcopy < ~/.pebble-desktop-signing/updater-key-password
pbcopy < ~/.pebble-desktop-signing/desktop-release-secret
pbcopy < /dev/null   # clear the clipboard afterwards
```

No backup is needed for the Developer ID private key (it lives only on the
signer; if it's lost, make a new certificate), the App Store Connect key
(already in the iOS signing setup) or the signer's keychain password
(random; `setup.sh` handles it).

**Restore (new laptop or new signer).** Recreate the three files in
`~/.pebble-desktop-signing/` from Bitwarden, exactly as pasted with no
trailing newline. Then:

```sh
chmod 600 ~/.pebble-desktop-signing/*
cd ~/dev/unicorn && ./desktop-signer/setup.sh
```

**Replace the publish secret** (if it's leaked or lost):

1. Generate a new value with
   `openssl rand -hex 32 > ~/.pebble-desktop-signing/desktop-release-secret`.
2. Rewrite `desktop-release.sops.yaml` in pebble-cluster with the new value,
   then encrypt it with `sops -e -i` and push it. Flux redeploys eng-dash.
3. Re-run `setup.sh` so the signer gets the new value.
4. Update Bitwarden.

**Renew the Developer ID certificate**: see `desktop-signer/README.md` in
the unicorn repo.

**Replace the updater key**: only possible while the old key still exists.
Ship one release signed with the old key that carries the new
`plugins.updater.pubkey`, and wait for users to take it before signing with
the new key.
