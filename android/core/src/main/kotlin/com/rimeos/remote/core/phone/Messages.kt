package com.rimeos.remote.core.phone

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray

/**
 * This phone's SMS conversations on a paired computer
 * (`docs/remote-live-contract.md` §2.11): what a row of Android's SMS store
 * becomes on the wire, how an answer is cut into lines the link carries, and
 * when this phone refuses.
 *
 * The computer asks with a `messages_request` or `messages_send` event on
 * the phone's events channel; this phone answers with `messages_reply`
 * lines, then `messages_sent`, on the control channel. Nothing is kept: each
 * answer is read from the phone's own store when it is asked for.
 *
 * Everything here is plain Kotlin, so the rules are tested on the JVM; the
 * app reads the store and sends through Android's SmsManager.
 */
object Messages {
    /** The feature a machine's `remote_hello` lists when it asks for this phone's messages. */
    const val FEATURE: String = "phone_messages"

    /** Most threads, or messages, one request asks for. The computer caps it the same. */
    const val MAX_LIMIT: Int = 50

    /** Longest text sent, and longest body shown, in characters. The computer caps both the same. */
    const val TEXT_CAP: Int = 2000

    /** Newlines a body keeps; the rest become spaces, as on the computer. */
    const val BODY_LINES: Int = 40

    const val SNIPPET_CAP: Int = 160
    const val NAME_CAP: Int = 60
    const val ADDRESS_CAP: Int = 40
    const val MAX_ADDRESSES: Int = 10

    /**
     * Largest answer line, in bytes. A wire frame holds about 64 KiB and the
     * computer takes at most 512 KiB in 64 lines; at this size the largest
     * possible answer (50 messages of 2,000 four-byte characters) is about
     * 400 KiB in 17 lines, and one message always fits a line on its own.
     */
    const val CHUNK_BYTES: Int = 24 * 1024

    /**
     * How long a send waits for Android to say each part went. The computer
     * waits 35 seconds for the result, so the answer is always in time.
     */
    const val SEND_WAIT_MS: Long = 25_000

    /** Rows of the SMS store read for the list, newest first. Bounds the work on a huge history. */
    const val SCAN_CAP: Int = 20_000

    // Why this phone did not answer, as the computer knows them (§2.11).
    const val OFF: String = "off"
    const val NO_PERMISSION: String = "no_permission"
    const val LOCKED: String = "locked"
    const val BUSY: String = "busy"
    const val NO_THREAD: String = "no_thread"
    const val GROUP: String = "group"
    const val TOO_LONG: String = "too_long"
    const val FAILED: String = "failed"
    const val TIMEOUT: String = "timeout"
    const val NO_SERVICE: String = "no_service"

    // Android's Telephony.Sms columns, as numbers, so this file needs no Android.
    const val TYPE_INBOX: Int = 1
    const val TYPE_SENT: Int = 2
    const val TYPE_DRAFT: Int = 3
    const val TYPE_OUTBOX: Int = 4
    const val TYPE_FAILED: Int = 5
    const val TYPE_QUEUED: Int = 6
    const val STATUS_COMPLETE: Int = 0
    const val STATUS_FAILED: Int = 64

    /** One row of the SMS store, as the app read it. */
    data class Row(
        val id: Long,
        val thread: Long,
        val address: String?,
        val body: String?,
        val dateMs: Long,
        val type: Int,
        val read: Boolean,
        val status: Int,
    ) {
        // Somebody's message and number: never in a log line by accident.
        override fun toString(): String = "Row(id=$id, thread=$thread, type=$type)"
    }

    data class Thread(
        val id: Long,
        val addresses: List<String>,
        val name: String?,
        val snippet: String,
        val dateMs: Long,
        val unread: Int,
        val count: Int,
    ) {
        override fun toString(): String = "Thread(id=$id, count=$count)"
    }

    data class Message(val id: Long, val dateMs: Long, val fromMe: Boolean, val body: String, val status: String) {
        override fun toString(): String = "Message(id=$id, status=$status)"
    }

    /**
     * Whether a machine is told it may ask (`can` has [Phone.CAN_MESSAGES]):
     * the person's switch is on and reading SMS is granted. The report itself
     * goes only while the app is unlocked or in companion mode.
     */
    fun offered(switchOn: Boolean, canRead: Boolean): Boolean = switchOn && canRead

    /**
     * Why this phone will not answer a request now, or null when it will.
     * [unlocked] is an unlocked app on a full link: a send needs it, because
     * a send is the phone speaking for the person; reading runs in companion
     * mode as the other device features do. The switch is checked whatever
     * an older report told the machine.
     */
    fun refusal(switchOn: Boolean, canRead: Boolean, canSend: Boolean, unlocked: Boolean, send: Boolean): String? = when {
        !switchOn -> OFF
        !canRead -> NO_PERMISSION
        send && !canSend -> NO_PERMISSION
        send && !unlocked -> LOCKED
        else -> null
    }

