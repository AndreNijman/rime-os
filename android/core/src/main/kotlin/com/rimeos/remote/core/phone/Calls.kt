package com.rimeos.remote.core.phone

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * This phone's calls on a paired computer (`docs/remote-live-contract.md`
 * §2.12): when one rings, is answered and ends; answering, declining, ending
 * and placing one from the computer; and the call history.
 *
 * There is no audio. The call stays on the phone; the computer only sees it
 * and presses the phone's buttons for the person.
 *
 * The phone says what its call is doing with `call_state` lines on the
 * control channel. The computer asks with `call_action` and
 * `call_history_request` events on the phone's events channel; this phone
 * answers with `call_done` and `call_history_reply` lines. Nothing is kept:
 * the history is read from the phone's own call log each time it is asked.
 *
 * Everything here is plain Kotlin, so the rules are tested on the JVM; the
 * app reads the call log and calls Android's telephony service.
 */
object Calls {
    /** The feature a machine's `remote_hello` lists when it understands this phone's calls. */
    const val FEATURE: String = "phone_calls"

    /** Most calls one history request asks for. The computer caps it the same. */
    const val MAX_LIMIT: Int = 50

    const val NAME_CAP: Int = 60
    const val NUMBER_CAP: Int = 40

    /** Largest answer line, in bytes, as for [Messages.CHUNK_BYTES]. */
    const val CHUNK_BYTES: Int = 24 * 1024

    /** Rows of the call log read for one answer. Bounds the work on a huge log. */
    const val SCAN_CAP: Int = 5_000

    // What the computer may ask (§2.12).
    const val ANSWER: String = "answer"
    const val DECLINE: String = "decline"
    const val HANGUP: String = "hangup"
    const val DIAL: String = "dial"
    val ACTIONS: Set<String> = setOf(ANSWER, DECLINE, HANGUP, DIAL)

    // What the phone says its call is doing.
    const val RINGING: String = "ringing"
    const val ACTIVE: String = "active"
    const val IDLE: String = "idle"

    // Why this phone did not do it, as the computer knows them (§2.12).
    const val OFF: String = "off"
    const val NO_PERMISSION: String = "no_permission"
    const val LOCKED: String = "locked"
    const val BUSY: String = "busy"
    const val NO_CALL: String = "no_call"
    const val EMERGENCY: String = "emergency"
    const val FAILED: String = "failed"

    // Android's TelephonyManager.CALL_STATE_* and CallLog.Calls.TYPE values,
    // as numbers, so this file needs no Android.
    const val CALL_STATE_IDLE: Int = 0
    const val CALL_STATE_RINGING: Int = 1
    const val CALL_STATE_OFFHOOK: Int = 2
    const val TYPE_INCOMING: Int = 1
    const val TYPE_OUTGOING: Int = 2
    const val TYPE_MISSED: Int = 3
    const val TYPE_VOICEMAIL: Int = 4
    const val TYPE_REJECTED: Int = 5
    const val TYPE_BLOCKED: Int = 6
    const val TYPE_ANSWERED_EXTERNALLY: Int = 7

    /** What the person allowed, and what Android granted, at one moment. */
    data class Grants(
        /** "Calls on your computer". */
        val switchOn: Boolean,
        /** READ_PHONE_STATE: the phone can see that a call rings at all. */
        val state: Boolean,
        /** READ_CALL_LOG: the history, and the number of a ringing call. */
        val history: Boolean,
        /** ANSWER_PHONE_CALLS: answer, decline and hang up. */
        val answer: Boolean,
        /** CALL_PHONE: place a call. */
        val dial: Boolean,
    )

    /**
     * The words a report's `can` carries for calls: none unless the switch is
     * on and the phone can see its calls, then one per thing it may do. A
     * companion link (the app locked, or the phone restarted) is never told
     * it may dial: placing a call is the phone speaking for the person.
     */
    fun can(g: Grants, companion: Boolean): List<String> = buildList {
        if (!g.switchOn || !g.state) return@buildList
        add(Phone.CAN_CALLS)
        if (g.history) add(Phone.CAN_CALLS_HISTORY)
        if (g.answer) add(Phone.CAN_CALLS_ANSWER)
        if (g.dial && !companion) add(Phone.CAN_CALLS_DIAL)
    }

