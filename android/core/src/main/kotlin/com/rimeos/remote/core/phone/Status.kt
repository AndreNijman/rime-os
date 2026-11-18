package com.rimeos.remote.core.phone

/**
 * How this phone is connected, as a `phone_report` carries it
 * (`docs/remote-live-contract.md` §2.9): the kind of network, and its bars
 * when the phone knows them.
 *
 * The machine refuses a whole report over one bad field, battery and all, so
 * nothing outside the contract's words and range can be built here.
 */
data class NetworkState(val type: String, val signal: Int? = null) {
    init {
        require(type in TYPES) { "a network type is one of $TYPES, not $type" }
        require(signal == null || signal in 0..MAX_SIGNAL) { "a signal is 0 to $MAX_SIGNAL bars, not $signal" }
    }

    companion object {
        const val WIFI: String = "wifi"
        const val CELLULAR: String = "cellular"
        const val ETHERNET: String = "ethernet"
        const val NONE: String = "none"
        const val OTHER: String = "other"
        val TYPES: Set<String> = setOf(WIFI, CELLULAR, ETHERNET, NONE, OTHER)

        /** The most bars: Android's own 0 to 4 scale. */
        const val MAX_SIGNAL: Int = 4

        /** What Android says when a network carries no signal strength. */
        const val UNSPECIFIED: Int = Int.MIN_VALUE

        /** What Android's Wi-Fi code writes for "no reading" (`WifiInfo.INVALID_RSSI`). */
        private const val INVALID_RSSI = -127

        // The ends of Android's own five-level Wi-Fi scale (the static
        // `WifiManager.calculateSignalLevel`). The instance method's top level
        // is whatever the device's config says, which is not the contract's 4.
        private const val WIFI_MIN_RSSI = -100
        private const val WIFI_MAX_RSSI = -55

        /**
         * The default network, from what its `NetworkCapabilities` say.
         * [connected] false is no default network at all. A VPN reports the
         * transports of the network under it, so the first match wins and a
         * bare VPN is `other`. [wifiRssi] is the Wi-Fi signal in dBm and
         * [cellLevel] the cellular bars, each null or [UNSPECIFIED] when
         * Android did not say; a value out of range is left out, never sent.
         */
        fun of(
            connected: Boolean,
            wifi: Boolean,
            cellular: Boolean,
            ethernet: Boolean,
            wifiRssi: Int?,
            cellLevel: Int?,
        ): NetworkState = when {
            !connected -> NetworkState(NONE)
            wifi -> NetworkState(WIFI, wifiRssi?.let(::wifiBars))
            cellular -> NetworkState(CELLULAR, cellLevel?.takeIf { it in 0..MAX_SIGNAL })
            ethernet -> NetworkState(ETHERNET)
            else -> NetworkState(OTHER)
        }

        /** Wi-Fi bars from a signal in dBm, or null when there is no reading. */
        fun wifiBars(rssi: Int): Int? = when {
            rssi <= INVALID_RSSI || rssi >= 0 -> null
            rssi <= WIFI_MIN_RSSI -> 0
            rssi >= WIFI_MAX_RSSI -> MAX_SIGNAL
            else -> (rssi - WIFI_MIN_RSSI) * MAX_SIGNAL / (WIFI_MAX_RSSI - WIFI_MIN_RSSI)
        }
    }
}

/**
 * Do Not Disturb, both ways (`docs/remote-live-contract.md` §2.9): what the
 * phone reports, and when a `focus` line from a computer is acted on.
 */
object Focus {
    // NotificationManager's interruption filters, here so core needs no Android.
    const val FILTER_UNKNOWN: Int = 0
    const val FILTER_ALL: Int = 1

    /**
     * The report's `dnd` for an interruption filter: on for anything but
     * "all", and unknown (left out) while Android says it does not know.
     */
    fun dnd(filter: Int): Boolean? = if (filter == FILTER_UNKNOWN) null else filter != FILTER_ALL

    /**
     * Whether a `focus` line asking for [wanted] is applied: only with the
     * person's switch on and the app unlocked, and only when the phone is not
     * already there. State, not a toggle, so a line that repeats the phone's
     * own state changes nothing and nothing loops.
     */
    fun apply(switchOn: Boolean, unlocked: Boolean, current: Boolean?, wanted: Boolean): Boolean =
        switchOn && unlocked && current != wanted
}

/**
 * Whether a Do Not Disturb this app turned on for a computer is still on, so
 * that turning the switch off or locking the app can take it back and never
 * leave it stranded (`docs/remote-live-contract.md` §2.9). Only one this app
 * turned on: a Do Not Disturb the person turned on at the phone is theirs,
 * and nothing here ever ends it.
 *
 * Kept in memory only: a process Android ends forgets it, and the person
 * then ends that Do Not Disturb at the phone.
 */
class OwnDnd {
    private var on = false

    /** A `focus` line was applied: [dnd] true turned it on, false off. */
    @Synchronized
    fun applied(dnd: Boolean) {
        on = dnd
    }

    /**
     * The phone's Do Not Disturb as Android now says it is. Off, by any path
     * (the person, the Modes tile, a computer), means nothing is left to take
     * back; on, or unknown, changes nothing.
     */
    @Synchronized
    fun observed(dnd: Boolean?) {
        if (dnd == false) on = false
    }

    /** Whether there is one to take back, given the phone's state now. */
    @Synchronized
    fun owed(current: Boolean?): Boolean {
        observed(current)
        return on
    }
}
