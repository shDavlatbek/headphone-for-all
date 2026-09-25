package io.github.shdavlatbek.hfa.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class UnheardCaptureEndTest {
    private val memo = UnheardCaptureEnd()

    @Test
    fun remembersAnUnheardEndUntilTaken() {
        val stopped = PlatformEvent.captureStopped("screen locked")
        memo.onEmitted(stopped, heard = false)
        assertEquals(stopped, memo.take())
        assertNull(memo.take()) // handed out once
    }

    @Test
    fun aHeardEndReplacesAnOlderUnheardOne() {
        memo.onEmitted(PlatformEvent.captureError("boom"), heard = false)
        memo.onEmitted(PlatformEvent.captureStopped(null), heard = true)
        assertNull(memo.take())
    }

    @Test
    fun keepsTheLatestUnheardEnd() {
        memo.onEmitted(PlatformEvent.captureError("first"), heard = false)
        val last = PlatformEvent.captureStopped("second")
        memo.onEmitted(last, heard = false)
        assertEquals(last, memo.take())
    }

    @Test
    fun otherEventsAreIgnored() {
        val stopped = PlatformEvent.captureStopped(null)
        memo.onEmitted(stopped, heard = false)
        memo.onEmitted(PlatformEvent("broadcastStarted"), heard = true)
        assertEquals(stopped, memo.take())
    }

    @Test
    fun aNewStartForgetsTheEnd() {
        memo.onEmitted(PlatformEvent.captureStopped(null), heard = false)
        memo.clear()
        assertNull(memo.take())
    }
}
