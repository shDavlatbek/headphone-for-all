package io.github.shdavlatbek.hfa.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class PcmFormatTest {
    @Test
    fun chunkFramesIsTenMilliseconds() {
        assertEquals(480, PcmFormat.chunkFrames(48_000))
        assertEquals(441, PcmFormat.chunkFrames(44_100))
        assertEquals(960, PcmFormat.chunkFrames(48_000, chunkMs = 20))
        assertEquals(1, PcmFormat.chunkFrames(50, chunkMs = 1))
        assertThrows(IllegalArgumentException::class.java) { PcmFormat.chunkFrames(0) }
    }

    @Test
    fun frameBytesDependOnEncoding() {
        assertEquals(8, PcmFormat.frameBytes(2, SampleEncoding.FLOAT32))
        assertEquals(4, PcmFormat.frameBytes(2, SampleEncoding.PCM16))
        assertEquals(2, PcmFormat.frameBytes(1, SampleEncoding.PCM16))
    }

    @Test
    fun recordBufferIsAtLeastTwiceTheMinimum() {
        // Large device minimum: 2x wins.
        assertEquals(30_720, PcmFormat.recordBufferBytes(15_360, 480, 2, SampleEncoding.FLOAT32))
    }

    @Test
    fun recordBufferHoldsAtLeastFourChunks() {
        // Tiny device minimum: 4 chunks of 480 stereo float frames win.
        assertEquals(4 * 480 * 8, PcmFormat.recordBufferBytes(100, 480, 2, SampleEncoding.FLOAT32))
    }

    @Test
    fun recordBufferIsWholeFrames() {
        for (min in listOf(1, 3, 1001, 7_777, 12_345)) {
            for (encoding in SampleEncoding.entries) {
                val bytes = PcmFormat.recordBufferBytes(min, 441, 2, encoding)
                val frame = PcmFormat.frameBytes(2, encoding)
                assertEquals("min=$min $encoding", 0, bytes % frame)
                assertTrue(bytes >= 2 * min)
                assertTrue(bytes >= PcmFormat.MIN_BUFFERED_CHUNKS * 441 * frame)
            }
        }
    }

    @Test
    fun recordBufferRejectsDeviceErrors() {
        // AudioRecord.getMinBufferSize returns ERROR (-1) / ERROR_BAD_VALUE (-2).
        assertThrows(IllegalArgumentException::class.java) {
            PcmFormat.recordBufferBytes(-2, 480, 2, SampleEncoding.PCM16)
        }
    }

    @Test
    fun wholeFramesDropsPartialFrames() {
        assertEquals(480, PcmFormat.wholeFrames(960, 2))
        assertEquals(480, PcmFormat.wholeFrames(961, 2))
        assertEquals(0, PcmFormat.wholeFrames(1, 2))
        assertEquals(0, PcmFormat.wholeFrames(-3, 2))
        assertEquals(7, PcmFormat.wholeFrames(7, 1))
    }
}
