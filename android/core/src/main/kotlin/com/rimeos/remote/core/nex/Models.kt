package com.rimeos.remote.core.nex

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/** A row of `sessions.list`. */
data class NexSession(
    val id: String,
    val title: String?,
    val state: String,
    val mode: String?,
    val provider: String?,
    val root: String?,
    val createdAt: String?,
) {
    /**
     * What a list shows: the title, else the folder's name, else the id.
     * "untitled" is nexd's stand-in for no title (`server.rs`), so it counts
     * as none: a list of three "untitled" rows says nothing.
     */
    val label: String
        get() = title?.takeIf { it.isNotBlank() && it != UNTITLED }
            ?: root?.trimEnd('/')?.substringAfterLast('/')?.takeIf { it.isNotEmpty() }
            ?: id

    val running: Boolean get() = state == "running"

    companion object {
        /** nexd's title for a session made without one. */
        const val UNTITLED = "untitled"

        fun list(result: JsonElement): List<NexSession> = result.objects().mapNotNull(::row)

        internal fun row(o: JsonObject): NexSession? {
            val id = o.str("session_id") ?: return null
            return NexSession(
                id = id,
                title = o.str("title"),
                state = o.str("state") ?: "unknown",
                mode = o.str("mode"),
                provider = o.str("provider"),
                root = o.str("root"),
                createdAt = o.str("created_at"),
            )
        }

        /**
         * The folders a phone may start a session in: those that already have
         * one. nexd refuses any other root from a paired device (`-32002`), so
         * offering one would be offering a refusal.
         */
        fun roots(sessions: List<NexSession>): List<String> =
            sessions.mapNotNull { it.root }.distinct()
    }
}

/** A tool call waiting for a person. */
data class NexApproval(
    val id: String,
    val sessionId: String?,
    val tool: String,
    val summary: String,
    val scopes: List<String>,
    val reasons: List<String>,
    val fingerprint: String?,
    val expiresAt: String?,
    /** A native runtime's own prompt (Codex, Claude Code), not a nex tool. */
    val native: Boolean,
) {
    companion object {
        /** `tools.pending`: flat rows. */
        fun pending(result: JsonElement): List<NexApproval> = result.objects().mapNotNull { o ->
            val id = o.str("approval_id") ?: return@mapNotNull null
            from(id, o.str("session_id"), o)
        }

        /** `sessions.get`'s `pending_approvals`: records, the details under `data`. */
        internal fun records(list: List<JsonObject>): List<NexApproval> = list.mapNotNull { r ->
            val id = r.str("id") ?: return@mapNotNull null
            if ((r.str("state") ?: "pending") != "pending") return@mapNotNull null
            from(id, r.str("scope"), r.obj("data") ?: JsonObject(emptyMap()))
        }

        private fun from(id: String, session: String?, o: JsonObject): NexApproval {
            val tool = o.str("tool") ?: "tool"
            return NexApproval(
                id = id,
                sessionId = session,
                tool = tool,
                summary = o.str("summary")?.takeIf { it.isNotBlank() } ?: "$tool ${o["args"].oneLine()}".trim(),
                scopes = o.strings("scopes"),
                reasons = o.strings("reasons"),
                fingerprint = o.str("fingerprint"),
                expiresAt = o.str("expires_at"),
                native = o.bool("native") ?: false,
            )
        }
    }
}

