package io.github.shdavlatbek.hfa

import android.Manifest
import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Process
import android.util.Log
import io.github.shdavlatbek.hfa.capture.CaptureLoop
import io.github.shdavlatbek.hfa.capture.CaptureRequest
import io.github.shdavlatbek.hfa.capture.LoopExit
import io.github.shdavlatbek.hfa.capture.PcmFormat
import io.github.shdavlatbek.hfa.capture.PcmPipe
import io.github.shdavlatbek.hfa.capture.SampleEncoding

/**
 * Foreground service (type `mediaProjection`) that captures the audio other apps play and
 * pushes it into the Rust sender's external feed (docs/CONTRACTS.md §8.1, §8.3).
 *
 * Start: `startForeground` first (Android 14 requires it before `getMediaProjection`), then
 * `MediaProjection` from the consent result → `AudioPlaybackCaptureConfiguration` (usages MEDIA,
 * GAME, UNKNOWN) → `AudioRecord` (float when supported, else 16-bit; the format the feed was
 * registered with) → a capture thread that reads 10 ms chunks into one reused buffer and calls
 * [NativeBridge]. Results go to [CaptureCoordinator] with the session number of the start.
 *
 * Every end — Dart's stop, the notification's Stop action, `MediaProjection.Callback.onStop`
 * (the user or the system ended the projection, for example on screen lock), a recording or
 * engine failure, `onDestroy` — goes through [teardown], which is idempotent. All state is
 * touched on the main thread only; the capture thread reports through [mainHandler].
 */
class CaptureService : Service() {
    private val mainHandler = Handler(Looper.getMainLooper())

    /** Id of the latest start command, for `stopSelf(startId)` (a newer start keeps the service). */
    private var lastStartId = 0
    private var active: ActiveCapture? = null
    private var wifiLock: WifiManager.WifiLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        lastStartId = startId
        when (intent?.action) {
            ACTION_START -> handleStart(intent)
            ACTION_STOP -> {
                // Dart asked: the coordinator already moved on, so nothing is reported.
                teardown()
                finish()
            }
            ACTION_STOP_FROM_NOTIFICATION -> if (active == null) {
                finish()
            } else {
                endActive { session ->
                    CaptureCoordinator.onServiceStopped(this, session, getString(R.string.capture_stopped_by_user))
                }
            }
            else -> {
                Log.w(TAG, "unexpected start command ${intent?.action}")
                if (active == null) finish()
            }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        endActive { session ->
            CaptureCoordinator.onServiceStopped(this, session, getString(R.string.capture_stopped_by_system))
        }
        SystemLocks.release(wifiLock)
        wifiLock = null
        mainHandler.removeCallbacksAndMessages(null)
        super.onDestroy()
    }

    private fun handleStart(intent: Intent) {
        val session = intent.getIntExtra(EXTRA_SESSION, -1)
        // Always go foreground first: the service was started with startForegroundService, and
        // Android 14+ refuses getMediaProjection before a mediaProjection-type foreground service.
        try {
            startForeground(
                Notifications.ID_CAPTURE,
                buildNotification(),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION,
            )
        } catch (e: RuntimeException) {
            // SecurityException (no consent / permission) or ForegroundServiceStartNotAllowedException.
            Log.e(TAG, "startForeground failed", e)
            CaptureCoordinator.onServiceFailed(this, session, getString(R.string.capture_error_foreground, e.message))
            finish()
            return
        }
        // A start replaces a previous capture (the coordinator already invalidated its session).
        teardown()
        try {
            active = startCapture(intent, session)
        } catch (e: CaptureStartException) {
            Log.e(TAG, "capture start failed: ${e.message}", e.cause)
            CaptureCoordinator.onServiceFailed(this, session, e.message ?: "capture failed")
            finish()
            return
        }
        if (!CaptureCoordinator.onServiceStarted(this, session)) {
            // Stale start (stopped, timed out or replaced while starting): its stop command may
            // never be delivered, so the service ends the capture itself.
            Log.i(TAG, "capture session $session is no longer wanted; stopping it")
            teardown()
            finish()
        }
    }

