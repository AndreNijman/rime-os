package com.rimeos.remote.core.input

/** Where the touchpad stands, as the screen shows it. */
sealed class TouchpadPhase {
    /** The screen is not in front; nothing is open. */
    data object Away : TouchpadPhase()

    /** This computer's Rime Remote has no touchpad. */
    data object Unsupported : TouchpadPhase()

    /** This phone is not allowed; [command] is what to type at the computer. */
    data class GrantOff(val command: String) : TouchpadPhase()

    data object Connecting : TouchpadPhase()

    data object Ready : TouchpadPhase()

    /** The computer locked; it resumes by itself when unlocked. */
    data object Paused : TouchpadPhase()

    /**
     * Refused for a reason that may pass (locked, busy, Gaming Mode or no
     * desktop, stopping); tried again at [retryAt] while the screen is in
     * front.
     */
    data class Waiting(val why: Refusal, val message: String, val retryAt: Long) : TouchpadPhase()

    /** Stopped at the computer: not tried again by itself, and refused there until [until]. */
    data class Stopped(val until: Long) : TouchpadPhase()

    /** Ended by the computer for a reason trying again would not fix by itself. */
    data class Ended(val why: Refusal, val message: String) : TouchpadPhase()

    /** The connection to the computer is gone; Rime Remote is reconnecting. */
    data object LinkLost : TouchpadPhase()
}

/** What the facts about the computer say, as its `remote_hello` gave them. */
data class TouchpadFacts(
    val connected: Boolean,
    /** Null until the hello is known. */
    val supported: Boolean?,
    /** The hello's `input`: null when it did not say. */
    val inputOn: Boolean?,
    /** This phone's name as it was paired, for the command that lets it in. */
    val phoneName: String,
)

/** What the caller must do after a step: open the channel, close it, or neither. */
data class TouchpadStep(val open: Boolean = false, val close: Boolean = false)

/**
 * The touchpad's states and the rules between them: when to open the
 * channel, when to try again, when to wait for a person.
 *
 * - A refusal that may pass by itself (the computer is locked, another phone
 *   has it, Gaming Mode, Rime Remote restarting) is tried again on a timer
 *   while the screen is in front.
 * - Stopped at the computer is never tried again by itself: a person did it
 *   on purpose. The screen offers to try, and the computer says how long is
 *   left.
 * - A lost connection opens the channel again only when Rime Remote has
 *   reconnected and the screen is still in front.
 * - Not allowed shows the command that allows it, never a dead pad.
 *
 * Pure: time comes in with every call.
 */
class TouchpadFlow(initial: TouchpadFacts) {
    var phase: TouchpadPhase = TouchpadPhase.Away
        private set

    var facts: TouchpadFacts = initial
        private set

    private var inFront = false

    /** Whether a channel is open or being opened, as far as this knows. */
    var channel: Boolean = false
        private set

    /** The screen came to the front. */
    fun enter(now: Long): TouchpadStep {
        inFront = true
        val p = phase
        // A fresh visit after the cool-down is a fresh wish to use it.
        if (p is TouchpadPhase.Stopped && now >= p.until) phase = TouchpadPhase.Away
        if (p is TouchpadPhase.Ended || p is TouchpadPhase.Waiting || p is TouchpadPhase.GrantOff) {
            phase = TouchpadPhase.Away
        }
        return settle(now)
    }

    /** The screen left (another screen, the app went to the background). */
    fun leave(): TouchpadStep {
        inFront = false
        val close = channel
        channel = false
        if (phase !is TouchpadPhase.Stopped) phase = TouchpadPhase.Away
        return TouchpadStep(close = close)
    }

    /** The link's facts changed (connected, reconnected, a new hello). */
    fun facts(now: Long, f: TouchpadFacts): TouchpadStep {
        val wasOn = facts.inputOn == true
        facts = f
        if (!f.connected) {
            channel = false
            if (phase.live()) phase = TouchpadPhase.LinkLost
            return TouchpadStep()
        }
        if (!inFront) return TouchpadStep()
        val p = phase
        return when {
            p is TouchpadPhase.LinkLost -> settle(now)
            p is TouchpadPhase.Away -> settle(now)
            p is TouchpadPhase.Connecting && !channel -> settle(now)
            // A new hello that says this phone is now allowed (once, not on every repeat of it).
            p is TouchpadPhase.GrantOff && f.inputOn == true && !wasOn -> settle(now)
            else -> TouchpadStep()
        }
    }

    /** The reply to `Open`. */
    fun opened(now: Long, reply: Opened): TouchpadStep {
        if (!inFront) {
            val close = reply is Opened.Ready
            channel = false
            return TouchpadStep(close = close)
        }
        when (reply) {
            Opened.Ready -> phase = TouchpadPhase.Ready
            is Opened.Refused -> {
                channel = false
                phase = refusedPhase(now, reply.kind, reply.message, atOpen = true)
            }
        }
        return TouchpadStep()
    }

