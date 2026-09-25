package io.github.shdavlatbek.hfa

import android.os.Handler
import android.os.Looper
import android.util.Log
import io.flutter.plugin.common.EventChannel
import io.github.shdavlatbek.hfa.capture.PlatformEvent

/**
 * Stream handler of `EventChannel('hfa/platform/events')`.
 *
 * Process-wide, because the capture service reports while the activity (and its Flutter
 * engine) may be gone: events emitted while Dart does not listen are dropped, which is fine
 * since a new UI asks for the state again.
 */
object PlatformEvents : EventChannel.StreamHandler {
    private const val TAG = "hfa.events"

    private val mainHandler = Handler(Looper.getMainLooper())

    /** Only touched on the main thread. */
    private var sink: EventChannel.EventSink? = null

    override fun onListen(arguments: Any?, events: EventChannel.EventSink?) {
        sink = events
    }

    override fun onCancel(arguments: Any?) {
        sink = null
    }

    /** Sends [event] to Dart; callable from any thread. */
    fun emit(event: PlatformEvent) {
        mainHandler.post {
            val current = sink
            if (current == null) {
                Log.i(TAG, "no listener for ${event.type}: ${event.message}")
            } else {
                current.success(event.toMap())
            }
        }
    }

    /** Forgets the sink of an engine that is being destroyed (main thread). */
    fun detach() {
        sink = null
    }
}
