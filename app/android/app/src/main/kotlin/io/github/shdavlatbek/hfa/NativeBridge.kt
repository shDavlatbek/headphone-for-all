package io.github.shdavlatbek.hfa

/**
 * JNI entry points of the Rust core (`core/hfa-ffi/src/android.rs`, docs/CONTRACTS.md §8.1).
 *
 * `libhfa_ffi.so` is the same library flutter_rust_bridge loads through Dart FFI; loading it
 * again here only registers it with the JVM so these `external` functions resolve.
 *
 * Both functions return an `HFA_*` code ([io.github.shdavlatbek.hfa.capture.HfaCode]) and never
 * throw: `0` ok (also while the sender is still connecting), `-1` invalid argument (format
 * different from the feed registered by `sender_start`, array shorter than
 * `frames * channels`), `-4` unknown feed (no sender reads it: drop the audio), `-3` JNI
 * failure, `-5` caught panic. The array is copied during the call, so the caller can reuse it.
 */
object NativeBridge {
    /** `null` once `libhfa_ffi.so` is loaded, else why loading failed (then no push may be made). */
    val loadError: String? =
        try {
            System.loadLibrary("hfa_ffi")
            null
        } catch (e: UnsatisfiedLinkError) {
            e.message ?: "UnsatisfiedLinkError"
        }

    /** Pushes [frames] frames of interleaved `f32` PCM in [-1, 1] from the start of [data]. */
    @JvmStatic
    external fun pushPcm(feedId: Int, data: FloatArray, frames: Int, channels: Int, sampleRate: Int): Int

    /** Pushes [frames] frames of interleaved signed 16-bit PCM from the start of [data]. */
    @JvmStatic
    external fun pushPcm16(feedId: Int, data: ShortArray, frames: Int, channels: Int, sampleRate: Int): Int
}
