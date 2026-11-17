package com.rimeos.remote.ui.nex

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.nex.NexApproval
import com.rimeos.remote.core.nex.NexConnection
import com.rimeos.remote.core.nex.NexEvent
import com.rimeos.remote.core.nex.NexMission
import com.rimeos.remote.core.nex.NexMissionDetail
import com.rimeos.remote.core.nex.NexRefused
import com.rimeos.remote.core.nex.NexSession
import com.rimeos.remote.core.nex.NexTranscript
import com.rimeos.remote.core.nex.NexUsage

/**
 * What the nex screens show, for one computer.
 *
 * A value, and every rule that turns nexd's answers and events into it lives
 * here as a plain function, so the JVM tests can drive it without a phone.
 * `NexViewModel` only moves bytes and calls these.
 */
data class NexUiState(
    /** The paired machine's device id, or null before a machine is opened. */
    val machine: String? = null,
    /**
     * Why nex cannot be reached on this computer right now, in the bridge's
     * words ("nex is not running on this computer"), or null when it can.
     * Distinct from [failure]: this replaces the screen, a failure is a strip.
     */
    val away: String? = null,
    val loading: Boolean = false,
    /** True once one full load has landed, so "nothing here" is not shown before it. */
    val loaded: Boolean = false,
    val failure: String? = null,
    /** Something that worked and is worth saying: "Approved", "Sent". */
    val notice: String? = null,
    val sessions: List<NexSession> = emptyList(),
    val pending: List<NexApproval> = emptyList(),
    val missions: List<NexMission> = emptyList(),
    /** Missions the user opened, with their tasks. */
    val missionDetails: Map<String, NexMissionDetail> = emptyMap(),
    val usage: List<NexUsage> = emptyList(),
    val connections: List<NexConnection> = emptyList(),
    /**
     * Methods this nexd answered "not built" (-32601). Their section is left
     * out rather than shown empty: an empty mission list says "no missions",
     * which is a different claim from "this nexd has no missions".
     */
    val unbuilt: Set<String> = emptySet(),
    /** Whether the live feed is open. When it is not, the lists are polled. */
    val live: Boolean = false,
    /** The session whose transcript is open. */
    val selected: String? = null,
    val transcript: NexTranscript? = null,
    /** The reply being written right now in [selected], from `message.delta`. */
    val typing: String = "",
    /** Approvals with an answer on the way, so their buttons are not pressed twice. */
    val deciding: Set<String> = emptySet(),
    /** A message, a stop or a new session on the way. */
    val sending: Boolean = false,
    /** A session this phone just made, for the screen to open once. */
    val created: String? = null,
) {
    fun shows(method: String): Boolean = method !in unbuilt

    fun waitingOnYou(session: String): Boolean = pending.any { it.sessionId == session }

    /** The approvals for [session]: from its transcript when open, else the list. */
    fun pendingFor(session: String): List<NexApproval> {
        val t = transcript?.takeIf { it.session.id == session }
        val fromList = pending.filter { it.sessionId == session }
        return (fromList + (t?.pending ?: emptyList())).distinctBy { it.id }
    }

    val roots: List<String> get() = NexSession.roots(sessions)

    /** Sessions with someone's attention needed first, then running, then the rest. */
    val ordered: List<NexSession>
        get() = sessions.sortedWith(
            compareByDescending<NexSession> { waitingOnYou(it.id) }
                .thenByDescending { it.running }
                .thenByDescending { it.createdAt ?: "" },
        )

    fun dismissed(): NexUiState = copy(failure = null, notice = null)

    /**
     * What a batch of events changes: the live reply text now, and which
     * reads are now out of date.
     */
    fun withEvents(events: List<NexEvent>): Pair<NexUiState, Reload> {
        var typing = typing
        var lists = false
        var transcript = false
        for (e in events) {
            if (e.changesLists) lists = true
            val mine = selected != null && e.stream == selected
            if (!mine) continue
            when (e.kind) {
                "message.delta" -> e.deltaText?.let { typing += it }
                // The words are in the transcript now; the live copy goes.
                "message.completed" -> {
                    typing = ""
                    transcript = true
                }
                else -> if (e.kind.startsWith("tool.") || e.kind.startsWith("turn.")) {
                    transcript = true
                    if (e.kind.startsWith("turn.")) typing = ""
                }
            }
        }
        // A reply that never ends is not drawn forever: past the cap, the
        // tail is what is being written now.
        if (typing.length > TYPING_CAP) typing = typing.takeLast(TYPING_CAP)
        return copy(typing = typing) to Reload(lists, transcript)
    }

    data class Reload(val lists: Boolean, val transcript: Boolean) {
        val any: Boolean get() = lists || transcript
    }

    companion object {
        const val TYPING_CAP = 8_000

        /** The read methods behind each section, for [unbuilt]. */
        const val MISSIONS = "missions.list"
        const val USAGE = "usage.list"
        const val CONNECTIONS = "connections.list"
    }
}

/** What went wrong with a nex request, in the place the screen shows it. */
sealed interface NexTrouble {
    /** nex is not there: the bridge has no nexd, or nexd is down. Replaces the screen. */
    data class Away(val why: String) : NexTrouble

    /** nexd's own refusal, in its words. A strip. */
    data class Refused(val why: String) : NexTrouble

    /** The method does not exist in this nexd. Its section goes. */
    object NotBuilt : NexTrouble

    /**
     * The connection dropped. A write may or may not have arrived, and saying
     * so is the honest answer; its key means sending it again is safe.
     */
    data class Dropped(val why: String) : NexTrouble

    companion object {
        fun of(e: Throwable): NexTrouble = when {
            e is NexRefused && e.notBuilt -> NotBuilt
            e is NexRefused -> Refused(e.message)
            e is AgentError && e.kind == "unavailable" -> Away(e.message)
            e is AgentError -> Refused(e.message)
            e is Disconnected -> Dropped(e.message ?: "the connection dropped")
            else -> Refused(e.message ?: e::class.java.simpleName)
        }
    }
}
