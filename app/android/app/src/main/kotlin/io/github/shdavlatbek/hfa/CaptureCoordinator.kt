package io.github.shdavlatbek.hfa

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.util.Log
import io.flutter.plugin.common.MethodChannel
import io.github.shdavlatbek.hfa.capture.CaptureEffect
import io.github.shdavlatbek.hfa.capture.CapturePhase
import io.github.shdavlatbek.hfa.capture.CaptureRequest
import io.github.shdavlatbek.hfa.capture.CaptureStateMachine

/**
 * Runs the [CaptureStateMachine] of `startSystemCapture` / `stopSystemCapture` against Android:
 * the permission and consent dialogs of the [Host] activity, the [CaptureService], the pending
 * `MethodChannel.Result` and [PlatformEvents].
 *
 * Process-wide because the capture service outlives the activity. Main thread only: the
 * activity calls it from channel handlers and activity results, the service from its main
 * thread callbacks.
 */
object CaptureCoordinator {
    private const val TAG = "hfa.capture"

    /** How long the service may take to report that it records. */
    private const val START_TIMEOUT_MS = 10_000L

    /** `PlatformException` code of a `startSystemCapture` refused for lack of RECORD_AUDIO. */
    private const val ERROR_PERMISSION_DENIED = "permissionDenied"

    /** The activity that shows the dialogs. */
    interface Host {
        /** Asks for the capture permissions; answers with [onPermissions]. */
        fun requestCapturePermissions()

        /** Shows the MediaProjection consent dialog; answers with [onConsent]. */
        fun requestCaptureConsent()
    }

    private val machine = CaptureStateMachine()
    private val mainHandler = Handler(Looper.getMainLooper())
    private var host: Host? = null
    private var pendingResult: MethodChannel.Result? = null

    /** The consent being handed to the service (only set while [onConsent] runs). */
    private var consent: Pair<Int, Intent>? = null

    /** [host] shows the dialogs from now on. */
    fun attach(host: Host) {
        this.host = host
    }

    /** [host] is going away; a start waiting for one of its dialogs is answered `false`. */
    fun detach(context: Context, host: Host) {
        if (this.host !== host) return
        this.host = null
        execute(context, machine.onHostLost())
    }

    /** `startSystemCapture`: answers [result] with whether capture started. */
    fun start(context: Context, request: CaptureRequest, result: MethodChannel.Result) {
        val effects = machine.start(request)
        if (effects == null) {
            Log.w(TAG, "startSystemCapture refused: another start is in progress")
            result.success(false)
            return
        }
        pendingResult = result
        PlatformEvents.clearUnheardCaptureEnd()
        execute(context, effects)
    }

    /** `stopSystemCapture`. */
    fun stop(context: Context) = execute(context, machine.stop())

    /**
     * `captureStatus`: whether a capture service records right now. A Flutter UI created while
     * the service kept running (the activity was destroyed, the process lived on) asks this to
     * take over the capture it did not start.
     */
    fun isRunning(): Boolean = machine.phase == CapturePhase.RUNNING

    /** The permission dialog ended; [granted]: RECORD_AUDIO is granted. */
    fun onPermissions(context: Context, granted: Boolean) = execute(context, machine.onPermissions(granted))

    /** The consent dialog ended with [resultCode] and [data]. */
    fun onConsent(context: Context, resultCode: Int, data: Intent?) {
        consent = if (resultCode == Activity.RESULT_OK && data != null) resultCode to data else null
        try {
            execute(context, machine.onConsent(consent != null))
        } finally {
            consent = null // single use: a MediaProjection token can be used once
        }
    }

    /**
     * The service records for [session]. Returns whether that capture is still wanted; when
     * `false` (stopped, timed out or replaced meanwhile) the service must end it without
     * reporting.
     */
    fun onServiceStarted(context: Context, session: Int): Boolean {
        execute(context, machine.onServiceStarted(session))
        return machine.isCurrentCapture(session)
    }

    /** The service of [session] failed with [message]. */
    fun onServiceFailed(context: Context, session: Int, message: String) =
        execute(context, machine.onServiceFailed(session, message))

    /** The service of [session] stopped without Dart asking. */
    fun onServiceStopped(context: Context, session: Int, message: String?) =
        execute(context, machine.onServiceStopped(session, message))

    private fun execute(context: Context, effects: List<CaptureEffect>) {
        for (effect in effects) {
            when (effect) {
                is CaptureEffect.Reply -> {
                    pendingResult?.success(effect.started)
                    pendingResult = null
                }
                CaptureEffect.ReplyPermissionDenied -> {
                    pendingResult?.error(
                        ERROR_PERMISSION_DENIED,
                        context.getString(R.string.capture_error_permission),
                        null,
                    )
                    pendingResult = null
                }
                is CaptureEffect.Emit -> PlatformEvents.emit(effect.event)
                CaptureEffect.RequestPermissions -> {
                    val current = host
                    if (current == null) {
                        // No activity to ask: the start cannot complete (not a refusal).
                        execute(context, machine.onHostLost())
                    } else {
                        current.requestCapturePermissions()
                    }
                }
                CaptureEffect.RequestConsent -> {
                    val current = host
                    if (current == null) {
                        execute(context, machine.onConsent(false))
                    } else {
                        current.requestCaptureConsent()
                    }
                }
                is CaptureEffect.StartService -> startService(context, effect)
                CaptureEffect.StopService -> CaptureService.stop(context)
            }
        }
    }

    private fun startService(context: Context, effect: CaptureEffect.StartService) {
        val (resultCode, data) = consent ?: run {
            execute(context, machine.onServiceFailed(effect.session, "no MediaProjection consent"))
            return
        }
        try {
            CaptureService.start(context, effect.session, effect.request, resultCode, data)
        } catch (e: RuntimeException) {
            // ForegroundServiceStartNotAllowedException, SecurityException.
            Log.e(TAG, "capture service not started", e)
            val message = "the capture service could not start: ${e.message}"
            execute(context, machine.onServiceFailed(effect.session, message))
            return
        }
        val appContext = context.applicationContext
        mainHandler.postDelayed({ execute(appContext, machine.onStartTimeout(effect.session)) }, START_TIMEOUT_MS)
    }
}
