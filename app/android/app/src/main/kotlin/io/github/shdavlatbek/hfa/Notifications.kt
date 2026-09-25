package io.github.shdavlatbek.hfa

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build

/** Notification channels and the foreground-service notifications of both services. */
object Notifications {
    /** Channel of the capture ("streaming to a hub") notification. */
    const val CHANNEL_CAPTURE: String = "hfa_capture"

    /** Channel of the hub ("playing the mix") notification. */
    const val CHANNEL_HUB: String = "hfa_hub"

    /** Notification id of [CaptureService]. */
    const val ID_CAPTURE: Int = 1001

    /** Notification id of [HubService]. */
    const val ID_HUB: Int = 1002

    private const val REQUEST_OPEN_APP = 0

    /** Creates both channels (idempotent; low importance: no sound, no heads-up). */
    fun ensureChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        manager.createNotificationChannels(
            listOf(
                NotificationChannel(
                    CHANNEL_CAPTURE,
                    context.getString(R.string.channel_capture_name),
                    NotificationManager.IMPORTANCE_LOW,
                ).apply { description = context.getString(R.string.channel_capture_description) },
                NotificationChannel(
                    CHANNEL_HUB,
                    context.getString(R.string.channel_hub_name),
                    NotificationManager.IMPORTANCE_LOW,
                ).apply { description = context.getString(R.string.channel_hub_description) },
            ),
        )
    }

    /** An ongoing notification on [channel]; tapping it opens the app. */
    fun ongoing(
        context: Context,
        channel: String,
        title: String,
        text: String,
        vararg actions: Notification.Action,
    ): Notification {
        ensureChannels(context)
        val open = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        val contentIntent = PendingIntent.getActivity(
            context,
            REQUEST_OPEN_APP,
            open,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(context, channel)
            .setSmallIcon(R.drawable.ic_stat_headphone)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(contentIntent)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setCategory(Notification.CATEGORY_SERVICE)
            .apply {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                    // Show it at once: the user must see that audio is captured / played.
                    setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
                }
                actions.forEach { addAction(it) }
            }
            .build()
    }
}
