# Headphone for All — Flutter app

The UI of headphone-for-all for Windows, macOS, Linux, Android and iOS. All audio work happens in
the Rust core (`../core`), reached through [flutter_rust_bridge] 2.13.0:

- `lib/src/rust/` — generated Dart bindings of `core/hfa-ffi/src/api` (never edit by hand;
  regenerate with `flutter_rust_bridge_codegen generate` from this directory).
- `rust_builder/` — the cargokit glue plugin (`rust_lib_headphone_for_all`) that builds
  `core/hfa-ffi` (library `hfa_ffi`) for every platform during `flutter build`.
- `flutter_rust_bridge.yaml` — codegen configuration.

Build commands: [`docs/BUILDING.md`](../docs/BUILDING.md) (the contract for the bindings is in `docs/CONTRACTS.md` §8).

[flutter_rust_bridge]: https://github.com/fzyzcjy/flutter_rust_bridge
