package io.github.shdavlatbek.hfa

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.util.Log

/**
 * Non-reference-counted Wi-Fi and power locks: acquiring twice or releasing an unheld lock is
 * harmless, so every cleanup path can simply release.
 */
internal object SystemLocks {
    private const val TAG = "hfa.locks"

    /** A multicast lock (mDNS discovery and advertising), or `null` without Wi-Fi. */
    fun multicast(context: Context, tag: String): WifiManager.MulticastLock? =
        wifi(context)?.createMulticastLock(tag)?.apply { setReferenceCounted(false) }

    /**
     * A low-latency Wi-Fi lock: disables Wi-Fi power save while the app is in the foreground
     * with the screen on (the system ignores it otherwise), which avoids bursty packet delivery.
     */
    fun wifiLowLatency(context: Context, tag: String): WifiManager.WifiLock? =
        wifi(context)?.createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, tag)?.apply {
            setReferenceCounted(false)
        }

    private fun wifi(context: Context): WifiManager? =
        context.applicationContext.getSystemService(WifiManager::class.java).also {
            if (it == null) Log.w(TAG, "no WifiManager")
        }

    /** Acquires [lock] if present; a failure is logged, not thrown (the lock is an optimisation). */
    fun acquire(lock: WifiManager.MulticastLock?) {
        try {
            if (lock != null && !lock.isHeld) lock.acquire()
        } catch (e: RuntimeException) {
            Log.w(TAG, "multicast lock not acquired", e)
        }
    }

    /** Releases [lock] if held. */
    fun release(lock: WifiManager.MulticastLock?) {
        if (lock != null && lock.isHeld) lock.release()
    }

    /** Acquires [lock] if present; a failure is logged, not thrown. */
    fun acquire(lock: WifiManager.WifiLock?) {
        try {
            if (lock != null && !lock.isHeld) lock.acquire()
        } catch (e: RuntimeException) {
            Log.w(TAG, "Wi-Fi lock not acquired", e)
        }
    }

    /** Releases [lock] if held. */
    fun release(lock: WifiManager.WifiLock?) {
        if (lock != null && lock.isHeld) lock.release()
    }
}

/**
 * A partial wake lock (CPU on, screen may be off) held until [release], renewed before its
 * timeout so a forgotten lock can never keep the device awake for more than [TIMEOUT_MS].
 *
 * Main thread only.
 */
internal class RenewingWakeLock(context: Context, tag: String) {
    private val lock: PowerManager.WakeLock? =
        context.applicationContext.getSystemService(PowerManager::class.java)
            ?.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, tag)
            ?.apply { setReferenceCounted(false) }
    private val handler = Handler(Looper.getMainLooper())
    private val renew = object : Runnable {
        override fun run() {
            lock?.acquire(TIMEOUT_MS)
            handler.postDelayed(this, RENEW_MS)
        }
    }
    private var held = false

    /** Acquires the lock (idempotent). */
    fun acquire() {
        if (held) return
        held = true
        renew.run()
    }

    /** Releases the lock (idempotent). */
    fun release() {
        if (!held) return
        held = false
        handler.removeCallbacks(renew)
        if (lock?.isHeld == true) lock.release()
    }

    private companion object {
        /** Each acquisition expires after 2 hours... */
        const val TIMEOUT_MS: Long = 2L * 60 * 60 * 1000

        /** ...and is renewed every hour while held. */
        const val RENEW_MS: Long = 60L * 60 * 1000
    }
}
