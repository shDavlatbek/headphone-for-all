package io.github.shdavlatbek.hfa.capture

/** Sample encodings the capture service records in, best first. */
enum class SampleEncoding(
    /** Size of one sample in bytes. */
    val bytesPerSample: Int,
) {
    /** 32-bit float in [-1, 1] (`AudioFormat.ENCODING_PCM_FLOAT`), pushed with `NativeBridge.pushPcm`. */
    FLOAT32(4),

    /** Signed 16-bit (`AudioFormat.ENCODING_PCM_16BIT`), pushed with `NativeBridge.pushPcm16`. */
    PCM16(2),
}

/**
 * Pure PCM arithmetic for the capture service: chunk and buffer sizes, frame counts.
 *
 * Kept free of Android classes so it runs in JVM unit tests.
 */
object PcmFormat {
    /** Duration of one read/push chunk in milliseconds (the Opus frame is 10 or 20 ms). */
    const val CHUNK_MS: Int = 10

    /** The `AudioRecord` buffer holds at least this many chunks. */
    const val MIN_BUFFERED_CHUNKS: Int = 4

    /**
     * Frames in one [chunkMs] chunk at [sampleRate] Hz (at least 1).
     *
     * @throws IllegalArgumentException for non-positive arguments.
     */
    fun chunkFrames(sampleRate: Int, chunkMs: Int = CHUNK_MS): Int {
        require(sampleRate > 0) { "sampleRate must be positive: $sampleRate" }
        require(chunkMs > 0) { "chunkMs must be positive: $chunkMs" }
        return maxOf(1, (sampleRate.toLong() * chunkMs / 1000).toInt())
    }

    /** Size of one interleaved frame in bytes. */
    fun frameBytes(channels: Int, encoding: SampleEncoding): Int {
        require(channels > 0) { "channels must be positive: $channels" }
        return channels * encoding.bytesPerSample
    }

    /**
     * `AudioRecord` buffer size in bytes: at least twice the device minimum [minBufferBytes]
     * (as returned by `AudioRecord.getMinBufferSize`, which must be positive) and at least
     * [MIN_BUFFERED_CHUNKS] chunks of [chunkFrames] frames, rounded up to whole frames.
     */
    fun recordBufferBytes(minBufferBytes: Int, chunkFrames: Int, channels: Int, encoding: SampleEncoding): Int {
        require(minBufferBytes > 0) { "minBufferBytes must be positive: $minBufferBytes" }
        require(chunkFrames > 0) { "chunkFrames must be positive: $chunkFrames" }
        val frame = frameBytes(channels, encoding)
        val wanted = maxOf(2L * minBufferBytes, MIN_BUFFERED_CHUNKS.toLong() * chunkFrames * frame)
        val rounded = (wanted + frame - 1) / frame * frame
        require(rounded <= Int.MAX_VALUE) { "buffer too large: $rounded bytes" }
        return rounded.toInt()
    }

    /** Whole frames in [samples] interleaved samples (a trailing partial frame is not counted). */
    fun wholeFrames(samples: Int, channels: Int): Int {
        require(channels > 0) { "channels must be positive: $channels" }
        return if (samples <= 0) 0 else samples / channels
    }
}
