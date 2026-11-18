package com.rimeos.remote.phone

import android.app.Notification
import android.app.NotificationManager
import android.app.RemoteInput
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Process
import android.provider.Settings
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import com.rimeos.remote.core.phone.Content
import com.rimeos.remote.core.phone.Facts
import com.rimeos.remote.core.phone.Mirror
import com.rimeos.remote.core.phone.ReplyAction
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
 *
 * When the person dismisses one of them on a computer, [dismiss] cancels it
 * here, as a swipe would; Android then reports the removal, which closes it
 * on the other computers too. When the person turned on replying, a reply
 * typed on a computer goes through [reply], into the app's own reply action,
 * as if typed into the shade here. And when the person lets Do Not Disturb
 * follow a computer, [focus] turns it on and off here (§2.9).
 */
class MirrorService : NotificationListenerService() {
    /** App labels by package, so a busy chat does not ask the package manager each time. */
    private val labels = object : LinkedHashMap<String, String>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, String>?) = size > 64
    }

    /** Android's key for each notification handed to the hub, by the key computers know it by. */
    private val sent = object : LinkedHashMap<String, String>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, String>?) = size > 256
    }

    override fun onListenerConnected() {
        live = this
        // The same grant opens media sessions (§2.8).
        LinkHub.get(this).onListenerChanged()
    }

    override fun onListenerDisconnected() {
        if (live === this) live = null
        synchronized(sent) { sent.clear() }
        LinkHub.get(this).onListenerChanged()
    }

    override fun onDestroy() {
        if (live === this) live = null
        super.onDestroy()
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
        // A reply field only when the person turned replying on.
        val reply = if (hub.mirrorReply()) Mirror.replyLabel(actionsOf(n).map { (a, _) -> a }) else null
        val content = Content.of(label(sbn.packageName), title, text, reply)
        if (content.empty) return
        val key = Mirror.key(sbn.key)
        synchronized(sent) { sent[key] = sbn.key }
        hub.onPhoneNotification(key, content)
    }

    override fun onNotificationRemoved(sbn: StatusBarNotification) {
        val key = Mirror.key(sbn.key)
        synchronized(sent) { sent.remove(key) }
        val hub = LinkHub.get(this)
        if (!hub.mirroring) return
        // Sent for every removal; the hub's outboxes act only on notifications
        // they actually sent, so another app's dismissal reaches nothing.
        hub.onPhoneNotificationRemoved(key)
    }

    private fun label(pkg: String): String = synchronized(labels) {
        labels.getOrPut(pkg) {
            runCatching { packageManager.getApplicationLabel(packageManager.getApplicationInfo(pkg, 0)).toString() }
                .getOrDefault(pkg)
        }
    }

    companion object {
        /** A notification's actions, with what the policy reads off each: its own, then those it gives a watch. */
        private fun actionsOf(n: Notification): List<Pair<ReplyAction, Notification.Action>> {
            val all = n.actions.orEmpty().toList() + runCatching { Notification.WearableExtender(n).actions }.getOrDefault(emptyList())
            return all.map { a -> ReplyAction(a.title, a.remoteInputs.orEmpty().any { it.allowFreeFormInput }) to a }
        }

        /** The listener Android has bound, if any. */
        @Volatile private var live: MirrorService? = null

        fun component(context: Context) = ComponentName(context, MirrorService::class.java)

        /**
         * Cancel the notification computers know as [key], if this listener
         * handed it to them and it is still up. False when there is nothing
         * to cancel (unbound, never sent, already gone).
         */
        fun dismiss(key: String): Boolean {
            val service = live ?: return false
            val androidKey = synchronized(service.sent) { service.sent[key] } ?: return false
            return runCatching { service.cancelNotification(androidKey) }.isSuccess
        }

        /**
         * Send [text] as the reply to the notification computers know as
         * [key], through the first of its actions that takes free-form text,
         * as the shade does: the results on a fill-in intent, marked typed.
         * False when there is nothing to answer (unbound, never sent, gone,
         * or no such action any more).
         */
        fun reply(key: String, text: String): Boolean {
            val service = live ?: return false
            val androidKey = synchronized(service.sent) { service.sent[key] } ?: return false
            val sbn = runCatching { service.getActiveNotifications(arrayOf(androidKey)) }.getOrNull()?.firstOrNull()
                ?: return false
            val action = actionsOf(sbn.notification).firstOrNull { (a, _) -> a.freeForm }?.second ?: return false
            val inputs = action.remoteInputs ?: return false
            val input = inputs.first { it.allowFreeFormInput }
            val intent = Intent()
            RemoteInput.addResultsToIntent(inputs, intent, Bundle().apply { putCharSequence(input.resultKey, text) })
            RemoteInput.setResultsSource(intent, RemoteInput.SOURCE_FREE_FORM_INPUT)
            return runCatching { action.actionIntent.send(service, 0, intent) }.isSuccess
        }

        /**
         * Turn this phone's Do Not Disturb on ([dnd]: priority only) or off
         * (everything), as a paired computer asked (§2.9). The listener's
         * own request: notification access is enough, and no Do Not Disturb
         * access is asked for. False when no listener is bound.
         */
        fun focus(dnd: Boolean): Boolean {
            val service = live ?: return false
            val filter = if (dnd) INTERRUPTION_FILTER_PRIORITY else INTERRUPTION_FILTER_ALL
            return runCatching { service.requestInterruptionFilter(filter) }.isSuccess
        }

        /**
         * Whether the person granted this app notification access. Asked of
         * NotificationManager (API 27+), about this exact component. MEASURED
         * on the Android 16 emulator: with access granted (and the listener
         * bound), `NotificationManagerCompat.getEnabledListenerPackages` did
         * not list this app, so the screen said access was off.
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
