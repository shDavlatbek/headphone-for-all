package io.github.shdavlatbek.hfa

import android.app.Application
import android.util.Log

/**
 * The process entry point: hands the application context to the Rust core before any Flutter
 * engine exists (docs/CONTRACTS.md §8.8).
 *
 * cpal's AAudio backend, which plays the hub's mix on the headphone, reads the `JavaVM` and an
 * `android.content.Context` from the `ndk-context` crate; nothing in a Flutter app sets them,
 * so without [NativeBridge.init] the hub could not open its output.
 */
class HfaApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        val loadError = NativeBridge.loadError
        if (loadError != null) {
            Log.e(TAG, "libhfa_ffi.so not loaded: $loadError")
            return
        }
        val code = NativeBridge.init(applicationContext)
        if (code != 0) Log.e(TAG, "NativeBridge.init failed with $code: the hub cannot play audio")
    }

    private companion object {
        const val TAG = "hfa.app"
    }
}
