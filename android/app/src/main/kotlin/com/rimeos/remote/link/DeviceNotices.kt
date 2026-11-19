package com.rimeos.remote.link

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import com.rimeos.remote.MainActivity
import com.rimeos.remote.ui.Notifier

/**
 * The notification for something that happened to this phone's computers
 * without the person doing it (contract §2.15): a computer joined, or one was
 * forgotten because another removed it. Plumbing only; the words come from
 * `LinkHub`.
 *
 * A channel of its own, "Your computers", so turning these off leaves agent
 * alerts alone. The text names computers, nothing more, so it is shown on the
 * lock screen.
 */
internal class DeviceNotices(private val context: Context) {
    private var next = BASE_ID

    fun post(text: String) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Notifier.PERMISSION) != PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        ensureChannel()
        val open = PendingIntent.getActivity(
            context,
            BASE_ID,
            Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle("Your computers")
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setAutoCancel(true)
            .setCategory(NotificationCompat.CATEGORY_STATUS)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setContentIntent(open)
            .build()
        // A few ids, reused: Android shows at most 50 notifications of one
        // app, and these must never crowd out an agent's alert.
        val id = synchronized(this) { next.also { next = if (it >= BASE_ID + KEPT - 1) BASE_ID else it + 1 } }
        runCatching { NotificationManagerCompat.from(context).notify(id, notification) }
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(CHANNEL, "Your computers", NotificationManager.IMPORTANCE_DEFAULT).apply {
            description = "A computer joined, or was forgotten because another removed it."
            setShowBadge(false)
        }
        context.getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
    }

    companion object {
        const val CHANNEL: String = "computers"

        /** Clear of the ids `NotificationContent` derives for agent alerts. */
        private const val BASE_ID: Int = 0x52_4d_00

        /** How many of these notices stay shown; an older one is replaced. */
        const val KEPT: Int = 5

        /** Take every notice down. */
        fun cancelAll(context: Context) {
            val manager = NotificationManagerCompat.from(context)
            for (id in BASE_ID until BASE_ID + KEPT) manager.cancel(id)
        }
    }
}
