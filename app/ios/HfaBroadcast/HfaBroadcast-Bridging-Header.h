//
// HfaBroadcast-Bridging-Header.h - exposes the Rust sender's C ABI to SampleHandler.swift.
//
// hfa_ext.h lives in core/hfa-ffi/include (HEADER_SEARCH_PATHS of the HfaBroadcast target);
// the implementation is the static library libhfa_ext.a built by scripts/build_rust_ext.sh.
//
#import "hfa_ext.h"
