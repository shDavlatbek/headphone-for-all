package io.github.shdavlatbek.hfa.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureLoopTest {
    /** Replays [reads]; each push answers the next of [codes] (then OK) and records its frames. */
    private class FakePipe(
        override val channels: Int,
        reads: List<Int>,
        codes: List<Int> = emptyList(),
    ) : PcmPipe {
        private val reads = ArrayDeque(reads)
        private val codes = ArrayDeque(codes)
        val pushed = mutableListOf<Int>()
        val exhausted: Boolean get() = reads.isEmpty()

        override fun read(): Int = reads.removeFirstOrNull() ?: 0

        override fun push(frames: Int): Int {
            pushed += frames
            return codes.removeFirstOrNull() ?: HfaCode.OK
        }
    }

    private fun FakePipe.runUntilExhausted(policy: PushPolicy = PushPolicy()): LoopExit {
        var extraRounds = 0
        return CaptureLoop(this, policy).run { !exhausted || extraRounds++ < 1 }
    }

    @Test
    fun pushesWholeFramesOfEveryRead() {
        val pipe = FakePipe(channels = 2, reads = listOf(960, 961, 0, 2))
        assertEquals(LoopExit.Stopped, pipe.runUntilExhausted())
        assertEquals(listOf(480, 480, 1), pipe.pushed)
    }

    @Test
    fun readErrorsEndTheLoop() {
        // AudioRecord.ERROR_DEAD_OBJECT = -6.
        val pipe = FakePipe(channels = 2, reads = listOf(960, -6, 960))
        assertEquals(LoopExit.ReadFailed(-6), pipe.runUntilExhausted())
        assertEquals(listOf(480), pipe.pushed)
    }

    @Test
    fun unknownFeedIsDroppedSilently() {
        val pipe = FakePipe(channels = 2, reads = List(199) { 960 }, codes = List(199) { HfaCode.UNKNOWN_FEED })
        assertEquals(LoopExit.Stopped, pipe.runUntilExhausted())
        assertEquals(199, pipe.pushed.size)
    }

    @Test
    fun captureEndsWhenNoSenderReadsTheFeedForTwoSeconds() {
        // Regression: after `sender_stop` every push answers UNKNOWN_FEED; the capture (and its
        // MediaProjection) must end instead of running forever.
        val pipe = FakePipe(channels = 2, reads = List(300) { 960 }, codes = List(300) { HfaCode.UNKNOWN_FEED })
        assertEquals(LoopExit.NoSender, pipe.runUntilExhausted())
        assertEquals(PushPolicy.DEFAULT_MAX_UNKNOWN_FEED, pipe.pushed.size)
    }

    @Test
    fun acceptedPushResetsTheUnknownFeedCount() {
        val policy = PushPolicy(maxUnknownFeed = 3)
        assertNull(policy.onResult(HfaCode.UNKNOWN_FEED))
        assertNull(policy.onResult(HfaCode.UNKNOWN_FEED))
        assertNull(policy.onResult(HfaCode.OK))
        assertNull(policy.onResult(HfaCode.UNKNOWN_FEED))
        assertNull(policy.onResult(HfaCode.ENGINE)) // a transient error also breaks the run
        assertNull(policy.onResult(HfaCode.UNKNOWN_FEED))
        assertNull(policy.onResult(HfaCode.UNKNOWN_FEED))
        assertEquals(LoopExit.NoSender, policy.onResult(HfaCode.UNKNOWN_FEED))
    }

    @Test
    fun formatRejectionFailsAtOnce() {
        val pipe = FakePipe(
            channels = 2,
            reads = listOf(960, 960, 960),
            codes = listOf(HfaCode.OK, HfaCode.INVALID_ARGUMENT),
        )
        val exit = pipe.runUntilExhausted()
        assertTrue(exit is LoopExit.PushFailed)
        assertEquals(2, pipe.pushed.size)
    }

    @Test
    fun stopsBeforePushingAReadThatEndedWithTheStop() {
        var running = true
        val pipe = object : PcmPipe {
            override val channels = 2
            var pushes = 0

            override fun read(): Int {
                running = false // AudioRecord.stop() unblocked this read
                return 960
            }

            override fun push(frames: Int): Int {
                pushes++
                return HfaCode.OK
            }
        }
        assertEquals(LoopExit.Stopped, CaptureLoop(pipe).run { running })
        assertEquals(0, pipe.pushes)
    }

    @Test
    fun policyToleratesTransientErrors() {
        val policy = PushPolicy(maxConsecutiveErrors = 3)
        assertNull(policy.onResult(HfaCode.ENGINE))
        assertNull(policy.onResult(HfaCode.INTERNAL))
        assertNull(policy.onResult(HfaCode.OK)) // resets
        assertNull(policy.onResult(HfaCode.ENGINE))
        assertNull(policy.onResult(HfaCode.ENGINE))
        val exit = policy.onResult(HfaCode.ENGINE)
        assertTrue("$exit", exit is LoopExit.PushFailed && exit.reason.contains("3 times"))
    }

    @Test
    fun unknownFeedResetsTheErrorCount() {
        val policy = PushPolicy(maxConsecutiveErrors = 2)
        assertNull(policy.onResult(HfaCode.ENGINE))
        assertNull(policy.onResult(HfaCode.UNKNOWN_FEED))
        assertNull(policy.onResult(HfaCode.ENGINE))
    }
}
