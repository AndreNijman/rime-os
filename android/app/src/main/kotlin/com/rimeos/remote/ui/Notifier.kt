package com.rimeos.remote.ui

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
import com.rimeos.remote.core.agent.Alert
import com.rimeos.remote.core.agent.NotificationContent

/**
 * Posting the alerts the poll loop raised (P1-058).
 *
 * ## What this class deliberately does NOT decide
 *
 * Nothing about content. The title, the text, the id and the grouping all come
 * from [NotificationContent], in `:core`, where `NotificationsTest` drives a
 * session carrying a password through the whole watcher-to-notification path
 * and asserts the password reaches none of the rendered fields. Nothing in
 * this project can test the builder calls below — no device, no emulator, no
 * Robolectric, and `assembleDebug` compiles them without running them — so a
 * privacy rule written here would be a privacy rule with no gate in front of
 * it. This file is the plumbing and only the plumbing.
 *
 * ## Two callers, and that is the point
 *
 * `RemoteViewModel`'s poll loop calls [post] while the app is running, and
 * `PushReceiver` calls it when a UnifiedPush distributor wakes the process
 * with the app closed. Both go through [NotificationContent], so both produce
 * the same id for the same (machine, session, kind) and a moment that reaches
 * this phone twice occupies one line in the shade rather than two. That is
 * P1-058's fourth criterion, and it is resolved by the id rather than by any
 * bookkeeping either caller does.
 *
 * **An earlier version of this note said there was no push transport
 * anywhere.** That was true when it was written. The poll path is still the
 * only one that works with no distributor installed, and it still runs in
 * `viewModelScope` — which survives the app going to the background but not
 * the process being killed.
 */
class Notifier(private val context: Context) {

    /**
     * Whether this phone will actually show anything.
     *
     * Three separate answers folded into one boolean would hide the case that
     * matters: on API 33+ the permission may simply never have been asked for,
     * which is not the same as the user having said no. [permissionNeeded]
     * tells the caller which.
     */
    val enabled: Boolean
        get() = NotificationManagerCompat.from(context).areNotificationsEnabled() && !permissionNeeded

    /** True when `POST_NOTIFICATIONS` has not been granted and must be asked for. */
    val permissionNeeded: Boolean
        get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, PERMISSION) != PackageManager.PERMISSION_GRANTED

    /**
     * Create the channel.
     *
     * Idempotent — `createNotificationChannel` on an existing id updates the
     * name and leaves the user's own choices about it alone, which is why this
     * can be called on every start.
     */
    fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(
            NotificationContent.CHANNEL,
            "Agents",
            // DEFAULT and not HIGH: these are not calls. An agent finishing a
            // turn should light the screen, not interrupt whatever is on it.
            NotificationManager.IMPORTANCE_DEFAULT,
        ).apply {
            description = "An agent on one of your machines needs you."
            // Off, deliberately. The content is minimized precisely because a
            // lock screen is read by whoever is in the room; a launcher badge
            // that also announces a count widens that for no gain, and the
            // Agent Center already shows the number to somebody who has the
            // app open.
            setShowBadge(false)
        }
        context.getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
    }

    /**
     * Post one alert.
     *
     * Silently does nothing when the permission is missing, because the
     * alternative — throwing from inside a poll loop — would take the poll
     * down and lose the session list as well as the notification. The caller
     * shows the state of [enabled] on screen instead.
     */
    fun post(alert: Alert, machineName: String) {
        if (permissionNeeded) return
        val content = NotificationContent.of(alert, machineName)
        val notification = NotificationCompat.Builder(context, NotificationContent.CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(content.title)
            .setContentText(content.text)
            .setGroup(content.group)
            .setAutoCancel(true)
            .setCategory(NotificationCompat.CATEGORY_STATUS)
            // PUBLIC is safe here and only because of what is in the content:
            // fixed strings, an adapter name and a machine name. It would not
            // be safe for anything carrying `SessionInfo.detail`, which is why
            // that rule lives in `:core` with a test on it rather than being a
            // habit of this file.
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setContentIntent(openIntent(content))
            .build()
        runCatching {
            NotificationManagerCompat.from(context).notify(content.id, notification)
        }
    }

    /**
     * Take down everything this app has posted.
     *
     * There is deliberately no per-machine version. Ids are derived from
     * (machine, session, kind) and the session range is not enumerable, so a
     * `clear(machine)` could only cancel the handful of ids it could guess —
     * a function named for a thing it does not do, which is the same shape as
     * a picker over a verb that does not exist. When one is needed, the ids
     * posted per machine have to be remembered, not recomputed.
     */
    fun cancelAll() = NotificationManagerCompat.from(context).cancelAll()

    /**
     * The tap.
     *
     * `FLAG_ACTIVITY_SINGLE_TOP` on a `singleTask` activity delivers this
     * through `onNewIntent` to the app that is already running, rather than
     * building a second copy on top of the first — which would mean two view
     * models, two stores and a race over the same file.
     *
     * `FLAG_UPDATE_CURRENT` because the id is stable per
     * [NotificationContent.idFor]: re-posting an alert for the same session
     * and state must carry the NEW extras, and without this flag a
     * `PendingIntent` matching an existing one keeps the old ones. The extras
     * here are (machine, session) and both can legitimately differ between two
     * posts that share an id — a machine's session pruned and another taking
     * the number is exactly that case.
     */
    private fun openIntent(content: NotificationContent): PendingIntent {
        val intent = Intent(context, MainActivity::class.java).apply {
            action = ACTION_OPEN_ALERT
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
            putExtra(EXTRA_MACHINE, content.machine)
            putExtra(EXTRA_SESSION, content.session)
            putExtra(EXTRA_NEX, content.opensNex)
        }
        return PendingIntent.getActivity(
            context,
            content.id,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    companion object {
        const val PERMISSION: String = "android.permission.POST_NOTIFICATIONS"
        const val ACTION_OPEN_ALERT: String = "com.rimeos.remote.OPEN_ALERT"
        const val EXTRA_MACHINE: String = "machine"
        const val EXTRA_SESSION: String = "session"
        const val EXTRA_NEX: String = "nex"
    }
}
