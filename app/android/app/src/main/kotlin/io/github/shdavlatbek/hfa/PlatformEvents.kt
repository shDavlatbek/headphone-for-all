package io.github.shdavlatbek.hfa

import android.os.Handler
import android.os.Looper
import android.util.Log
import io.flutter.plugin.common.EventChannel
import io.github.shdavlatbek.hfa.capture.OwnedSlot
import io.github.shdavlatbek.hfa.capture.PlatformEvent

/**
 * The Dart listener of `EventChannel('hfa/platform/events')`.
 *
 * Process-wide, because the capture service reports while the activity (and its Flutter
 * engine) may be gone: events emitted while Dart does not listen are dropped, which is fine
 * since a new UI asks for the state again. Each engine registers its own [handlerFor]; the
 * sink belongs to the engine that listened last, so a late cancel or cleanup of an older
 * engine cannot silence a newer one.
 */
object PlatformEvents {
    private const val TAG = "hfa.events"

    private val mainHandler = Handler(Looper.getMainLooper())

    /** The sink of the listening engine, keyed by its messenger. Only touched on the main thread. */
    private val sink = OwnedSlot<EventChannel.EventSink>()

    /** The stream handler for the engine identified by [owner] (its `BinaryMessenger`). */
    fun handlerFor(owner: Any): EventChannel.StreamHandler =
        object : EventChannel.StreamHandler {
            override fun onListen(arguments: Any?, events: EventChannel.EventSink?) {
                if (events == null) sink.clear(owner) else sink.set(owner, events)
            }

            override fun onCancel(arguments: Any?) {
                sink.clear(owner)
            }
        }

    /** Sends [event] to Dart; callable from any thread. */
    fun emit(event: PlatformEvent) {
        mainHandler.post {
            val current = sink.value
            if (current == null) {
                Log.i(TAG, "no listener for ${event.type}: ${event.message}")
            } else {
                current.success(event.toMap())
            }
        }
    }

    /** Forgets the sink of the engine [owner] that is being destroyed, if it still holds it (main thread). */
    fun detach(owner: Any) {
        sink.clear(owner)
    }
}