    /**
     * Opening failed without a refusal: the connection dropped or the
     * computer never answered.
     */
    fun openFailed(now: Long): TouchpadStep {
        channel = false
        if (!inFront) return TouchpadStep()
        phase = if (facts.connected) {
            TouchpadPhase.Waiting(Refusal.OTHER, "", now + RETRY_OTHER_MS)
        } else {
            TouchpadPhase.LinkLost
        }
        return TouchpadStep()
    }

    /** An event line from the computer. */
    fun notice(now: Long, n: Notice): TouchpadStep {
        if (!channel) return TouchpadStep()
        when (n) {
            is Notice.Paused -> phase = TouchpadPhase.Paused
            Notice.Resumed -> if (phase is TouchpadPhase.Paused) phase = TouchpadPhase.Ready
            is Notice.Released -> {}
            is Notice.Closed -> {
                channel = false
                phase = refusedPhase(now, n.why, n.message, atOpen = false)
            }
        }
        return TouchpadStep()
    }

    /** The channel closed without saying why first. */
    fun channelClosed(now: Long): TouchpadStep {
        if (!channel) return TouchpadStep()
        channel = false
        if (!inFront) return TouchpadStep()
        phase = if (facts.connected) {
            TouchpadPhase.Waiting(Refusal.OTHER, "", now + RETRY_OTHER_MS)
        } else {
            TouchpadPhase.LinkLost
        }
        return TouchpadStep()
    }

    /** Time passed: a refusal's retry may be due. */
    fun tick(now: Long): TouchpadStep {
        val p = phase
        if (inFront && !channel && p is TouchpadPhase.Waiting && now >= p.retryAt) return open()
        return TouchpadStep()
    }

    /** When [tick] has something to do, or null. */
    fun dueAt(): Long? = (phase as? TouchpadPhase.Waiting)?.retryAt?.takeIf { inFront && !channel }

    /** A person asked to try again ("Check again", "Connect"). */
    fun retry(now: Long): TouchpadStep {
        if (!inFront || channel) return TouchpadStep()
        if (!facts.connected) {
            phase = TouchpadPhase.LinkLost
            return TouchpadStep()
        }
        return open()
    }

    private fun settle(now: Long): TouchpadStep {
        if (!inFront || channel) return TouchpadStep()
        val f = facts
        val p = phase
        phase = when {
            p is TouchpadPhase.Stopped && now < p.until -> p
            !f.connected -> TouchpadPhase.LinkLost
            f.supported == null -> TouchpadPhase.Connecting
            f.supported == false -> TouchpadPhase.Unsupported
            f.inputOn == false -> TouchpadPhase.GrantOff(Input.grantCommand(f.phoneName))
            else -> return open()
        }
        return TouchpadStep()
    }

    private fun open(): TouchpadStep {
        phase = TouchpadPhase.Connecting
        channel = true
        return TouchpadStep(open = true)
    }

    private fun refusedPhase(now: Long, why: Refusal, message: String, atOpen: Boolean): TouchpadPhase =
        when (why) {
            Refusal.NOT_ALLOWED -> {
                val named = Input.nameInRefusal(message)
                when {
                    named != null -> TouchpadPhase.GrantOff(Input.grantCommand(named))
                    // Mid-session: the grant was lowered at the computer.
                    !atOpen || message.isEmpty() -> TouchpadPhase.GrantOff(Input.grantCommand(facts.phoneName))
                    // Something else is missing (a companion link has no touchpad).
                    else -> TouchpadPhase.Ended(why, message)
                }
            }
            Refusal.STOPPED -> TouchpadPhase.Stopped(now + (Input.secondsLeft(message)?.times(1000L) ?: Input.COOL_DOWN_MS))
            Refusal.LOCKED -> TouchpadPhase.Waiting(why, message, now + RETRY_LOCKED_MS)
            Refusal.BUSY -> TouchpadPhase.Waiting(why, message, now + RETRY_BUSY_MS)
            Refusal.UNAVAILABLE -> TouchpadPhase.Waiting(why, message, now + RETRY_UNAVAILABLE_MS)
            Refusal.STOPPING -> TouchpadPhase.Waiting(why, message, now + RETRY_OTHER_MS)
            Refusal.BAD_REQUEST, Refusal.REFUSED, Refusal.OTHER -> TouchpadPhase.Ended(why, message)
        }

    private fun TouchpadPhase.live(): Boolean =
        this is TouchpadPhase.Connecting || this is TouchpadPhase.Ready || this is TouchpadPhase.Paused ||
            this is TouchpadPhase.Waiting

    companion object {
        const val RETRY_LOCKED_MS: Long = 3_000
        const val RETRY_BUSY_MS: Long = 5_000
        const val RETRY_UNAVAILABLE_MS: Long = 10_000
        const val RETRY_OTHER_MS: Long = 4_000
    }
}
