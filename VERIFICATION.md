# Extraction parity verification

Verified locally on 2026-09-17 on this Apple Silicon Mac (macOS 26.4.1).

- Launched the original Corten `ExtractKey.app` from its shipped archive.
- Used its **Save to File** action to capture the reference key.
- Its exported file contains exactly 1,300 bytes.
- Ran the separate original Corten CLI; its key matches the GUI export exactly
  (the CLI additionally writes one terminating LF).
- Ran the native arm64 Rust CLI three times in debug mode, then five times in
  release mode: every stdout byte matched the original GUI export. No decoding,
  whitespace trimming, JSON reordering, or normalization was used in that comparison.
- Ran a fresh Corten CLI after the release build and compared all three outputs
  again: exact match.
- Five Rust unit tests pass, covering upstream encryption vectors, concurrent
  encryption, NVRAM decoding, existing/malformed encrypted properties, and exact
  JSON serialization/escaping. Clippy passes with warnings denied; rustfmt passes.
- Confirmed that the tested Mac's identifiers/key are absent from project source.

The native Rust process uses Unicorn for the unchanged upstream encryption
routine; it does not execute the reference Corten app/CLI, Swift, Go, or Rosetta.
Raw reference keys are local, private, ignored test artifacts. The repeatable
comparison command is in README.md. Intel hardware has not been tested here.

At this checkpoint, the Rust-only extraction milestone was complete.
The desktop work that followed is recorded below.

## Tauri/web stage — 2026-09-17

The Rust-only milestone above is complete. The subsequent desktop stage adds:

- A locally built and launched Tauri Mac app with the existing Index React UI.
- Native-only settings detection verified in the Mac app; regular-browser
  Settings has no iMessage section. Top branding now says Pebble.
- Account validation, per-account On/Off preferences, owner-scoped Firebase save,
  restart deduplication, and removal retries covered by 9 native desktop tests.
  Together with the extractor there are 14 passing Rust tests.
- 68 web tests, including duplicate-request/account separation and QR decoding;
  TypeScript, changed-file ESLint, and the Next.js production build pass.
- The actual private 1,300-character Corten reference also round-trips through
  the 512px QR encoder/decoder exactly. This check prints no key material.
- Four Firestore emulator tests pass against both the isolated collection rules
  and the full production rules with the additive match. The owner-only match
  was deployed to `coreapp-ce061` and read back; all existing rules were preserved.
- Browser sign-in and its timeout state were verified. After the user signed in,
  the packaged Mac app successfully saved the key to their Pebble account;
  Settings showed the computer name and saved date. The QR/key reveal and
  native clipboard copy succeeded. One reveal attempt returned a recoverable
  extraction error before a subsequent attempt succeeded.
  Mobile plugin automatic key selection and public app distribution remain separate.
- Packaged both native arm64 release (16 MB) and debug Mac apps. The release
  bundle passes `codesign --verify --deep --strict` with an ad-hoc signature.
  It is not notarized, and the accompanying website changes have not been deployed.
- Added the hardened-runtime `allow-jit` entitlement required by Unicorn's
  Apple Silicon emulator. Without it, the signed app exited when extraction
  tried to allocate its translator buffer. Both bundles were re-signed and
  verified; the debug app stayed open through live extraction, save, and copy.
- Simplified the app sign-in copy. The shared backend confirmation now renders
  Pebble Mac-specific wording for the desktop callback, while keeping CLI
  sign-ins identified as CLI. Type checking, HTML escaping checks, and a local
  browser preview pass. The backend change was deployed to production on
  2026-09-17 as `dpl_ET4XggQ3qSem73S4RjH7vXCRsZ6k`, based on backend main
  `11f9d77` plus the two authorization-page files. The production domain points
  to the READY deployment; fresh-flow redirects, invalid-flow rejection, and
  cross-origin POST rejection passed live checks. The separate Index website
  changes remain local.
