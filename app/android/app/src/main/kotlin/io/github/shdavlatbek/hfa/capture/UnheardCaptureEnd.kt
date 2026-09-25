package io.github.shdavlatbek.hfa.capture

/**
 * Remembers the last end of a capture (`captureStopped` / `captureError`) that no Dart listener
 * received, so a Flutter UI created later can still learn about it through `captureStatus`.
 *
 * The capture service outlives the activity: when the capture ends while no Flutter engine
 * listens, its event is dropped, and the Rust sender (which lives as long as the process) keeps
 * streaming an empty feed to the hub. The next UI reads [take] and stops that sender.
 *
 * Not thread-safe: [io.github.shdavlatbek.hfa.PlatformEvents] uses it on the main thread only.
 */
class UnheardCaptureEnd {
    private var event: PlatformEvent? = null

    /** [event] was emitted; [heard] is whether a Dart listener received it. */
    fun onEmitted(event: PlatformEvent, heard: Boolean) {
        if (!event.isCaptureEnd) return
        this.event = if (heard) null else event
    }

    /** A new capture starts: an older end no longer matters. */
    fun clear() {
        event = null
    }

    /** The unheard end, if any, and forgets it (a later UI must not act on it twice). */
    fun take(): PlatformEvent? = event.also { event = null }
}