/** One session, as `sessions.get` returns it: what was said, done and decided. */
data class NexTranscript(
    val session: NexSession,
    val entries: List<Entry>,
    val pending: List<NexApproval>,
    val lastSequence: Long,
) {
    /** Is a turn still going? From the turns, not only the session's state. */
    val running: Boolean
        get() = session.running || entries.any { it is Entry.TurnEnd && it.state == "running" }

    sealed class Entry {
        abstract val turnId: String?

        /** Words: the person's, or the agent's. */
        data class Said(val role: String, val text: String, override val turnId: String?) : Entry()

        /** The agent used a tool. [state] is nexd's record state (`succeeded`, `failed`, …). */
        data class ToolUse(
            val name: String,
            val args: String,
            val state: String?,
            val exitCode: Long?,
            val error: Boolean,
            val result: String?,
            override val turnId: String?,
        ) : Entry()

        /** A turn finished, or is still running. */
        data class TurnEnd(
            val state: String,
            val text: String?,
            /** nex's own check of what the agent claimed: `verified`, `unverified`, … */
            val verification: String?,
            val verificationReason: String?,
            override val turnId: String?,
        ) : Entry()
    }

    companion object {
        fun parse(result: JsonElement): NexTranscript {
            val r = result as? JsonObject ?: JsonObject(emptyMap())
            val s = r.obj("session") ?: JsonObject(emptyMap())
            val data = s.obj("data") ?: JsonObject(emptyMap())
            val session = NexSession(
                id = s.str("id") ?: "",
                title = data.str("title"),
                state = s.str("state") ?: "unknown",
                mode = data.str("mode"),
                provider = data.str("provider"),
                root = data.str("root"),
                createdAt = s.str("created_at"),
            )
            // Tool calls by the provider's id, which is what a message's parts name.
            val calls = r.objects("tool_calls").associateBy { it.obj("data")?.str("provider_call_id") }
            val results = HashMap<String, JsonObject>()
            val messages = r.objects("messages").sortedBy { it.long("seq") ?: 0 }
            for (m in messages) {
                val msg = m.obj("message") ?: continue
                if (msg.str("role") != "tool") continue
                for (p in msg.objects("parts")) {
                    if (p.str("type") == "tool_result") p.str("id")?.let { results[it] = p }
                }
            }
            val turns = r.objects("turns").associateBy { it.str("id") }
            val entries = ArrayList<Entry>()
            val ended = HashSet<String>()
            fun endTurn(id: String?) {
                if (id == null || !ended.add(id)) return
                val t = turns[id] ?: return
                val outcome = t.obj("data")?.obj("outcome")
                val v = outcome?.obj("verification")
                entries.add(
                    Entry.TurnEnd(
                        state = t.str("state") ?: "unknown",
                        text = outcome?.str("text") ?: outcome?.str("reason"),
                        verification = v?.str("status"),
                        verificationReason = v?.str("reason"),
                        turnId = id,
                    ),
                )
            }
            var current: String? = null
            for (m in messages) {
                val turn = m.str("turn_id")
                if (current != null && turn != current) endTurn(current)
                current = turn
                val msg = m.obj("message") ?: continue
                val role = msg.str("role") ?: continue
                if (role == "tool") continue
                val text = msg.objects("parts").filter { it.str("type") == "text" }
                    .mapNotNull { it.str("text") }.joinToString("\n").trim()
                if (text.isNotEmpty()) entries.add(Entry.Said(role, text, turn))
                for (p in msg.objects("parts")) {
                    if (p.str("type") != "tool_call") continue
                    val id = p.str("id")
                    val call = calls[id]
                    val cd = call?.obj("data")
                    val res = results[id]
                    entries.add(
                        Entry.ToolUse(
                            name = p.str("name") ?: "tool",
                            args = p["args"].oneLine(),
                            state = call?.str("state"),
                            exitCode = cd?.long("exit_code"),
                            error = res?.bool("is_error") ?: cd?.obj("result")?.bool("is_error") ?: false,
                            result = res?.str("content")?.trim()?.takeIf { it.isNotEmpty() },
                            turnId = turn,
                        ),
                    )
                }
            }
            endTurn(current)
            // A turn with no message yet (just submitted) still shows as running.
            for ((id, _) in turns) endTurn(id)
            return NexTranscript(
                session = session,
                entries = entries,
                pending = NexApproval.records(r.objects("pending_approvals")),
                lastSequence = r.long("last_sequence") ?: 0,
            )
        }
    }
}