    /**
     * Why this phone will not do [action] now, or null when it will. [state]
     * is the phone's call state ([RINGING], [ACTIVE] or [IDLE]). [unlocked] is
     * an unlocked app on a full link: dialling needs it; answering, declining
     * and hanging up do not, because the phone's own lock screen allows them
     * too. The switch is checked whatever an older report told the machine.
     */
    fun refusal(g: Grants, unlocked: Boolean, action: String, state: String): String? = when {
        !g.switchOn -> OFF
        !g.state -> NO_PERMISSION
        action !in ACTIONS -> FAILED
        action == DIAL && !g.dial -> NO_PERMISSION
        action != DIAL && !g.answer -> NO_PERMISSION
        action == DIAL && !unlocked -> LOCKED
        action == DIAL && state != IDLE -> BUSY
        action == ANSWER && state != RINGING -> NO_CALL
        action == DECLINE && state != RINGING -> NO_CALL
        action == HANGUP && state == IDLE -> NO_CALL
        else -> null
    }

    /** Why this phone will not read its history now, or null when it will. */
    fun historyRefusal(g: Grants, live: Boolean): String? = when {
        !g.switchOn -> OFF
        !g.state || !g.history -> NO_PERMISSION
        !live -> LOCKED
        else -> null
    }

    /**
     * The number a dial goes to, or the word refusing it. An emergency number
     * is never placed from a computer: that call belongs on the phone, where
     * the person can be heard and located.
     */
    fun dialNumber(raw: String?, emergency: (String) -> Boolean): Result<String> {
        val n = raw?.let { Messages.number(it) } ?: return Result.failure(Messages.Refused(FAILED))
        if (emergency(n)) return Result.failure(Messages.Refused(EMERGENCY))
        return Result.success(n)
    }

    /** Android's call state as the wire says it. */
    fun stateOf(telephony: Int): String = when (telephony) {
        CALL_STATE_RINGING -> RINGING
        CALL_STATE_OFFHOOK -> ACTIVE
        else -> IDLE
    }

    /**
     * The phone's call as last seen: [state], since when ([sinceMs], this
     * phone's clock), and who, when Android said.
     */
    data class Now(val state: String, val sinceMs: Long, val number: String? = null, val name: String? = null) {
        // Somebody's number: never in a log line by accident.
        override fun toString(): String = "Now(state=$state, sinceMs=$sinceMs)"

        companion object {
            val IDLE_NOW = Now(IDLE, 0)
        }
    }

    /**
     * The call after Android says [telephony] at [nowMs] with [number]
     * (null when it did not say). Android says a ringing call twice, once
     * before the number is known: the second saying keeps what the first
     * knew, and its time.
     */
    fun next(before: Now, telephony: Int, number: String?, nowMs: Long): Now {
        val state = stateOf(telephony)
        val clean = number?.let { clean(it, NUMBER_CAP) }?.takeIf { it.isNotEmpty() }
        if (state == IDLE) return Now(IDLE, nowMs)
        if (state == before.state) {
            return before.copy(number = clean ?: before.number)
        }
        // An answered call keeps who it is with.
        val who = clean ?: if (before.state == RINGING) before.number else null
        val name = if (who != null && who == before.number) before.name else null
        return Now(state, nowMs, who, name)
    }

    /** The `call_state` line for [now]. An idle phone names nobody. */
    fun stateLine(now: Now): String = buildJsonObject {
        put("cmd", "call_state")
        put("state", now.state)
        if (now.state != IDLE) {
            now.number?.let { put("number", clean(it, NUMBER_CAP)) }
            now.name?.let { clean(it, NAME_CAP) }?.takeIf { it.isNotEmpty() }?.let { put("name", it) }
        }
        put("since_ms", now.sinceMs.coerceAtLeast(0))
    }.toString()

    /** An action's result: `ok`, or the word saying why not. */
    fun done(id: Long, error: String?): String = buildJsonObject {
        put("cmd", "call_done")
        put("id", id)
        put("ok", error == null)
        if (error != null) put("error", error)
    }.toString()

