package io.github.shdavlatbek.hfa.capture

/**
 * Arguments of the `startSystemCapture` platform call (docs/CONTRACTS.md §8.3).
 *
 * The Rust sender registered the external feed [feedId] with exactly this format
 * (`CaptureSourceDto::External { feed_id, sample_rate, channels }`), and the JNI push rejects
 * any other format, so the capture service records in this format.
 *
 * @property feedId opaque 32-bit feed id shared with Dart and Rust (Rust reads the bits as `u32`).
 * @property sampleRate sample rate in Hz ([MIN_SAMPLE_RATE]..[MAX_SAMPLE_RATE]).
 * @property channels 1 (mono) or 2 (stereo): the channel counts `AudioRecord` captures.
 */
data class CaptureRequest(val feedId: Int, val sampleRate: Int, val channels: Int) {
    init {
        require(sampleRate in MIN_SAMPLE_RATE..MAX_SAMPLE_RATE) { "sampleRate out of range: $sampleRate" }
        require(channels in 1..2) { "channels must be 1 or 2: $channels" }
    }

    companion object {
        /** Lowest accepted sample rate (the Rust feed accepts 8000..=192000 Hz). */
        const val MIN_SAMPLE_RATE: Int = 8_000

        /** Highest accepted sample rate. */
        const val MAX_SAMPLE_RATE: Int = 192_000

        private const val MAX_FEED_ID: Long = 0xFFFF_FFFFL

        /**
         * Parses the method-call arguments `{feedId, sampleRate, channels}`.
         *
         * Dart integers arrive as [Int] or [Long] depending on their size. `feedId` may use the
         * full unsigned 32-bit range and is narrowed to an [Int] with the same bits.
         *
         * @throws IllegalArgumentException when a field is missing, not an integer or out of range.
         */
        fun fromArguments(arguments: Any?): CaptureRequest {
            val map = arguments as? Map<*, *>
                ?: throw IllegalArgumentException("expected a map {feedId, sampleRate, channels}")
            val feedId = integer(map, "feedId")
            require(feedId in 0..MAX_FEED_ID) { "feedId out of range: $feedId" }
            val sampleRate = integer(map, "sampleRate")
            require(sampleRate in MIN_SAMPLE_RATE..MAX_SAMPLE_RATE) { "sampleRate out of range: $sampleRate" }
            val channels = integer(map, "channels")
            require(channels in 1L..2L) { "channels must be 1 or 2: $channels" }
            return CaptureRequest(feedId.toInt(), sampleRate.toInt(), channels.toInt())
        }

        private fun integer(map: Map<*, *>, key: String): Long =
            when (val value = map[key]) {
                is Int -> value.toLong()
                is Long -> value
                null -> throw IllegalArgumentException("missing $key")
                else -> throw IllegalArgumentException("$key must be an integer, got ${value::class.simpleName}")
            }
    }
}