/** A row of `missions.list`. */
data class NexMission(val id: String, val objective: String, val state: String, val updatedAt: String?) {
    companion object {
        fun list(result: JsonElement): List<NexMission> = result.objects().mapNotNull { o ->
            NexMission(
                id = o.str("mission_id") ?: return@mapNotNull null,
                objective = o.str("objective") ?: "",
                state = o.str("state") ?: "unknown",
                updatedAt = o.str("updated_at"),
            )
        }

        /**
         * What a phone may do with a mission in [state], as nexd's verbs.
         *
         * Planning is not here: a plan is a spec of tasks, agents and checks,
         * and it is written on the computer.
         */
        fun actions(state: String): List<String> = when (state) {
            // nex-domain's MissionState: these are its allowed moves.
            "planned" -> listOf("missions.approve", "missions.cancel")
            "approved" -> listOf("missions.start", "missions.cancel")
            "active" -> listOf("missions.pause", "missions.cancel")
            "paused", "blocked" -> listOf("missions.resume", "missions.cancel")
            "draft", "verifying" -> listOf("missions.cancel")
            else -> emptyList()
        }

        /** Finished: nothing more will happen to it. */
        fun terminal(state: String): Boolean = state in setOf("complete", "failed", "cancelled")
    }
}

/** `missions.get`: a mission and where each task is. */
data class NexMissionDetail(
    val mission: NexMission,
    val tasks: List<Task>,
    val reason: String?,
    val verify: List<String>,
) {
    data class Task(val id: String, val state: String, val attempt: Long, val lastError: String?, val sessionId: String?)

    companion object {
        fun parse(result: JsonElement): NexMissionDetail {
            val r = result as? JsonObject ?: JsonObject(emptyMap())
            return NexMissionDetail(
                mission = NexMission(
                    id = r.str("mission_id") ?: "",
                    objective = r.str("objective") ?: "",
                    state = r.str("state") ?: "unknown",
                    updatedAt = r.str("updated_at"),
                ),
                tasks = r.objects("tasks").mapNotNull { t ->
                    Task(
                        id = t.str("task_id") ?: return@mapNotNull null,
                        state = t.str("state") ?: "unknown",
                        attempt = t.long("attempt") ?: 0,
                        lastError = t.str("last_error"),
                        sessionId = t.str("session_id"),
                    )
                },
                reason = r.str("reason"),
                verify = r.obj("spec")?.strings("verify") ?: emptyList(),
            )
        }
    }
}

/** One connection's row of `usage.list`. */
data class NexUsage(
    val connection: String,
    val tokens: List<Tokens>,
    val quota: List<Quota>,
) {
    data class Tokens(val metric: String, val total: Double, val estimate: Boolean)

    /** [usedPercent] null is "not reported", which nexd says in [label]. */
    data class Quota(val label: String, val usedPercent: Double?, val window: String?, val resetAt: String?, val stale: Boolean)

    /** Has anything been used or reported? A row of zeros and unknowns is noise on a phone. */
    val worthShowing: Boolean
        get() = tokens.any { it.total > 0 } || quota.any { it.usedPercent != null }

    companion object {
        fun list(result: JsonElement): List<NexUsage> = result.objects().mapNotNull { o ->
            NexUsage(
                connection = o.str("connection_id") ?: return@mapNotNull null,
                tokens = o.objects("tokens").mapNotNull { t ->
                    Tokens(
                        metric = t.str("metric") ?: return@mapNotNull null,
                        total = t.double("total") ?: 0.0,
                        estimate = t.str("source_kind") == "local_estimate",
                    )
                },
                quota = o.objects("quota").map { q ->
                    Quota(
                        label = q.str("label") ?: "",
                        usedPercent = q.double("used_percent"),
                        window = q.str("window"),
                        resetAt = q.str("reset_at"),
                        stale = q.bool("stale") ?: false,
                    )
                },
            )
        }
    }
}
