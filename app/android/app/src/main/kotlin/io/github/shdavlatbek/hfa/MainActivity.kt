package io.github.shdavlatbek.hfa

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionConfig
import android.media.projection.MediaProjectionManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.activity.result.contract.ActivityResultContracts
import io.flutter.embedding.android.FlutterFragmentActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import io.github.shdavlatbek.hfa.capture.CaptureRequest
import io.github.shdavlatbek.hfa.capture.CaptureSupport
import java.io.File

/**
 * The Flutter activity plus the native side of `MethodChannel('hfa/platform')` and
 * `EventChannel('hfa/platform/events')` (docs/CONTRACTS.md §8.3).
 *
 * A [FlutterFragmentActivity] (a `ComponentActivity`), so the permission and MediaProjection
 * consent dialogs use the activity-result API.
 */
class MainActivity : FlutterFragmentActivity(), CaptureCoordinator.Host {
    private val mainHandler = Handler(Looper.getMainLooper())

    /** Multicast lock for discovery (`acquireMulticastLock`); the hub service has its own. */
    private var multicastLock: WifiManager.MulticastLock? = null

    private val permissionLauncher =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            val granted = grants[Manifest.permission.RECORD_AUDIO] ?: hasPermission(Manifest.permission.RECORD_AUDIO)
            if (!granted) Log.w(TAG, "RECORD_AUDIO denied: system audio cannot be captured")
            CaptureCoordinator.onPermissions(this, granted)
        }

    private val consentLauncher =
        registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            CaptureCoordinator.onConsent(this, result.resultCode, result.data)
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        CaptureCoordinator.attach(this)
    }

    override fun onDestroy() {
        CaptureCoordinator.detach(this, this)
        SystemLocks.release(multicastLock)
        mainHandler.removeCallbacksAndMessages(null)
        super.onDestroy()
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        val messenger = flutterEngine.dartExecutor.binaryMessenger
        MethodChannel(messenger, METHOD_CHANNEL).setMethodCallHandler(::onMethodCall)
        EventChannel(messenger, EVENT_CHANNEL).setStreamHandler(PlatformEvents.handlerFor(messenger))
    }

    override fun cleanUpFlutterEngine(flutterEngine: FlutterEngine) {
        PlatformEvents.detach(flutterEngine.dartExecutor.binaryMessenger)
        super.cleanUpFlutterEngine(flutterEngine)
    }

    private fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "getDataDir" -> getDataDir(result)
            "startSystemCapture" -> startSystemCapture(call, result)
            "stopSystemCapture" -> {
                CaptureCoordinator.stop(this)
                result.success(null)
            }
            "startHubService" -> try {
                HubService.start(this)
                result.success(null)
            } catch (e: RuntimeException) {
                Log.e(TAG, "hub service not started", e)
                result.error(ERROR_SERVICE, "The hub service could not start: ${e.message}", null)
            }
            "stopHubService" -> {
                HubService.stop(this)
                result.success(null)
            }
            "acquireMulticastLock" -> {
                if (multicastLock == null) multicastLock = SystemLocks.multicast(this, "hfa:discovery")
                SystemLocks.acquire(multicastLock)
                result.success(null)
            }
            "releaseMulticastLock" -> {
                SystemLocks.release(multicastLock)
                result.success(null)
            }
            "captureSupport" -> result.success(CaptureSupport.forSdk(Build.VERSION.SDK_INT))
            "captureStatus" -> {
                val ended = PlatformEvents.takeUnheardCaptureEnd()
                result.success(
                    mapOf(
                        "running" to CaptureCoordinator.isRunning(),
                        "endedWhileAway" to ended?.let { it.message ?: "" },
                    ),
                )
            }
            else -> result.notImplemented()
        }
    }

    private fun getDataDir(result: MethodChannel.Result) {
        val dir = File(filesDir, DATA_DIR_NAME)
        if (dir.isDirectory || dir.mkdirs()) {
            result.success(dir.absolutePath)
        } else {
            result.error(ERROR_IO, "Cannot create ${dir.absolutePath}", null)
        }
    }

    /** minSdk is 29, so AudioPlaybackCapture is always available here. */
    private fun startSystemCapture(call: MethodCall, result: MethodChannel.Result) {
        val request = try {
            CaptureRequest.fromArguments(call.arguments)
        } catch (e: IllegalArgumentException) {
            result.error(ERROR_ARGUMENT, e.message, null)
            return
        }
        CaptureCoordinator.start(this, request, result)
    }

    override fun requestCapturePermissions() {
        val wanted = buildList {
            add(Manifest.permission.RECORD_AUDIO)
            // The capture notification (with its Stop action); capture works without it.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) add(Manifest.permission.POST_NOTIFICATIONS)
        }
        val missing = wanted.filterNot(::hasPermission)
        if (missing.isEmpty()) {
            // Answer asynchronously, like the dialog would (no re-entrant state machine calls).
            mainHandler.post { CaptureCoordinator.onPermissions(this, true) }
        } else {
            permissionLauncher.launch(missing.toTypedArray())
        }
    }

    override fun requestCaptureConsent() {
        val manager = getSystemService(MediaProjectionManager::class.java)
        if (manager == null) {
            mainHandler.post { CaptureCoordinator.onConsent(this, Activity.RESULT_CANCELED, null) }
            return
        }
        val intent: Intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            // Whole display: single-app sharing makes no sense for system audio.
            manager.createScreenCaptureIntent(MediaProjectionConfig.createConfigForDefaultDisplay())
        } else {
            manager.createScreenCaptureIntent()
        }
        try {
            consentLauncher.launch(intent)
        } catch (e: ActivityNotFoundException) {
            Log.e(TAG, "no MediaProjection consent activity", e)
            mainHandler.post { CaptureCoordinator.onConsent(this, Activity.RESULT_CANCELED, null) }
        }
    }

    private fun hasPermission(permission: String): Boolean =
        checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED

    private companion object {
        const val TAG = "hfa.activity"
        const val METHOD_CHANNEL = "hfa/platform"
        const val EVENT_CHANNEL = "hfa/platform/events"
        const val DATA_DIR_NAME = "hfa"
        const val ERROR_ARGUMENT = "invalidArgument"
        const val ERROR_SERVICE = "serviceFailed"
        const val ERROR_IO = "io"
    }
}
