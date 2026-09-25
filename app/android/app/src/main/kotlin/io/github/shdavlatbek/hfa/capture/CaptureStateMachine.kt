package io.github.shdavlatbek.hfa.capture

/** Where the system-capture flow is. */
enum class CapturePhase {
    /** Nothing is captured and no start is pending. */
    IDLE,

    /** Waiting for the RECORD_AUDIO / POST_NOTIFICATIONS permission dialog. */
    AWAITING_PERMISSION,

    /** Waiting for the MediaProjection consent dialog. */
    AWAITING_CONSENT,

    /** The capture service was started and has not reported back yet. */
    STARTING,

    /** The capture service records and pushes PCM. */
    RUNNING,
}

/** Something the Android side must do after a transition. */
sealed interface CaptureEffect {
    /** Answer the pending `startSystemCapture` call with [started]. */
    data class Reply(val started: Boolean) : CaptureEffect

    /** Send [event] to Dart. */
    data class Emit(val event: PlatformEvent) : CaptureEffect

    /** Ask for the runtime permissions, then call [CaptureStateMachine.onPermissions]. */
    data object RequestPermissions : CaptureEffect

    /** Show the MediaProjection consent dialog, then call [CaptureStateMachine.onConsent]. */
    data object RequestConsent : CaptureEffect

    /**
     * Start the capture service for [session] with the consent just given, and arm the start
     * timeout (then call [CaptureStateMachine.onStartTimeout] with [session]).
     */
    data class StartService(val session: Int, val request: CaptureRequest) : CaptureEffect

    /** Stop the capture service (whatever session it runs). */
    data object StopService : CaptureEffect
}

/**
 * The system-capture flow of `startSystemCapture` / `stopSystemCapture`, without Android types.
 *
 * Every start gets a new session number, and every stop or restart invalidates the current
 * one, so late reports from a capture service that was already replaced or stopped are
 * ignored. At most one `startSystemCapture` call is pending; a second start while one is
 * pending is refused by [start] (the caller answers `false` itself).
 *
 * Not thread-safe: the Android side calls it on the main thread only.
 */
class CaptureStateMachine {
    /** The current phase. */
    var phase: CapturePhase = CapturePhase.IDLE
        private set

    /** The current session number (increases on every start and stop). */
    var session: Int = 0
        private set

    private var request: CaptureRequest? = null

    /** Whether a start is in progress (a `startSystemCapture` call is waiting for its answer). */
    val isStartPending: Boolean
        get() = phase == CapturePhase.AWAITING_PERMISSION ||
            phase == CapturePhase.AWAITING_CONSENT ||
            phase == CapturePhase.STARTING

    /**
     * A `startSystemCapture` call. Returns `null` when refused because another start is
     * pending, else the effects. A running capture is stopped first (a restart needs new consent).
     */
    fun start(request: CaptureRequest): List<CaptureEffect>? {
        if (isStartPending) return null
        val wasRunning = phase == CapturePhase.RUNNING
        session++
        this.request = request
        phase = CapturePhase.AWAITING_PERMISSION
        return if (wasRunning) {
            listOf(CaptureEffect.StopService, CaptureEffect.RequestPermissions)
        } else {
            listOf(CaptureEffect.RequestPermissions)
        }
    }

    /** The permission dialog ended; [granted] is true when RECORD_AUDIO is granted. */
    fun onPermissions(granted: Boolean): List<CaptureEffect> {
        if (phase != CapturePhase.AWAITING_PERMISSION) return emptyList()
        if (!granted) return finish(CaptureEffect.Reply(false))
        phase = CapturePhase.AWAITING_CONSENT
        return listOf(CaptureEffect.RequestConsent)
    }

    /** The consent dialog ended; [granted] is true when the user allowed the projection. */
    fun onConsent(granted: Boolean): List<CaptureEffect> {
        if (phase != CapturePhase.AWAITING_CONSENT) return emptyList()
        if (!granted) return finish(CaptureEffect.Reply(false))
        val request = this.request ?: return finish(CaptureEffect.Reply(false))
        phase = CapturePhase.STARTING
        return listOf(CaptureEffect.StartService(session, request))
    }

    /** The service of [session] records. */
    fun onServiceStarted(session: Int): List<CaptureEffect> {
        if (session != this.session || phase != CapturePhase.STARTING) return emptyList()
        phase = CapturePhase.RUNNING
        return listOf(CaptureEffect.Reply(true))
    }

    /** The service of [session] failed with [message] (it cleaned up and stopped itself). */
    fun onServiceFailed(session: Int, message: String): List<CaptureEffect> {
        if (session != this.session) return emptyList()
        return when (phase) {
            CapturePhase.STARTING -> finish(CaptureEffect.Reply(false), emit(PlatformEvent.captureError(message)))
            CapturePhase.RUNNING -> finish(emit(PlatformEvent.captureError(message)))
            else -> emptyList()
        }
    }

    /**
     * The service of [session] stopped without Dart asking ([message]: why), for example from
     * the notification or because the system ended the projection.
     */
    fun onServiceStopped(session: Int, message: String?): List<CaptureEffect> {
        if (session != this.session) return emptyList()
        return when (phase) {
            CapturePhase.STARTING -> finish(CaptureEffect.Reply(false), emit(PlatformEvent.captureStopped(message)))
            CapturePhase.RUNNING -> finish(emit(PlatformEvent.captureStopped(message)))
            else -> emptyList()
        }
    }

    /** The start timeout of [session] fired. */
    fun onStartTimeout(session: Int): List<CaptureEffect> {
        if (session != this.session || phase != CapturePhase.STARTING) return emptyList()
        return finish(
            CaptureEffect.Reply(false),
            CaptureEffect.StopService,
            emit(PlatformEvent.captureError("the capture service did not start in time")),
        )
    }

    /** A `stopSystemCapture` call: cancels a pending start and stops a running capture. */
    fun stop(): List<CaptureEffect> =
        when (phase) {
            CapturePhase.IDLE -> emptyList()
            CapturePhase.AWAITING_PERMISSION, CapturePhase.AWAITING_CONSENT -> finish(CaptureEffect.Reply(false))
            CapturePhase.STARTING -> finish(CaptureEffect.Reply(false), CaptureEffect.StopService)
            CapturePhase.RUNNING -> finish(CaptureEffect.StopService)
        }

    /**
     * The activity that shows the dialogs went away: a start waiting for a dialog cannot
     * complete, so it is answered `false`. A starting or running service is left alone.
     */
    fun onHostLost(): List<CaptureEffect> =
        when (phase) {
            CapturePhase.AWAITING_PERMISSION, CapturePhase.AWAITING_CONSENT -> finish(CaptureEffect.Reply(false))
            else -> emptyList()
        }

    private fun emit(event: PlatformEvent) = CaptureEffect.Emit(event)

    /** Back to idle: invalidates the session so later reports of its service are ignored. */
    private fun finish(vararg effects: CaptureEffect): List<CaptureEffect> {
        phase = CapturePhase.IDLE
        request = null
        session++
        return effects.toList()
    }
}