    /** What a row's message is on the wire, or null for a row never shown (a draft, or a type unknown here). */
    fun status(type: Int, status: Int): String? = when (type) {
        TYPE_INBOX -> "received"
        TYPE_SENT -> when (status) {
            STATUS_COMPLETE -> "delivered"
            STATUS_FAILED -> "failed"
            else -> "sent"
        }
        TYPE_OUTBOX, TYPE_QUEUED -> "sending"
        TYPE_FAILED -> "failed"
        else -> null
    }

    fun message(row: Row): Message? {
        val status = status(row.type, row.status) ?: return null
        return Message(row.id, row.dateMs.coerceAtLeast(0), row.type != TYPE_INBOX, clean(row.body, TEXT_CAP, BODY_LINES), status)
    }

    /**
     * The conversation list from [rows], newest first as the store gave
     * them: one thread per thread id, its newest shown message as the
     * snippet, its addresses in the order they appear, how many messages
     * and how many unread. Threads whose newest message is not older than
     * [before] are left out, then the first [limit] kept. [name] looks a
     * number up in the person's contacts, or gives null.
     */
    fun threads(rows: Sequence<Row>, before: Long?, limit: Int, name: (String) -> String? = { null }): List<Thread> {
        class Acc(val first: Row, val firstMessage: Message) {
            val addresses = LinkedHashSet<String>()
            var count = 0
            var unread = 0
        }
        val acc = LinkedHashMap<Long, Acc>()
        for (row in rows.take(SCAN_CAP)) {
            val m = message(row) ?: continue
            val a = acc.getOrPut(row.thread) { Acc(row, m) }
            a.count++
            if (row.type == TYPE_INBOX && !row.read) a.unread++
            val address = clean(row.address, ADDRESS_CAP, 0)
            if (address.isNotEmpty() && a.addresses.size < MAX_ADDRESSES) a.addresses += address
        }
        return acc.values.asSequence()
            .filter { before == null || it.first.dateMs < before }
            .filter { it.addresses.isNotEmpty() }
            .take(limit.coerceIn(1, MAX_LIMIT))
            .map { a ->
                val addresses = a.addresses.toList()
                val contact = if (addresses.size == 1) name(addresses[0]) else null
                Thread(
                    id = a.first.thread,
                    addresses = addresses,
                    name = contact?.let { clean(it, NAME_CAP, 0) }?.takeIf { it.isNotEmpty() },
                    snippet = clean(a.firstMessage.body, SNIPPET_CAP, 0),
                    dateMs = a.first.dateMs.coerceAtLeast(0),
                    unread = a.unread,
                    count = a.count,
                )
            }
            .toList()
    }

    /**
     * One conversation's messages from [rows] (that thread's, newest first):
     * the newest [limit] older than [before]. Null when the thread has no
     * message to show at all, which the computer hears as [NO_THREAD].
     */
    fun thread(rows: Sequence<Row>, before: Long?, limit: Int): List<Message>? {
        var any = false
        val out = ArrayList<Message>()
        val cap = limit.coerceIn(1, MAX_LIMIT)
        for (row in rows.take(SCAN_CAP)) {
            val m = message(row) ?: continue
            any = true
            if (before != null && row.dateMs >= before) continue
            out += m
            if (out.size == cap) break
        }
        return if (any) out else null
    }

    /**
     * The `messages_reply` lines answering request [id]: [threads] for the
     * list, or [messages] of [thread]. Each line is at most [CHUNK_BYTES]
     * long (one item always fits a line); every line but the last says
     * `more`, so the computer knows the answer is not over. No items is one
     * line with an empty list.
     */
    fun threadsReply(id: Long, threads: List<Thread>): List<String> =
        chunk(threads.map { encode(it) }) { items, more -> reply(id, "threads", items, more, thread = null) }

    fun messagesReply(id: Long, thread: Long, messages: List<Message>): List<String> =
        chunk(messages.map { encode(it) }) { items, more -> reply(id, "messages", items, more, thread) }

    /** The one line refusing request [id] with [word]. */
    fun refused(id: Long, word: String): String =
        buildJsonObject {
            put("cmd", "messages_reply")
            put("id", id)
            put("error", word)
        }.toString()

    /** A send's result: `ok`, or the word saying why not. */
    fun sent(id: Long, error: String?): String =
        buildJsonObject {
            put("cmd", "messages_sent")
            put("id", id)
            put("ok", error == null)
            if (error != null) put("error", error)
        }.toString()

    /** Something changed in the store: the computer may ask again. Carries nothing. */
    fun changed(): String = """{"cmd":"messages_changed"}"""

