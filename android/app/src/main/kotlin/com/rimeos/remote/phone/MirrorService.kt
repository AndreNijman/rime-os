package com.rimeos.remote.phone

import android.app.Notification
import android.app.NotificationManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Process
import android.provider.Settings
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import com.rimeos.remote.core.phone.Content
import com.rimeos.remote.core.phone.Facts
import com.rimeos.remote.core.phone.Mirror
import com.rimeos.remote.link.LinkHub

/**
 * Reads this phone's notifications for the paired computers
 * (`docs/remote-live-contract.md` §2.7).
 *
 * Android binds this once the person grants notification access, and from
 * then on reports every notification on the phone to it. So the first thing
 * each callback does is ask [LinkHub.mirroring] whether anything could go
 * anywhere (unlocked, a computer connected that takes them); almost always
 * the answer is no and nothing is read. Then [Mirror.skip] decides, against
 * the apps the person chose. What survives is an app name, a title and a
 * text, handed to [LinkHub], which sends it over the connections already
 * open. Nothing here is logged or written down.
 */
class MirrorService : NotificationListenerService() {
    /** App labels by package, so a busy chat does not ask the package manager each time. */
    private val labels = object : LinkedHashMap<String, String>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, String>?) = size > 64
    }

    override fun onNotificationPosted(sbn: StatusBarNotification) {
        val hub = LinkHub.get(this)
        if (!hub.mirroring) return
        val n = sbn.notification ?: return
        val facts = Facts(
            pkg = sbn.packageName,
            ownPkg = packageName,
            allowed = hub.mirrorApps(),
            otherUser = sbn.user != Process.myUserHandle(),
            localOnly = n.flags and Notification.FLAG_LOCAL_ONLY != 0,
            secret = n.visibility == Notification.VISIBILITY_SECRET,
            groupSummary = n.flags and Notification.FLAG_GROUP_SUMMARY != 0,
            ongoing = n.flags and Notification.FLAG_ONGOING_EVENT != 0,
            foregroundService = n.flags and Notification.FLAG_FOREGROUND_SERVICE != 0,
            media = n.extras?.containsKey(Notification.EXTRA_MEDIA_SESSION) == true ||
                n.category == Notification.CATEGORY_TRANSPORT,
        )
        if (Mirror.skip(facts) != null) return
        val extras = n.extras ?: return
        val title = extras.getCharSequence(Notification.EXTRA_TITLE_BIG) ?: extras.getCharSequence(Notification.EXTRA_TITLE)
        val text = extras.getCharSequence(Notification.EXTRA_BIG_TEXT) ?: extras.getCharSequence(Notification.EXTRA_TEXT)
        val content = Content.of(label(sbn.packageName), title, text)
        if (content.empty) return
        hub.onPhoneNotification(Mirror.key(sbn.key), content)
    }

    override fun onNotificationRemoved(sbn: StatusBarNotification) {
        val hub = LinkHub.get(this)
        if (!hub.mirroring) return
        // Sent for every removal; the hub's outboxes act only on notifications
        // they actually sent, so another app's dismissal reaches nothing.
        hub.onPhoneNotificationRemoved(Mirror.key(sbn.key))
    }

    private fun label(pkg: String): String = synchronized(labels) {
        labels.getOrPut(pkg) {
            runCatching { packageManager.getApplicationLabel(packageManager.getApplicationInfo(pkg, 0)).toString() }
                .getOrDefault(pkg)
        }
    }

    companion object {
        fun component(context: Context) = ComponentName(context, MirrorService::class.java)

        /**
         * Whether the person granted this app notification access. Asked of
         * NotificationManager (API 27+): `NotificationManagerCompat`'s package
         * list reads a hidden Secure setting that an app targeting Android 12
         * or later cannot read, so it always said no (found on the emulator).
         */
        fun granted(context: Context): Boolean =
            context.getSystemService(NotificationManager::class.java).isNotificationListenerAccessGranted(component(context))

        /**
         * Where notification access is granted: this app's own switch where
         * Android has a page for it (11 and later), else the list of apps.
         */
        fun settingsIntent(context: Context): Intent =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                Intent(Settings.ACTION_NOTIFICATION_LISTENER_DETAIL_SETTINGS)
                    .putExtra(Settings.EXTRA_NOTIFICATION_LISTENER_COMPONENT_NAME, component(context).flattenToString())
            } else {
                Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS)
            }

        /** App info, where Android 13 and later hide "Allow restricted settings" for a sideloaded app. */
        fun appInfoIntent(context: Context): Intent =
            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, android.net.Uri.fromParts("package", context.packageName, null))
    }
}
