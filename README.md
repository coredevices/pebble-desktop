# Pebble Desktop

The desktop app for [Pebble Index](https://index.repebble.com), for macOS,
Windows and Linux. It's built with [Tauri 2](https://tauri.app) and shows the
Index web app in a native window. On top of that it adds:

- a menu bar / system tray icon and a global shortcut for a quick capture bar;
- reminder pop-ups on your desktop;
- "at this computer" presence, so your phone can hold back watch notifications
  while you're at the computer (can be turned off in Settings). The Pebble
  mobile app doesn't use it yet; this is groundwork too;
- signed automatic updates (can be turned off in Settings);
- on macOS, early groundwork for iMessage support (see below).

## Download

Installers are on the [GitHub releases](../../releases) page: `.dmg` for Apple
silicon and Intel Macs, `-setup.exe` for Windows and `.AppImage` for Linux.
macOS builds are signed and notarized. Windows and Linux installers aren't
code-signed yet, so Windows SmartScreen warns on first run. Every update, on
every platform, is checked against the app's update signing key before it's
installed.

The app loads `https://index.repebble.com`, so it needs a connection for the
first load and for sign-in. No web server is bundled.

## Build from source

Requires Rust, Node.js and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform.
On macOS you also need the Xcode command-line tools and CMake/libclang (for the
Unicorn CPU emulator used by the iMessage extractor).

```sh
npm ci
npm run build        # release build, loads index.repebble.com
npm run dev          # development build, loads http://localhost:3017
```

`npm run dev` and `npm run build:dev` expect the Index web app running on
`localhost:3017`. The development build is called Pebble Dev and installs
beside the release app. It never checks for updates.

Local builds are ad-hoc signed, so they need no signing keys.

## iMessage setup (macOS)

**Very early.** The Pebble mobile app doesn't support iMessage yet, so this
does nothing useful today. It's only the foundation for future support.

Only the Mac app shows this section at the top of Settings. Web browsers,
including browsers on a Mac, don't.

- It's on by default, separately for each signed-in account.
- Sign-in uses the Pebble browser sign-in (Google, GitHub or Apple) and returns
  to the app through a random, short-lived local callback. Your Pebble account
  is checked before anything is extracted or uploaded.
- The activation key is extracted locally in Rust and uploaded with your own
  Firebase sign-in; no service credentials are built in. The key is never
  logged.
- Each Mac stores one record in
  `chat_activation_keys/{uid}/keys/{installationUUID}` with `computerName`,
  `macValidationData`, `formatVersion: 1`, `platform: "macOS"` and `createdAt`.
  Refreshing doesn't add duplicates.
- Turning it off removes this Mac's saved key (retried after restart if it
  fails). It can't revoke a key a phone has already copied.
- "Show setup key" shows the key as a QR code and copyable text, generated
  locally. The app can write to the clipboard but can't read it.
- This is one-time setup. The Mac doesn't relay messages and can be turned off.

The native bridge is limited to the main window and the Index HTTPS origin.
Localhost access exists only in debug builds. Other links open in your browser
without native access.

## Tests

```sh
cargo test --workspace -j 2
cargo clippy --workspace --all-targets -j 2 -- -D warnings
cargo fmt --all --check
firebase emulators:exec --only firestore --project demo-pebble-desktop \
  'node --test firebase/rules.test.mjs'   # needs Java 21+
```

To compare the extractor byte for byte with the original Corten ExtractKey app,
save its output, then:

```sh
cargo build --release -p pebble-mac-key
python3 scripts/verify-reference.py /path/to/corten-gui.txt
```

`target/release/pebble-extract-key` prints only the base64 key with no newline,
matching the original app's saved file. The library entry point is
`pebble_mac_key::extract_key() -> Result<String, pebble_mac_key::Error>`. No
Rosetta, Go, Swift or original app is needed at runtime.

`firebase/firestore.rules` is a test fixture for the emulator, not a complete
production rules file. Set `PEBBLE_RULES_FILE` to test another rules file.

## Releases

Maintainers release with `.github/workflows/release.yml`: push a `vX.Y.Z` tag
matching `version` in `src-tauri/tauri.conf.json`, or run the workflow by hand
for a tester track.

## Credits

See [UPSTREAM.md](UPSTREAM.md) for source provenance and third-party notices,
and [VERIFICATION.md](VERIFICATION.md) for extractor parity.
