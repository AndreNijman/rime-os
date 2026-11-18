package com.rimeos.remote.link

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import com.rimeos.remote.MainActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * Keeps the app's connections alive while it is unlocked: "I want it just
 * always connected."
 *
 * ## What it does, and what it does not
 *
 * It OWNS nothing. The links and the keys are [LinkHub]'s; this service is
 * what tells Android the process is doing something the person asked for, so
 * that backing out to the launcher or turning the screen off does not end it
 * a minute later. While it runs it also listens for the phone changing
 * networks, which is when a connection is most likely to have died quietly,
 * and asks the hub to check.
 *
 * It does not survive the app being locked — [LinkHub.lock] stops it — and it
 * is `START_NOT_STICKY`, so Android does not restart it after killing the
 * process: the keys died with the process, and a restarted service would be a
 * notification saying "connected" over nothing. Screen-off Doze can still
 * suspend the network; what this buys is that coming back is a reconnect with
 * no prompt, not a fresh unlock.
 *
 * ## `specialUse`
 *
 * None of Android's named foreground-service types describes "hold an
 * encrypted session to the person's own computer so their agents are one tap
 * away": it is not `dataSync` (nothing is being transferred), not
 * `connectedDevice` (that type is for Bluetooth, USB and the like, and needs
 * their permissions), not `remoteMessaging`. `specialUse` is the type for
 * exactly that, with the reason stated in the manifest's
 * `PROPERTY_SPECIAL_USE_FGS_SUBTYPE`. The app is sideloaded, so there is no
 * store review of the declaration — which is a reason to write it honestly,
 * not a reason to skip it.
 *
 * ## The notification says almost nothing, on purpose
 *
 * A fixed sentence and the machine's name — the same rule every notification
 * in this app follows (`NotificationContent`): no session names, no agent
 * titles, nothing an agent printed. It is low importance and silent: it is
 * the price of the service, not news.
 */
