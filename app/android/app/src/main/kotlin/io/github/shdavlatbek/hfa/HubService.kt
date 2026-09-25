package io.github.shdavlatbek.hfa

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.IBinder
import android.util.Log

/**
 * Foreground service (type `mediaPlayback`) that keeps the process, the CPU and multicast
 * reception alive while the Rust hub receives, mixes and plays the streams
 * (`startHubService` / `stopHubService`, docs/CONTRACTS.md §8.3).
 *
 * It holds a `MulticastLock` (mDNS advertising), a partial wake lock and a low-latency Wi-Fi
 * lock, and shows a persistent notification. The audio itself is played by Rust. It
 * deliberately does **not** request audio focus and has no media session: the hub mixes
 * alongside whatever the phone itself plays.
 */
class HubService : Service() {
    private var multicastLock: WifiManager.MulticastLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: RenewingWakeLock? = null
    private var lastStartId = 0

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        lastStartId = startId
        when (intent?.action) {
            ACTION_START -> handleStart()
            else -> {
                // ACTION_STOP (or an unexpected command): release and stop.
                releaseLocks()
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf(lastStartId)
            }
        }
        // Not sticky: after process death the Rust hub is gone too, so there is nothing to keep.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        releaseLocks()
        super.onDestroy()
    }

    private fun handleStart() {
        val notification = Notifications.ongoing(
            this,
            Notifications.CHANNEL_HUB,
            getString(R.string.hub_notification_title),
            getString(R.string.hub_notification_text),
        )
        try {
            startForeground(Notifications.ID_HUB, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } catch (e: RuntimeException) {
            Log.e(TAG, "startForeground failed", e)
            releaseLocks()
            stopSelf(lastStartId)
            return
        }
        if (multicastLock == null) multicastLock = SystemLocks.multicast(this, "hfa:hub-multicast")
        if (wifiLock == null) wifiLock = SystemLocks.wifiLowLatency(this, "hfa:hub")
        if (wakeLock == null) wakeLock = RenewingWakeLock(this, "hfa:hub")
        SystemLocks.acquire(multicastLock)
        SystemLocks.acquire(wifiLock)
        wakeLock?.acquire()
        Log.i(TAG, "hub service running")
    }

    private fun releaseLocks() {
        SystemLocks.release(multicastLock)
        SystemLocks.release(wifiLock)
        wakeLock?.release()
    }

    companion object {
        private const val TAG = "hfa.hub"
        private const val ACTION_START = "io.github.shdavlatbek.hfa.action.HUB_START"
        private const val ACTION_STOP = "io.github.shdavlatbek.hfa.action.HUB_STOP"

        /**
         * Starts (or keeps) the hub service. Idempotent.
         *
         * @throws RuntimeException when the system refuses a foreground service (app in background).
         */
        fun start(context: Context) {
            context.startForegroundService(Intent(context, HubService::class.java).setAction(ACTION_START))
        }

        /**
         * Stops the hub service. Sent as a command (not `stopService`) so it is handled after a
         * pending start, which must reach `startForeground` first.
         */
        fun stop(context: Context) {
            try {
                context.startService(Intent(context, HubService::class.java).setAction(ACTION_STOP))
            } catch (e: IllegalStateException) {
                // Background start refused: the service is not running, nothing to stop.
                Log.i(TAG, "stop not delivered: ${e.message}")
            }
        }
    }
}
