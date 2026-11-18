package com.rimeos.remote.phone

import android.app.NotificationManager
import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.telephony.TelephonyManager
import com.rimeos.remote.core.phone.Focus
import com.rimeos.remote.core.phone.NetworkState

/**
 * How this phone is connected and whether its Do Not Disturb is on, for the
 * report (`docs/remote-live-contract.md` §2.9). Read when a report is about
 * to go, so it is always the state of that moment.
 *
 * No permission is asked for either. The kind of network and the Wi-Fi
 * signal come from the default network's `NetworkCapabilities`
 * (`ACCESS_NETWORK_STATE`, which the app already holds; the signal is not one
 * of the fields Android hides without location access). The cellular bars
 * come from `TelephonyManager.getSignalStrength`, which needs no permission;
 * if a phone refuses it anyway, the bars are left out. Do Not Disturb is
 * `NotificationManager.getCurrentInterruptionFilter`, which anyone may read.
 */
object PhoneStatus {
    /** The default network now, or null when Android would not say. */
    fun network(context: Context): NetworkState? = runCatching {
        val manager = context.getSystemService(ConnectivityManager::class.java) ?: return null
        val caps = manager.activeNetwork?.let { manager.getNetworkCapabilities(it) }
        val cellular = caps?.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) == true
        NetworkState.of(
            connected = caps != null,
            wifi = caps?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true,
            cellular = cellular,
            ethernet = caps?.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) == true,
            wifiRssi = caps?.signalStrength,
            cellLevel = if (cellular) cellLevel(context) else null,
        )
    }.getOrNull()

    /** The cellular bars, 0 to 4, or null when the modem has not said or the phone refuses. */
    fun cellLevel(context: Context): Int? = runCatching {
        context.getSystemService(TelephonyManager::class.java)?.signalStrength?.level
    }.getOrNull()

    /** Whether Do Not Disturb is on, or null when Android does not know. */
    fun dnd(context: Context): Boolean? = runCatching {
        context.getSystemService(NotificationManager::class.java)?.currentInterruptionFilter?.let(Focus::dnd)
    }.getOrNull()
}
