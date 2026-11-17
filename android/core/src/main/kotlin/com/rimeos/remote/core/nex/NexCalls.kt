package com.rimeos.remote.core.nex

import com.rimeos.remote.core.agent.AgentStates
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * Every nexd request this app makes, in one place.
 *
 * The parameter names are nexd's (`crates/nexd/src/server.rs`); the tests
 * check each against a request the recorder sent to a real nexd.
 */
object NexCalls {
    private fun p(vararg pairs: Pair<String, String?>): JsonObject =
        JsonObject(pairs.filter { it.second != null }.associate { it.first to JsonPrimitive(it.second) })

    fun sessions(): NexCall = NexCall.read("sessions.list")

    /** Every approval waiting, across sessions. */
    fun pending(): NexCall = NexCall.read("tools.pending")

    fun missions(): NexCall = NexCall.read("missions.list")

    fun mission(id: String): NexCall = NexCall.read("missions.get", p("mission_id" to id))

    fun usage(): NexCall = NexCall.read("usage.list")

    fun connections(): NexCall = NexCall.read("connections.list")

    fun transcript(session: String): NexCall = NexCall.read("sessions.get", p("session_id" to session))

    /**
     * The fingerprint goes with the approval: nexd refuses it if the action
     * changed since the phone was shown it, so a yes is a yes to what was read.
     */
    fun approve(a: NexApproval): NexCall =
        NexCall.write("tools.approve", p("approval_id" to a.id, "fingerprint" to a.fingerprint))

    fun deny(a: NexApproval): NexCall = NexCall.write("tools.deny", p("approval_id" to a.id))

    fun submit(session: String, text: String): NexCall {
        require(text.isNotBlank()) { "nothing to send" }
        return NexCall.write("turns.submit", p("session_id" to session, "text" to text))
    }

    /** Stop the running turn. The session stays. */
    fun cancel(session: String): NexCall = NexCall.write("turns.cancel", p("session_id" to session))

    /**
     * A new session, in a folder that already has one (nexd refuses any other
     * from a phone). [provider] is required unless nexd has a default.
     */
    fun create(root: String, provider: String?, title: String?): NexCall = NexCall.write(
        "sessions.create",
        p("root" to root, "mode" to "build", "provider" to provider, "title" to title?.takeIf { it.isNotBlank() }),
    )

    /** One of [NexMission.actions] for a mission. Anything else is refused here. */
    fun mission(verb: String, id: String): NexCall {
        require(verb in MISSION_VERBS) { "$verb is not a mission action" }
        return NexCall.write(verb, p("mission_id" to id))
    }

    private val MISSION_VERBS = setOf(
        "missions.approve", "missions.start", "missions.pause", "missions.resume", "missions.cancel",
    )
}

/** A provider nex can run a session with, from `connections.list`. */
data class NexConnection(val id: String, val kind: String, val available: Boolean, val native: Boolean) {
    companion object {
        fun list(result: JsonElement): List<NexConnection> = result.objects().mapNotNull { o ->
            val kind = o.str("kind") ?: ""
            NexConnection(
                id = o.str("id") ?: return@mapNotNull null,
                kind = kind,
                available = o.bool("available") ?: false,
                native = kind == "native",
            )
        }

        /**
         * The provider to offer first for a new session in [root]: the one its
         * latest session used, if still available, else the first available.
         */
        fun suggest(root: String, sessions: List<NexSession>, connections: List<NexConnection>): String? {
            val usable = connections.filter { it.available }.map { it.id }
            val recent = sessions.filter { it.root == root }.sortedByDescending { it.createdAt ?: "" }
                .mapNotNull { it.provider }.firstOrNull { it in usable }
            return recent ?: usable.firstOrNull()
        }
    }
}

/**
 * nex's states, in the app's agent vocabulary, so a nex session wears the
 * same badge and colour as an agent in the same situation.
 */
object NexStates {
    fun agentState(state: String, waitingOnYou: Boolean = false): String = when {
        waitingOnYou || state == "awaiting_approval" || state == "approval_pending" -> AgentStates.PERMISSION_REQUEST
        state in setOf("running", "active", "queued", "verifying", "starting") -> AgentStates.WORKING
        state in setOf("planned", "approved", "draft", "paused", "blocked", "suspended") -> AgentStates.WAITING_FOR_USER
        state in setOf("idle", "open", "completed", "complete", "integrated", "succeeded", "closed") -> AgentStates.COMPLETE
        state in setOf("failed", "timed_out", "denied", "expired") -> AgentStates.FAILED
        state in setOf("cancelled", "interrupted") -> AgentStates.EXITED
        else -> state
    }

    /** A state in words a person uses. */
    fun words(state: String): String = when (state) {
        "idle" -> "ready"
        "awaiting_approval", "approval_pending" -> "waiting for you"
        "timed_out" -> "timed out"
        else -> state.replace('_', ' ')
    }
}
