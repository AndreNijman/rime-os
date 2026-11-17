package com.rimeos.remote.core.agent

/**
 * What a notification says, decided where a test can read it.
 *
 * Every rule about notification CONTENT lives in this file rather than at the
 * `NotificationCompat.Builder` call, because nothing in this project can test
 * that call: there is no device, no emulator, no Robolectric, and
 * `assembleDebug` compiles the builder without running it. A privacy rule
 * enforced only inside an Android class is a privacy rule with no gate in
 * front of it.
 *
 * ## The criterion this file is the answer to, and the half it is not
 *
 * P1-058 asks that "notification content is encrypted/minimized so push
 * infrastructure does not receive sensitive prompt/code content". Since the
 * push path landed there IS push infrastructure, and the *encrypted* half of
 * the criterion is answered somewhere else entirely: [Push] seals a fixed
 * 22-byte body under a key the push server never has, so what crosses it is 51
 * bytes of which nothing is a string. Nothing in this file ever reaches a
 * server.
 *
 * What this file answers is *minimized*: the words on the lock screen. A phone
 * on a table shows its notifications to whoever is in the room, and that is a
 * disclosure channel whether or not a server was involved — and it is the one
 * the encryption does nothing about, because by then the words have been
 * rendered on the phone.
 *
 * So the rule is:
 * **an alert renders from fixed strings, a machine name and an adapter name,
 * and from nothing else.** In particular `SessionInfo.detail` never reaches
 * here. It is built by `hook::detail_for` (`hook.rs:437`) and copies verbatim,
 * clipped to 120 characters: Bash command lines, file paths, grep patterns,
 * fetched URLs, task descriptions and Claude's own notification text. It is
 * the most sensitive field on the record and it is the one a notification
 * would most naturally want. [Alert] does not carry it, this file could not
 * reach it, and `NotificationsTest` drives a session whose detail is a secret
 * through the whole watcher-to-notification path and asserts the secret is in
 * none of the rendered fields.
 *
 * Nor are `cwd`, `project`, `worktree`, `args` or `telemetry.branch` carried:
 * they name what the user is working on, which is the thing a person reading
 * over a shoulder learns the most from.
 */
data class NotificationContent(
    /**
     * The Android notification id.
     *
     * Derived from [Alert.Key] and stable for it, so that a session which
     * enters `waiting_for_user`, is answered, and enters it again REPLACES its
     * notification rather than stacking a second one. Four paths inside Rime
     * publish `waiting_for_user` for one turn (see [Alert.Key]); the watcher
     * already collapses those to one alert, and this makes the notification
     * survive the case the watcher cannot see — an app restart, which empties
     * the watcher and makes the next poll a first poll.
     */
    val id: Int,
    val title: String,
    val text: String,
    /** Which machine, for the tap. A paired device id. */
    val machine: String,
    /** Which session, for the tap. [Alert.NO_SESSION] when there is none. */
    val session: Int,
    /** Grouped per machine, so two laptops do not interleave in the shade. */
    val group: String,
    /** Whether tapping this opens the machine's nex screen. */
    val opensNex: Boolean = false,
) {
    /** Whether tapping this can open a session, or only the Agent Center. */
    val opensSession: Boolean get() = session != Alert.NO_SESSION && !opensNex

    companion object {
        /**
         * Render an alert.
         *
         * @param machineName the machine's display name, which the user chose
         *   at pairing. Not the device id: an id is unreadable, and the name
         *   is already on this phone's own screen in the machines list, so it
         *   discloses nothing the phone does not show anyway. Empty when the
         *   phone has no name for it, and then the title carries no machine.
         */
        fun of(alert: Alert, machineName: String = ""): NotificationContent {
            // nex is the agent, and its title says so; "Agent on" before the
            // machine would be a second, vaguer name for the same thing.
            val agent = if (alert.kind == Alert.Kind.NEX_APPROVAL) "" else AgentNames.of(alert.agent)
            // "Waiting for you · claude" rather than a sentence, because the
            // shade truncates hard and the first two words have to carry it.
            val who = when {
                agent.isNotEmpty() && machineName.isNotEmpty() -> "$agent on $machineName"
                agent.isNotEmpty() -> agent
                machineName.isNotEmpty() -> machineName
                else -> ""
            }
            return NotificationContent(
                id = idFor(alert.key),
                title = if (who.isEmpty()) alert.kind.title else "${alert.kind.title} · $who",
                text = alert.kind.detail,
                machine = alert.machine,
                session = alert.session,
                group = GROUP_PREFIX + alert.machine,
                opensNex = alert.kind == Alert.Kind.NEX_APPROVAL,
            )
        }

        /**
         * A stable, deterministic id for a key.
         *
         * `String.hashCode` is **specified** by the JVM — `s[0]*31^(n-1) + …` —
         * so this is the same number in this process, in the next one, and on
         * another phone. That matters: the whole point is that the
         * notification posted before an app restart is the one replaced after
         * it, and an id from an unspecified hash would post a second.
         *
         * Forced non-negative because `NotificationManager` accepts a negative
         * id but Android's own tooling and several launchers treat ids as
         * opaque positive handles; and `Int.MIN_VALUE` has no positive
         * counterpart, which is the one value `-x` does not fix.
         */
        fun idFor(key: Alert.Key): Int {
            var h = key.machine.hashCode()
            h = h * 31 + key.session
            h = h * 31 + key.kind.ordinal
            return h and Int.MAX_VALUE
        }

        /** One shade group per machine. */
        const val GROUP_PREFIX: String = "rime.machine."

        /**
         * The single channel every alert is posted on.
         *
         * One channel and not six, deliberately. Android lets the user turn a
         * channel off, and the six kinds are not six things a person wants
         * separately — they are one thing, "an agent on my laptop needs me".
         * Six channels would present six switches whose only real use is to
         * turn off the one that matters and forget.
         */
        const val CHANNEL: String = "rime.agents"
    }
}

/**
 * Where a tapped notification asks to go: a session on a machine, or that
 * machine's nex screen. A hint only; the app checks the machine is the
 * connected one before it opens anything.
 */
data class AlertTap(val machine: String, val session: Int, val nex: Boolean = false) {
    enum class GoesTo { NEX, SESSION, ELSEWHERE }

    /**
     * Where this tap may go, given the machine the phone is connected to and
     * whether that machine offers nex.
     *
     * Anything for another machine goes to the machine list or the Agent
     * Center: opening it on the connected machine would show the wrong
     * computer's work. A nex tap on a machine whose bridge no longer reaches
     * nexd has nothing to open either. A session tap may still find its
     * session gone; the caller checks that.
     */
    fun goesTo(connected: String?, nexOffered: Boolean): GoesTo = when {
        connected != machine -> GoesTo.ELSEWHERE
        nex -> if (nexOffered) GoesTo.NEX else GoesTo.ELSEWHERE
        else -> GoesTo.SESSION
    }
}
