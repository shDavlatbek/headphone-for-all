package io.github.shdavlatbek.hfa.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class CaptureRequestTest {
    @Test
    fun parsesTheDartArguments() {
        val request = CaptureRequest.fromArguments(mapOf("feedId" to 1, "sampleRate" to 48_000, "channels" to 2))
        assertEquals(CaptureRequest(1, 48_000, 2), request)
    }

    @Test
    fun acceptsLongIntegers() {
        // Dart ints that do not fit 32 bits arrive as Long.
        val request = CaptureRequest.fromArguments(mapOf("feedId" to 7L, "sampleRate" to 44_100L, "channels" to 1L))
        assertEquals(CaptureRequest(7, 44_100, 1), request)
    }

    @Test
    fun keepsTheBitsOfUnsigned32BitFeedIds() {
        // Rust reads the jint as u32: 0xE7E70000 must reach it unchanged.
        val request = CaptureRequest.fromArguments(
            mapOf("feedId" to 0xE7E7_0000L, "sampleRate" to 48_000, "channels" to 2),
        )
        assertEquals(0xE7E7_0000L.toInt(), request.feedId)
        assertEquals(0xE7E7_0000L, request.feedId.toLong() and 0xFFFF_FFFFL)
    }

    @Test
    fun rejectsInvalidArguments() {
        val valid = mapOf<String, Any>("feedId" to 1, "sampleRate" to 48_000, "channels" to 2)
        val invalid = listOf(
            null,
            "not a map",
            valid - "feedId",
            valid + ("feedId" to -1),
            valid + ("feedId" to 0x1_0000_0000L),
            valid + ("sampleRate" to 7_999),
            valid + ("sampleRate" to 192_001),
            valid + ("sampleRate" to "48000"),
            valid + ("channels" to 0),
            valid + ("channels" to 3),
            valid + ("channels" to 2.0),
        )
        for (arguments in invalid) {
            assertThrows("accepted $arguments", IllegalArgumentException::class.java) {
                CaptureRequest.fromArguments(arguments)
            }
        }
    }

    @Test
    fun constructorValidatesTheFormat() {
        assertThrows(IllegalArgumentException::class.java) { CaptureRequest(1, 48_000, 8) }
        assertThrows(IllegalArgumentException::class.java) { CaptureRequest(1, 0, 2) }
    }
}