    /** Opens the projection and the recorder and starts the capture thread. */
    private fun startCapture(intent: Intent, session: Int): ActiveCapture {
        val request = try {
            CaptureRequest(
                intent.getIntExtra(EXTRA_FEED_ID, 0),
                intent.getIntExtra(EXTRA_SAMPLE_RATE, 0),
                intent.getIntExtra(EXTRA_CHANNELS, 0),
            )
        } catch (e: IllegalArgumentException) {
            throw CaptureStartException("invalid capture request: ${e.message}", e)
        }
        NativeBridge.loadError?.let { throw CaptureStartException("native library not loaded: $it") }
        val consent = consentData(intent) ?: throw CaptureStartException("missing MediaProjection consent")
        val resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, 0)
        val manager = getSystemService(MediaProjectionManager::class.java)
            ?: throw CaptureStartException("no MediaProjectionManager")
        val projection = try {
            manager.getMediaProjection(resultCode, consent)
        } catch (e: RuntimeException) {
            throw CaptureStartException(getString(R.string.capture_error_projection, e.message), e)
        } ?: throw CaptureStartException(getString(R.string.capture_error_projection, "consent not valid"))

        val callback = ProjectionCallback(session)
        var recorder: Recorder? = null
        try {
            projection.registerCallback(callback, mainHandler)
            recorder = openRecorder(projection, request)
            val capture = ActiveCapture(session, projection, callback, recorder)
            recorder.record.startRecording()
            if (recorder.record.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
                throw CaptureStartException(getString(R.string.capture_error_record, "recording did not start"))
            }
            if (wifiLock == null) wifiLock = SystemLocks.wifiLowLatency(this, "hfa:capture")
            SystemLocks.acquire(wifiLock)
            // Last: once the thread runs, it owns the release of the recorder.
            capture.startThread()
            Log.i(
                TAG,
                "capturing session $session: ${request.sampleRate} Hz, ${request.channels} ch, ${recorder.encoding}",
            )
            return capture
        } catch (e: RuntimeException) {
            SystemLocks.release(wifiLock)
            recorder?.record?.release()
            projection.unregisterCallback(callback)
            projection.stop()
            throw e as? CaptureStartException
                ?: CaptureStartException(getString(R.string.capture_error_record, e.message), e)
        }
    }

    /** The recorder in the best encoding the device supports for [request]. */
    private fun openRecorder(projection: MediaProjection, request: CaptureRequest): Recorder {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            throw CaptureStartException(getString(R.string.capture_error_record, "RECORD_AUDIO not granted"))
        }
        val playbackConfig = AudioPlaybackCaptureConfiguration.Builder(projection)
            .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
            .addMatchingUsage(AudioAttributes.USAGE_GAME)
            .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
            .build()
        val channelMask = if (request.channels == 1) AudioFormat.CHANNEL_IN_MONO else AudioFormat.CHANNEL_IN_STEREO
        val chunkFrames = PcmFormat.chunkFrames(request.sampleRate)
        var lastError = "no supported PCM encoding"
        for (encoding in SampleEncoding.entries) {
            val androidEncoding = when (encoding) {
                SampleEncoding.FLOAT32 -> AudioFormat.ENCODING_PCM_FLOAT
                SampleEncoding.PCM16 -> AudioFormat.ENCODING_PCM_16BIT
            }
            val minBytes = AudioRecord.getMinBufferSize(request.sampleRate, channelMask, androidEncoding)
            if (minBytes <= 0) {
                lastError = "$encoding unsupported (getMinBufferSize = $minBytes)"
                continue
            }
            val record = try {
                AudioRecord.Builder()
                    .setAudioFormat(
                        AudioFormat.Builder()
                            .setEncoding(androidEncoding)
                            .setSampleRate(request.sampleRate)
                            .setChannelMask(channelMask)
                            .build(),
                    )
                    .setBufferSizeInBytes(
                        PcmFormat.recordBufferBytes(minBytes, chunkFrames, request.channels, encoding),
                    )
                    .setAudioPlaybackCaptureConfig(playbackConfig)
                    .build()
            } catch (e: UnsupportedOperationException) {
                lastError = "$encoding: ${e.message}"
                continue
            } catch (e: IllegalArgumentException) {
                lastError = "$encoding: ${e.message}"
                continue
            }
            if (record.state != AudioRecord.STATE_INITIALIZED) {
                record.release()
                lastError = "$encoding: AudioRecord not initialized"
                continue
            }
            val samples = chunkFrames * request.channels
            val pipe = when (encoding) {
                SampleEncoding.FLOAT32 -> FloatPipe(record, FloatArray(samples), request)
                SampleEncoding.PCM16 -> ShortPipe(record, ShortArray(samples), request)
            }
            return Recorder(record, encoding, pipe)
        }
        throw CaptureStartException(getString(R.string.capture_error_record, lastError))
    }

    /** Tears the active capture down and reports it through [report] (with its session). */
    private fun endActive(report: (Int) -> Unit) {
        val capture = active ?: return
        teardown()
        report(capture.session)
        finish()
    }

    /** Stops and releases the active capture, if any (idempotent, main thread). */
    private fun teardown() {
        val capture = active ?: return
        active = null
        capture.close()
        SystemLocks.release(wifiLock)
    }

    /** Leaves the foreground and stops, unless a newer start command is pending. */
    private fun finish() {
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf(lastStartId)
    }

    /** Called on the main thread when the capture thread of [capture] ended by itself. */
    private fun onLoopExit(capture: ActiveCapture, exit: LoopExit) {
        if (active !== capture) return
        val message = when (exit) {
            LoopExit.Stopped -> return
            is LoopExit.ReadFailed -> getString(R.string.capture_error_record, "AudioRecord.read = ${exit.code}")
            is LoopExit.PushFailed -> exit.reason
        }
        Log.e(TAG, "capture session ${capture.session} failed: $message")
        teardown()
        CaptureCoordinator.onServiceFailed(this, capture.session, message)
        finish()
    }

    private fun buildNotification(): Notification {
        val stop = PendingIntent.getService(
            this,
            REQUEST_STOP,
            Intent(this, CaptureService::class.java).setAction(ACTION_STOP_FROM_NOTIFICATION),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val action = Notification.Action.Builder(
            null,
            getString(R.string.action_stop),
            stop,
        ).build()
        return Notifications.ongoing(
            this,
            Notifications.CHANNEL_CAPTURE,
            getString(R.string.capture_notification_title),
            getString(R.string.capture_notification_text),
            action,
        )
    }

    @Suppress("DEPRECATION") // getParcelableExtra(String) is the only variant before API 33.
    private fun consentData(intent: Intent): Intent? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent::class.java)
        } else {
            intent.getParcelableExtra(EXTRA_RESULT_DATA)
        }

    /** Reports the end of the projection of [session] (user, system, screen lock, other app). */
    private inner class ProjectionCallback(private val session: Int) : MediaProjection.Callback() {
        override fun onStop() {
            val capture = active
            if (capture == null || capture.session != session) return
            endActive {
                CaptureCoordinator.onServiceStopped(
                    this@CaptureService,
                    it,
                    getString(R.string.capture_stopped_by_system),
                )
            }
        }
    }

    /** A running capture: projection + recorder + capture thread. */
    private inner class ActiveCapture(
        val session: Int,
        private val projection: MediaProjection,
        private val callback: ProjectionCallback,
        private val recorder: Recorder,
    ) {
        @Volatile
        private var running = true

        /** Guards `stop` against `release` of the recorder (the thread releases it). */
        private val recordLock = Any()
        private var released = false
        private var thread: Thread? = null

        fun startThread() {
            val loop = CaptureLoop(recorder.pipe)
            thread = Thread({
                Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
                val exit = try {
                    loop.run { running }
                } finally {
                    synchronized(recordLock) {
                        released = true
                        recorder.record.release()
                    }
                }
                mainHandler.post { onLoopExit(this@ActiveCapture, exit) }
            }, "hfa-capture-$session").apply { start() }
        }

        /** Stops the thread (the blocking read returns after `stop`) and releases everything. */
        fun close() {
            running = false
            synchronized(recordLock) {
                if (!released) {
                    try {
                        recorder.record.stop()
                    } catch (e: IllegalStateException) {
                        Log.w(TAG, "AudioRecord.stop", e)
                    }
                }
            }
            val worker = thread
            if (worker == null) {
                synchronized(recordLock) {
                    if (!released) {
                        released = true
                        recorder.record.release()
                    }
                }
            } else {
                worker.join(JOIN_TIMEOUT_MS)
                if (worker.isAlive) Log.w(TAG, "capture thread still running; it releases the recorder itself")
            }
            projection.unregisterCallback(callback)
            projection.stop()
        }
    }

    /** An initialized `AudioRecord` with its encoding and buffer. */
    private class Recorder(val record: AudioRecord, val encoding: SampleEncoding, val pipe: PcmPipe)

    private class FloatPipe(
        private val record: AudioRecord,
        private val buffer: FloatArray,
        private val request: CaptureRequest,
    ) : PcmPipe {
        override val channels: Int get() = request.channels

        override fun read(): Int = record.read(buffer, 0, buffer.size, AudioRecord.READ_BLOCKING)

        override fun push(frames: Int): Int =
            NativeBridge.pushPcm(request.feedId, buffer, frames, request.channels, request.sampleRate)
    }

    private class ShortPipe(
        private val record: AudioRecord,
        private val buffer: ShortArray,
        private val request: CaptureRequest,
    ) : PcmPipe {
        override val channels: Int get() = request.channels

        override fun read(): Int = record.read(buffer, 0, buffer.size, AudioRecord.READ_BLOCKING)

        override fun push(frames: Int): Int =
            NativeBridge.pushPcm16(request.feedId, buffer, frames, request.channels, request.sampleRate)
    }

    /** A start failure with a user-readable message. */
    private class CaptureStartException(message: String, cause: Throwable? = null) :
        RuntimeException(message, cause)

    companion object {
        private const val TAG = "hfa.capture"
        private const val ACTION_START = "io.github.shdavlatbek.hfa.action.CAPTURE_START"
        private const val ACTION_STOP = "io.github.shdavlatbek.hfa.action.CAPTURE_STOP"
        private const val ACTION_STOP_FROM_NOTIFICATION = "io.github.shdavlatbek.hfa.action.CAPTURE_STOP_USER"
        private const val EXTRA_SESSION = "session"
        private const val EXTRA_FEED_ID = "feedId"
        private const val EXTRA_SAMPLE_RATE = "sampleRate"
        private const val EXTRA_CHANNELS = "channels"
        private const val EXTRA_RESULT_CODE = "resultCode"
        private const val EXTRA_RESULT_DATA = "resultData"
        private const val REQUEST_STOP = 1
        private const val JOIN_TIMEOUT_MS = 500L

        /**
         * Starts capturing for [session] with the MediaProjection consent ([resultCode], [data]).
         *
         * @throws RuntimeException when the system refuses to start the service.
         */
        fun start(context: Context, session: Int, request: CaptureRequest, resultCode: Int, data: Intent) {
            val intent = Intent(context, CaptureService::class.java)
                .setAction(ACTION_START)
                .putExtra(EXTRA_SESSION, session)
                .putExtra(EXTRA_FEED_ID, request.feedId)
                .putExtra(EXTRA_SAMPLE_RATE, request.sampleRate)
                .putExtra(EXTRA_CHANNELS, request.channels)
                .putExtra(EXTRA_RESULT_CODE, resultCode)
                .putExtra(EXTRA_RESULT_DATA, data)
            context.startForegroundService(intent)
        }

        /**
         * Stops the capture without reporting it. Sent as a command (not `stopService`) so it is
         * handled after a pending start, which must reach `startForeground` first.
         */
        fun stop(context: Context) {
            try {
                context.startService(Intent(context, CaptureService::class.java).setAction(ACTION_STOP))
            } catch (e: IllegalStateException) {
                // Background start refused: the service is not running (a running foreground
                // service makes the app exempt), so there is nothing to stop.
                Log.i(TAG, "stop not delivered: ${e.message}")
            }
        }
    }
}
