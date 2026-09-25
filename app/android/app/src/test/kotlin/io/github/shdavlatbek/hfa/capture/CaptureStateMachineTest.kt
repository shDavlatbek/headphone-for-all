package io.github.shdavlatbek.hfa.capture

import io.github.shdavlatbek.hfa.capture.CaptureEffect.Emit
import io.github.shdavlatbek.hfa.capture.CaptureEffect.Reply
import io.github.shdavlatbek.hfa.capture.CaptureEffect.RequestConsent
import io.github.shdavlatbek.hfa.capture.CaptureEffect.RequestPermissions
import io.github.shdavlatbek.hfa.capture.CaptureEffect.StartService
import io.github.shdavlatbek.hfa.capture.CaptureEffect.StopService
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureStateMachineTest {
    private val request = CaptureRequest(1, 48_000, 2)
    private val machine = CaptureStateMachine()

    /** Drives a start through both dialogs; returns the service session. */
    private fun startToService(): Int {
        assertEquals(listOf(RequestPermissions), machine.start(request))
        assertEquals(listOf(RequestConsent), machine.onPermissions(true))
        val effects = machine.onConsent(true)
        val start = effects.single() as StartService
        assertEquals(request, start.request)
        assertEquals(CapturePhase.STARTING, machine.phase)
        return start.session
    }

    private fun running(): Int {
        val session = startToService()
        assertEquals(listOf(Reply(true)), machine.onServiceStarted(session))
        assertEquals(CapturePhase.RUNNING, machine.phase)
        return session
    }

    @Test
    fun happyPathRepliesTrueOnceTheServiceRecords() {
        running()
        assertFalse(machine.isStartPending)
    }

    @Test
    fun deniedPermissionRepliesFalse() {
        machine.start(request)
        assertEquals(listOf(Reply(false)), machine.onPermissions(false))
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun refusedConsentRepliesFalse() {
        machine.start(request)
        machine.onPermissions(true)
        assertEquals(listOf(Reply(false)), machine.onConsent(false))
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun secondStartWhilePendingIsRefused() {
        machine.start(request)
        assertNull(machine.start(request))
        assertEquals(CapturePhase.AWAITING_PERMISSION, machine.phase)
        machine.onPermissions(true)
        assertNull(machine.start(request))
        machine.onConsent(true)
        assertNull(machine.start(request))
    }

    @Test
    fun serviceFailureWhileStartingRepliesFalseAndReportsTheError() {
        val session = startToService()
        assertEquals(
            listOf(Reply(false), Emit(PlatformEvent.captureError("no audio"))),
            machine.onServiceFailed(session, "no audio"),
        )
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun serviceFailureWhileRunningEmitsCaptureError() {
        val session = running()
        assertEquals(listOf(Emit(PlatformEvent.captureError("dead"))), machine.onServiceFailed(session, "dead"))
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun systemStopWhileRunningEmitsCaptureStopped() {
        val session = running()
        assertEquals(
            listOf(Emit(PlatformEvent.captureStopped("screen locked"))),
            machine.onServiceStopped(session, "screen locked"),
        )
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun dartStopIsNotReportedBack() {
        val session = running()
        assertEquals(listOf(StopService), machine.stop())
        // The service's late report of that session is ignored.
        assertTrue(machine.onServiceStopped(session, "stopped").isEmpty())
        assertTrue(machine.onServiceFailed(session, "late").isEmpty())
        assertTrue(machine.stop().isEmpty())
    }

    @Test
    fun stopWhileWaitingForADialogCancelsTheStart() {
        machine.start(request)
        assertEquals(listOf(Reply(false)), machine.stop())
        // The dialog's answer arrives afterwards and is ignored.
        assertTrue(machine.onPermissions(true).isEmpty())

        machine.start(request)
        machine.onPermissions(true)
        assertEquals(listOf(Reply(false)), machine.stop())
        assertTrue(machine.onConsent(true).isEmpty())
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun stopWhileStartingRepliesFalseAndStopsTheService() {
        val session = startToService()
        assertEquals(listOf(Reply(false), StopService), machine.stop())
        assertTrue(machine.onServiceStarted(session).isEmpty())
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun startTimeoutGivesUp() {
        val session = startToService()
        val effects = machine.onStartTimeout(session)
        assertEquals(Reply(false), effects[0])
        assertEquals(StopService, effects[1])
        assertEquals(PlatformEvent.CAPTURE_ERROR, (effects[2] as Emit).event.type)
        assertTrue(machine.onServiceStarted(session).isEmpty())
    }

    @Test
    fun startTimeoutAfterTheStartIsIgnored() {
        val session = running()
        assertTrue(machine.onStartTimeout(session).isEmpty())
        assertEquals(CapturePhase.RUNNING, machine.phase)
    }

    @Test
    fun restartStopsTheRunningCaptureAndIgnoresItsReports() {
        val old = running()
        assertEquals(listOf(StopService, RequestPermissions), machine.start(request))
        assertTrue(machine.onServiceStopped(old, "replaced").isEmpty())
        machine.onPermissions(true)
        val next = (machine.onConsent(true).single() as StartService).session
        assertTrue(next != old)
        assertTrue(machine.onServiceStarted(old).isEmpty())
        assertEquals(listOf(Reply(true)), machine.onServiceStarted(next))
    }

    @Test
    fun losingTheHostAnswersAPendingDialog() {
        machine.start(request)
        assertEquals(listOf(Reply(false)), machine.onHostLost())
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun losingTheHostKeepsARunningCapture() {
        running()
        assertTrue(machine.onHostLost().isEmpty())
        assertEquals(CapturePhase.RUNNING, machine.phase)
    }

    @Test
    fun onlyTheRunningSessionIsCurrent() {
        val session = startToService()
        assertFalse(machine.isCurrentCapture(session)) // not recording yet
        machine.onServiceStarted(session)
        assertTrue(machine.isCurrentCapture(session))
        assertFalse(machine.isCurrentCapture(session + 1))
    }

    @Test
    fun aServiceThatStartsAfterStopWhileStartingIsNotCurrent() {
        // stopSystemCapture during STARTING: if the queued stop command never reaches the
        // service, its late start must still learn that it is stale and tear itself down.
        val session = startToService()
        machine.stop()
        machine.onServiceStarted(session)
        assertFalse(machine.isCurrentCapture(session))
        assertEquals(CapturePhase.IDLE, machine.phase)
    }

    @Test
    fun aServiceThatStartsAfterTheTimeoutIsNotCurrent() {
        val session = startToService()
        machine.onStartTimeout(session)
        machine.onServiceStarted(session)
        assertFalse(machine.isCurrentCapture(session))
    }

    @Test
    fun aReplacedCaptureIsNotCurrent() {
        val old = running()
        machine.start(request)
        assertFalse(machine.isCurrentCapture(old))
    }

    @Test
    fun sessionsNeverRepeat() {
        val seen = mutableSetOf<Int>()
        repeat(5) {
            val session = running()
            assertTrue(seen.add(session))
            machine.stop()
        }
    }
}