    /**
     * Where a send goes: [to] (one number), or [thread]'s one address. A
     * conversation with several people is refused ([GROUP]): an SMS to each
     * would be several conversations, not that one. Returns the number, or
     * the word refusing it.
     */
    fun destination(to: List<String>?, thread: Long?, threadAddresses: (Long) -> List<String>?): Result<String> {
        if (to != null) {
            if (to.size != 1) return Result.failure(Refused(GROUP))
            val n = number(to[0]) ?: return Result.failure(Refused(FAILED))
            return Result.success(n)
        }
        val addresses = thread?.let(threadAddresses)?.mapNotNull { number(it) }?.distinct()
        return when {
            addresses.isNullOrEmpty() -> Result.failure(Refused(NO_THREAD))
            addresses.size > 1 -> Result.failure(Refused(GROUP))
            else -> Result.success(addresses[0])
        }
    }

    /** A send refused with a word the computer knows. */
    class Refused(val word: String) : Exception(word)

    /**
     * A number: digits, an optional leading `+`, and the spaces, dashes, dots
     * and brackets people write; returned without those, or null. 3 to 20
     * digits, as on the computer.
     */
    fun number(raw: String): String? {
        val t = raw.trim()
        val out = StringBuilder()
        for ((i, c) in t.withIndex()) {
            when {
                c in '0'..'9' -> out.append(c)
                c == '+' && i == 0 -> out.append(c)
                c == ' ' || c == '-' || c == '.' || c == '(' || c == ')' -> {}
                else -> return null
            }
        }
        val digits = out.trimStart('+').length
        return if (digits in 3..20) out.toString() else null
    }

    /**
     * The text of a send, or the word refusing it: trailing newlines go; it
     * may not be empty, longer than [TEXT_CAP] characters, or carry control
     * or direction-changing characters but newlines.
     */
    fun text(raw: String): Result<String> {
        val t = raw.trimEnd('\n', '\r')
        if (t.isBlank()) return Result.failure(Refused(FAILED))
        if (t.codePointCount(0, t.length) > TEXT_CAP) return Result.failure(Refused(TOO_LONG))
        if (t.any { (it.isISOControl() && it != '\n') || bidi(it) }) return Result.failure(Refused(FAILED))
        return Result.success(t)
    }

    /**
     * Text made safe to show: control and direction-changing characters go,
     * up to [lines] newlines stay (the rest, and tabs, become spaces), and
     * it is cut to [cap] characters with an ellipsis. The computer does the
     * same again; neither side relies on the other.
     */
    fun clean(raw: String?, cap: Int, lines: Int): String {
        val s = raw ?: return ""
        val out = StringBuilder()
        var count = 0
        var kept = 0
        var i = 0
        while (i < s.length) {
            val cp = s.codePointAt(i)
            i += Character.charCount(cp)
            val c: Int = when {
                cp == '\n'.code && kept < lines -> {
                    kept++
                    cp
                }
                cp == '\n'.code || cp == '\t'.code -> ' '.code
                cp == '\r'.code -> continue
                Character.isISOControl(cp) || (cp <= 0xFFFF && bidi(cp.toChar())) || cp == 0x2028 || cp == 0x2029 -> continue
                else -> cp
            }
            if (count == cap) {
                return out.toString().trimEnd() + "…"
            }
            count++
            out.appendCodePoint(c)
        }
        return out.toString()
    }

    private fun bidi(c: Char): Boolean =
        c == '؜' || c == '‎' || c == '‏' || c in '‪'..'‮' || c in '⁦'..'⁩'

    private fun encode(t: Thread): JsonObject = buildJsonObject {
        put("id", t.id)
        putJsonArray("addresses") { t.addresses.forEach { add(kotlinx.serialization.json.JsonPrimitive(it)) } }
        if (t.name != null) put("name", t.name)
        put("snippet", t.snippet)
        put("date_ms", t.dateMs)
        put("unread", t.unread)
        put("count", t.count)
    }

    private fun encode(m: Message): JsonObject = buildJsonObject {
        put("id", m.id)
        put("date_ms", m.dateMs)
        put("from_me", m.fromMe)
        put("body", m.body)
        put("status", m.status)
    }

    private fun reply(id: Long, key: String, items: List<JsonObject>, more: Boolean, thread: Long?): String =
        buildJsonObject {
            put("cmd", "messages_reply")
            put("id", id)
            if (thread != null) put("thread", thread)
            put(key, kotlinx.serialization.json.JsonArray(items))
            if (more) put("more", true)
        }.toString()

    /** Packs [items] into as few lines as fit [CHUNK_BYTES], in order; [line] writes one. */
    private fun chunk(items: List<JsonObject>, line: (List<JsonObject>, Boolean) -> String): List<String> {
        val groups = ArrayList<List<JsonObject>>()
        var current = ArrayList<JsonObject>()
        var size = 0
        // Room for the line's own keys around the items.
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
        return groups.mapIndexed { i, g -> line(g, i < groups.size - 1) }
    }
}
