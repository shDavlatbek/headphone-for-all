package io.github.shdavlatbek.hfa.capture

/**
 * An event for Dart on `EventChannel('hfa/platform/events')` (docs/CONTRACTS.md §8.3).
 *
 * @property type `captureStopped` or `captureError` on Android.
 * @property message optional human-readable detail.
 */
data class PlatformEvent(val type: String, val message: String? = null) {
    /** The `{type, message?}` map sent over the channel (`message` only when present). */
    fun toMap(): Map<String, String> =
        if (message == null) mapOf("type" to type) else mapOf("type" to type, "message" to message)

    /** Whether this event reports the end of a capture ([CAPTURE_STOPPED] or [CAPTURE_ERROR]). */
    val isCaptureEnd: Boolean
        get() = type == CAPTURE_STOPPED || type == CAPTURE_ERROR

    companion object {
        /** Event type: capture ended without Dart asking (notification action, system, screen lock). */
        const val CAPTURE_STOPPED: String = "captureStopped"

        /** Event type: capture failed. */
        const val CAPTURE_ERROR: String = "captureError"

        /** A [CAPTURE_STOPPED] event. */
        fun captureStopped(message: String?): PlatformEvent = PlatformEvent(CAPTURE_STOPPED, message)

        /** A [CAPTURE_ERROR] event. */
        fun captureError(message: String): PlatformEvent = PlatformEvent(CAPTURE_ERROR, message)
    }
}

/** The `captureSupport` result: `{supported, reason}`. */
object CaptureSupport {
    /** Android 10 (API 29) introduced `AudioPlaybackCaptureConfiguration`. */
    const val MIN_SDK: Int = 29

    /** The result map for a device running API level [sdkInt]. */
    fun forSdk(sdkInt: Int): Map<String, Any> =
        if (sdkInt >= MIN_SDK) {
            mapOf(
                "supported" to true,
                "reason" to "Captures media and game audio of apps that allow playback capture (Android 10+).",
            )
        } else {
            mapOf("supported" to false, "reason" to "System audio capture needs Android 10 (API 29) or newer.")
        }
}
