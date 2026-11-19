# Extractor provenance

Reference binaries are the Mac `tools/ExtractKey.app.zip` and
`tools/extract-key-cli.zip` in https://github.com/lrhodin/corten-matrix at commit
`7b3af506a73de0b202ec42bd1fb4c032d5bc9da5` (archive date 2026-06-26). They are used only for local parity testing;
they are not linked, executed, or distributed by the Rust implementation.

The Rust hardware reader is a port of the IOKit/DiskArbitration extraction in
`tools/extract-key/main.go` and `tools/extract-key-app/Sources/ExtractKeyApp` from
https://github.com/lrhodin/imessage at commit
`7514002a7ba96e5498c6f45a76d006375e888ccc` (MPL 2.0, see LICENSE).
The released Corten binaries use protocol 1640 and a different JSON order/user
agent than that older source. The Rust serializer matches the released binaries.

`vendor/encrypt.s` is preserved unchanged from that checkout's
`rustpush/open-absinthe/src/asm/encrypt.s`. See `vendor/LICENSE.rustpush` and
`vendor/LICENSE.exceptions` for the upstream notices. `build.rs` assembles it
into a Mach-O image; Rust runs that image inside Unicorn on both architectures.
Unicorn is GPL-2.0 licensed; the dependency and its license must be accounted for
in any distributed desktop application. Cargo.lock pins the dependency graph.

The release was also inspected to preserve these Apple Silicon behaviors:

- Read en0's permanent `IOMACAddress` through IOKit before `getifaddrs`.
- Prefer `IODeviceTree:/chosen`'s `boot-uuid` over DiskArbitration's volume UUID.
- Encode the chosen node's board ID bytes as lowercase hex after `Mac-`.
- Derive missing ROM from the final six bytes of SHA-256 of `unique-chip-id`.

No Matrix bridge is included.

Reference archive SHA-256 checksums:

- ExtractKey.app.zip: `380c87cdf059364a9613dff9127af18deeebebd2ccb7af1c18404b0340252e5b`
- extract-key-cli.zip: `41ff44ee8db56affbcba16b88496a76f9b021b449cb1321485c1e9e40418384e`