    /** The one line refusing history request [id] with [word]. */
    fun refused(id: Long, word: String): String = buildJsonObject {
        put("cmd", "call_history_reply")
        put("id", id)
        put("error", word)
    }.toString()

    /** One row of the call log, as the app read it. */
    data class Row(
        val number: String?,
        /** CallLog's NUMBER_PRESENTATION: 1 is a number the caller let through. */
        val presentation: Int,
        val cachedName: String?,
        val type: Int,
        val dateMs: Long,
        val durationS: Long,
    ) {
        override fun toString(): String = "Row(type=$type, dateMs=$dateMs)"
    }

    data class Entry(val number: String, val name: String?, val type: String, val dateMs: Long, val durationS: Long) {
        override fun toString(): String = "Entry(type=$type, dateMs=$dateMs)"
    }

    const val PRESENTATION_ALLOWED: Int = 1

    /**
     * A call log row's kind on the wire, or null for a row never shown: a
     * voicemail is not a call, and a type unknown here is left out rather
     * than guessed. A call blocked by the phone reads as rejected; one
     * answered on another device read as incoming.
     */
    fun type(type: Int): String? = when (type) {
        TYPE_INCOMING, TYPE_ANSWERED_EXTERNALLY -> "incoming"
        TYPE_OUTGOING -> "outgoing"
        TYPE_MISSED -> "missed"
        TYPE_REJECTED, TYPE_BLOCKED -> "rejected"
        else -> null
    }

    /**
     * The history from [rows] (newest first, as read): the newest [limit]
     * calls older than [before]. A withheld number is empty; [name] looks a
     * number up in the person's contacts when the log kept no name.
     */
    fun history(rows: Sequence<Row>, before: Long?, limit: Int, name: (String) -> String? = { null }): List<Entry> {
        val cap = limit.coerceIn(1, MAX_LIMIT)
        val out = ArrayList<Entry>()
        for (row in rows.take(SCAN_CAP)) {
            val kind = type(row.type) ?: continue
            if (before != null && row.dateMs >= before) continue
            val number = if (row.presentation == PRESENTATION_ALLOWED) clean(row.number, NUMBER_CAP) else ""
            val shown = row.cachedName?.takeIf { it.isNotBlank() } ?: number.takeIf { it.isNotEmpty() }?.let(name)
            out += Entry(
                number = number,
                name = shown?.let { clean(it, NAME_CAP) }?.takeIf { it.isNotEmpty() },
                type = kind,
                dateMs = row.dateMs.coerceAtLeast(0),
                durationS = row.durationS.coerceAtLeast(0),
            )
            if (out.size == cap) break
        }
        return out
    }

    /**
     * The `call_history_reply` lines answering request [id]. Each is at most
     * [CHUNK_BYTES]; every line but the last says `more`. No calls is one
     * line with an empty list.
     */
    fun historyReply(id: Long, entries: List<Entry>): List<String> {
        val items = entries.map { encode(it) }
        val groups = ArrayList<List<JsonObject>>()
        var current = ArrayList<JsonObject>()
        var size = 0
        val frame = 128
        for (item in items) {
            val n = item.toString().toByteArray(Charsets.UTF_8).size + 1
            if (current.isNotEmpty() && frame + size + n > CHUNK_BYTES) {
                groups += current
                current = ArrayList()
                size = 0
            }
            current += item
            size += n
        }
        groups += current
        return groups.mapIndexed { i, g ->
            buildJsonObject {
                put("cmd", "call_history_reply")
                put("id", id)
                put("calls", JsonArray(g))
                if (i < groups.size - 1) put("more", true)
            }.toString()
        }
    }

    private fun encode(e: Entry): JsonObject = buildJsonObject {
        put("number", e.number)
        if (e.name != null) put("name", e.name)
        put("type", e.type)
        put("date_ms", e.dateMs)
        put("duration_s", e.durationS)
    }

    private fun clean(raw: String?, cap: Int): String = Messages.clean(raw, cap, 0).trim()
}