class LinkService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var network: ConnectivityManager.NetworkCallback? = null
    private var battery: android.content.BroadcastReceiver? = null
    private var focus: android.content.BroadcastReceiver? = null
    /** The `TelephonyCallback` while one is registered: `Any`, because the type is Android 12's. */
    private var signal: Any? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        ensureChannel(this)
        goForeground(emptyList(), connected = false)
        val hub = LinkHub.get(this)
        scope.launch {
            hub.states.collect { states ->
                val held = states.values.toList()
                val up = held.filter { it.status == LinkHub.Status.CONNECTED }
                val names = (up.ifEmpty { held }).map { it.machine }.distinct()
                notify(names, connected = up.isNotEmpty())
            }
        }
        watchNetwork(hub)
        watchBattery(hub)
        watchFocus(hub)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val hub = LinkHub.get(this)
        if (intent?.action == ACTION_LOCK) {
            // The notification's "Lock": the same lock as the app's button.
            // It stops this service on its way out.
            hub.lock()
            return START_NOT_STICKY
        }
        if (!hub.unlocked.value) {
            // Started with nothing to hold — a process Android recreated, say.
            // There are no keys, so there is nothing to keep alive.
            stopSelf()
            return START_NOT_STICKY
        }
        goForeground(hub.heldNames(), connected = false)
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        network?.let { cb ->
            runCatching { getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(cb) }
        }
        network = null
        battery?.let { runCatching { unregisterReceiver(it) } }
        battery = null
        focus?.let { runCatching { unregisterReceiver(it) } }
        focus = null
        watchSignal(LinkHub.get(this), false)
        scope.cancel()
        super.onDestroy()
    }

    /**
     * A new default network — Wi-Fi to mobile, mobile to Wi-Fi, back from no
     * signal — is the moment a held connection has most likely died without
     * a word. The hub probes each link and reconnects the ones that did not
     * answer, at once rather than after the forty-second watchdog.
     *
     * The probe is on `onAvailable` only. `onCapabilitiesChanged` fires on
     * every change of signal strength, and probing on each would be a ping a
     * second in a moving car; it, and `onLost`, only tell the hub that what a
     * report says may have changed (§2.9), which its throttle weighs. The
     * callback runs on a system thread, and the hub does its socket work on
     * its own IO dispatcher, never here.
     */
    private fun watchNetwork(hub: LinkHub) {
        val manager = getSystemService(ConnectivityManager::class.java) ?: return
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                hub.onNetworkChanged()
            }

            override fun onCapabilitiesChanged(network: Network, caps: android.net.NetworkCapabilities) {
                watchSignal(hub, caps.hasTransport(android.net.NetworkCapabilities.TRANSPORT_CELLULAR))
                hub.onPhoneChanged()
            }

            override fun onLost(network: Network) {
                watchSignal(hub, false)
                hub.onPhoneChanged()
            }
        }
        runCatching { manager.registerDefaultNetworkCallback(callback) }
            .onSuccess { network = callback }
    }

    /**
     * The phone's battery, for the machines it is connected to (contract
     * §2.5). Android sends this broadcast often while charging; the hub only
     * passes on a change worth reporting ([com.rimeos.remote.core.phone.ReportThrottle]).
     * Registered here, not in the manifest: it cannot be declared there, and
     * there is nobody to tell while no connection is held.
     */
    private fun watchBattery(hub: LinkHub) {
        val receiver = object : android.content.BroadcastReceiver() {
            override fun onReceive(context: android.content.Context, intent: Intent) {
                hub.onPhoneChanged()
            }
        }
        runCatching { registerReceiver(receiver, android.content.IntentFilter(Intent.ACTION_BATTERY_CHANGED)) }
            .onSuccess { battery = receiver }
    }

    /**
     * Do Not Disturb turning on or off here, from the shade, a schedule or a
     * computer's `focus` line, for the machines' reports (§2.9). The
     * broadcast and not the listener's own callback, because `dnd` is
     * reported without notification access too. A system broadcast, sent
     * only to receivers registered at run time, as this one is.
     */
    private fun watchFocus(hub: LinkHub) {
        val receiver = object : android.content.BroadcastReceiver() {
            override fun onReceive(context: android.content.Context, intent: Intent) {
                hub.onPhoneChanged()
            }
        }
        runCatching { registerReceiver(receiver, android.content.IntentFilter(NotificationManager.ACTION_INTERRUPTION_FILTER_CHANGED)) }
            .onSuccess { focus = receiver }
    }

    /**
     * The cellular bars, heard while the default network is cellular and only
     * then: the modem reports every step, and on Wi-Fi none of them is in a
     * report. Android 12 and later; before that the bars are read whenever
     * something else sends a report. `SignalStrengthsListener` needs no
     * permission; a phone that refuses it anyway simply sends no bars.
     */
    private fun watchSignal(hub: LinkHub, on: Boolean) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return
        val manager = getSystemService(android.telephony.TelephonyManager::class.java) ?: return
        synchronized(this) {
            if (on && signal == null) {
                val callback = object : android.telephony.TelephonyCallback(), android.telephony.TelephonyCallback.SignalStrengthsListener {
                    override fun onSignalStrengthsChanged(strength: android.telephony.SignalStrength) {
                        hub.onPhoneChanged()
                    }
                }
                runCatching { manager.registerTelephonyCallback(mainExecutor, callback) }.onSuccess { signal = callback }
            } else if (!on) {
                (signal as? android.telephony.TelephonyCallback)?.let { runCatching { manager.unregisterTelephonyCallback(it) } }
                signal = null
            }
        }
    }

    private fun goForeground(names: List<String>, connected: Boolean) {
        val notification = build(this, names, connected)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            ServiceCompat.startForeground(
                this,
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
            )
        } else {
            // Before API 34 there is no `specialUse` to name, and the
            // two-argument form takes the manifest's declaration as it stands.
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    private fun notify(names: List<String>, connected: Boolean) {
        runCatching {
            getSystemService(NotificationManager::class.java)
                ?.notify(NOTIFICATION_ID, build(this, names, connected))
        }
    }

    companion object {
        const val CHANNEL: String = "rime.connection"
        const val ACTION_LOCK: String = "com.rimeos.remote.LOCK"
        private const val NOTIFICATION_ID: Int = 0x52494d45 // "RIME"

        fun start(context: Context) {
            runCatching {
                ContextCompat.startForegroundService(context, Intent(context, LinkService::class.java))
            }
        }

        fun stop(context: Context) {
            runCatching { context.stopService(Intent(context, LinkService::class.java)) }
        }

        /**
         * The words, from fixed strings and machine names only.
         *
         * Separate so the rule is one function: nothing about a session can
         * reach it because nothing about a session is passed in.
         */
        fun text(names: List<String>, connected: Boolean): String = when {
            names.isEmpty() -> "Keeping your computers connected"
            connected -> "Connected to ${names.joinToString(", ")}"
            else -> "Connecting to ${names.joinToString(", ")}…"
        }

        private fun ensureChannel(context: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
            val channel = NotificationChannel(
                CHANNEL,
                "Connection",
                // LOW: no sound, no heads-up, and still visible where a person
                // looks for why something is running.
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = "Shown while Rime Remote keeps your computers connected."
                setShowBadge(false)
            }
            context.getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
        }

        private fun build(context: Context, names: List<String>, connected: Boolean): Notification {
            val open = PendingIntent.getActivity(
                context,
                0,
                Intent(context, MainActivity::class.java).apply {
                    flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
                },
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            val lock = PendingIntent.getService(
                context,
                1,
                Intent(context, LinkService::class.java).setAction(ACTION_LOCK),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            return NotificationCompat.Builder(context, CHANNEL)
                .setSmallIcon(android.R.drawable.stat_notify_sync)
                .setContentTitle("Rime Remote")
                .setContentText(text(names, connected))
                .setOngoing(true)
                .setSilent(true)
                .setOnlyAlertOnce(true)
                .setShowWhen(false)
                .setCategory(NotificationCompat.CATEGORY_SERVICE)
                .setPriority(NotificationCompat.PRIORITY_LOW)
                // Machine names are already on this phone's own machines
                // list, and nothing else is in the text.
                .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
                .setContentIntent(open)
                .addAction(0, "Lock", lock)
                .build()
        }
    }
}
