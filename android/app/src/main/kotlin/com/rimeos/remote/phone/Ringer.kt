package com.rimeos.remote.phone

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.os.BatteryManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import com.rimeos.remote.ui.Notifier

/**
 * Rings this phone because a paired computer asked (`rime remote ring`).
 *
 * A notification on its own channel, played as an alarm: the alarm sound,
 * alarm audio attributes, repeated until somebody stops it or 30 s pass. It
 * has a Stop button and goes away when tapped. The same ringer serves both
 * paths a ring arrives on, the open link and a push.
 */
class Ringer(private val context: Context) {

    fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val sound = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
        val channel = NotificationChannel(CHANNEL, "Ring my phone", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "Your computer rings this phone so you can find it."
            setSound(
                sound,
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ALARM)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build(),
            )
            enableVibration(true)
            setShowBadge(false)
        }
        context.getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
    }

    /**
     * Whether a ring would reach the person: notifications allowed, and the
     * ring channel not switched off. The machine is told "rang" only when this
     * is true.
     */
    val canRing: Boolean
        get() {
            val notifier = Notifier(context)
            if (!notifier.enabled) return false
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return true
            val channel = context.getSystemService(NotificationManager::class.java)?.getNotificationChannel(CHANNEL)
            return channel == null || channel.importance != NotificationManager.IMPORTANCE_NONE
        }

    /** Ring now. Returns whether the notification was posted. */
    fun ring(machineName: String): Boolean {
        ensureChannel()
        if (!canRing) return false
        val name = machineName.ifEmpty { "Your computer" }
        val stop = PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, RingStopReceiver::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setContentTitle("Ringing")
            .setContentText("$name is ringing this phone so you can find it.")
            .setCategory(NotificationCompat.CATEGORY_ALARM)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setAutoCancel(true)
            .setTimeoutAfter(RING_MS)
            .setContentIntent(stop)
            .addAction(0, "Stop", stop)
            .build()
            .apply { flags = flags or android.app.Notification.FLAG_INSISTENT }
        return runCatching {
            NotificationManagerCompat.from(context).notify(ID, notification)
            true
        }.getOrDefault(false)
    }

    fun stop() = NotificationManagerCompat.from(context).cancel(ID)

    /**
     * A ring that came by push (contract §2.5): rung only if it was raised
     * within a minute of [nowS] and is not the ring this phone already rang
     * over the link from the same machine. Returns whether it rang.
     */
    fun ringFromPush(deviceId: String, machineName: String, startedS: Long, nowS: Long): Boolean {
        if (!com.rimeos.remote.core.phone.Phone.freshPush(startedS, nowS)) return false
        if (!rings.first(deviceId, startedS)) return false
        return ring(machineName)
    }

    companion object {
        const val CHANNEL: String = "ring"
        const val ID: Int = 0x52494E47 // "RING"
        const val RING_MS: Long = 30_000
        const val ACTION_STOP: String = "com.rimeos.remote.STOP_RING"

        /**
         * The last ring rung per machine, for the whole process: the link and
         * the push receiver both consult it, so a ring whose ack was lost and
         * which then arrives again by push rings once.
         */
        val rings = com.rimeos.remote.core.phone.RingMemory()

        /** The battery as Android last broadcast it, or null on a phone that has none. */
        fun battery(context: Context): Pair<Int?, Boolean?> {
            val sticky = runCatching {
                context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
            }.getOrNull() ?: return null to null
            return reading(sticky)
        }

        fun reading(intent: Intent): Pair<Int?, Boolean?> {
            if (!intent.getBooleanExtra(BatteryManager.EXTRA_PRESENT, true)) return null to null
            val level = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
            val scale = intent.getIntExtra(BatteryManager.EXTRA_SCALE, -1)
            val percent = if (level >= 0 && scale > 0) (level * 100 / scale).coerceIn(0, 100) else null
            // On a charger or not, from EXTRA_PLUGGED: the status can lag
            // behind (or, on an emulator, ignore) the cable, and "charging"
            // here means "plugged in", full or not.
            val plugged = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, -1)
            val charging = when {
                plugged > 0 -> true
                plugged == 0 -> false
                else -> when (intent.getIntExtra(BatteryManager.EXTRA_STATUS, -1)) {
                    BatteryManager.BATTERY_STATUS_CHARGING, BatteryManager.BATTERY_STATUS_FULL -> true
                    BatteryManager.BATTERY_STATUS_DISCHARGING, BatteryManager.BATTERY_STATUS_NOT_CHARGING -> false
                    else -> null
                }
            }
            return percent to charging
        }
    }
}

/** The Stop button and the tap. Not exported: only this app's own notification can send it. */
class RingStopReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == Ringer.ACTION_STOP) Ringer(context).stop()
    }
}
