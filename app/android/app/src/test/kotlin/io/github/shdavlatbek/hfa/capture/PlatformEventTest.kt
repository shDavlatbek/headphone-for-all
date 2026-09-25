package io.github.shdavlatbek.hfa.capture

import org.junit.Assert.assertEquals
import org.junit.Test

class PlatformEventTest {
    @Test
    fun mapsMatchTheContract() {
        assertEquals(mapOf("type" to "captureStopped"), PlatformEvent.captureStopped(null).toMap())
        assertEquals(
            mapOf("type" to "captureError", "message" to "boom"),
            PlatformEvent.captureError("boom").toMap(),
        )
    }

    @Test
    fun captureSupportNeedsApi29() {
        assertEquals(true, CaptureSupport.forSdk(29)["supported"])
        assertEquals(true, CaptureSupport.forSdk(36)["supported"])
        assertEquals(false, CaptureSupport.forSdk(28)["supported"])
        for (sdk in listOf(28, 29)) {
            val reason = CaptureSupport.forSdk(sdk)["reason"]
            assertEquals(true, reason is String && reason.isNotBlank())
        }
    }
}
