package com.rimeos.remote.core.phone

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** This phone's SMS on a computer (`docs/remote-live-contract.md` §2.11). */
class MessagesTest {
    private fun row(
        id: Long,
        thread: Long,
        address: String? = "5556",
        body: String? = "hello $id",
        date: Long = 1_000L * id,
        type: Int = Messages.TYPE_INBOX,
        read: Boolean = true,
        status: Int = -1,
    ) = Messages.Row(id, thread, address, body, date, type, read, status)

    private fun parse(line: String): JsonObject = Json.parseToJsonElement(line).jsonObject

    @Test
    fun `each kind of row is shown as the computer knows it, and a draft is never shown`() {
        assertEquals("received", Messages.status(Messages.TYPE_INBOX, -1))
        assertEquals("sent", Messages.status(Messages.TYPE_SENT, -1))
        assertEquals("sent", Messages.status(Messages.TYPE_SENT, 32))
        assertEquals("delivered", Messages.status(Messages.TYPE_SENT, Messages.STATUS_COMPLETE))
        assertEquals("failed", Messages.status(Messages.TYPE_SENT, Messages.STATUS_FAILED))
        assertEquals("sending", Messages.status(Messages.TYPE_OUTBOX, -1))
        assertEquals("sending", Messages.status(Messages.TYPE_QUEUED, -1))
        assertEquals("failed", Messages.status(Messages.TYPE_FAILED, -1))
        assertNull(Messages.status(Messages.TYPE_DRAFT, -1))
        assertNull(Messages.status(0, -1))
        assertNull(Messages.message(row(1, 1, type = Messages.TYPE_DRAFT)))
        val mine = Messages.message(row(2, 1, type = Messages.TYPE_SENT))!!
        assertTrue(mine.fromMe)
        assertFalse(Messages.message(row(3, 1))!!.fromMe)
    }

    @Test
    fun `the list has one thread per conversation with its newest message, counts and unread`() {
        // Newest first, as the store is read.
        val rows = sequenceOf(
            row(9, 2, address = "+15555215556", body = "newest", date = 9_000, read = false),
            row(8, 1, address = "5554", body = "draft", date = 8_500, type = Messages.TYPE_DRAFT),
            row(7, 1, address = "5554", body = "one", date = 8_000, type = Messages.TYPE_SENT),
            row(6, 2, address = "+15555215556", date = 7_000, read = false),
            row(5, 2, address = "+15555215556", date = 6_000, read = true),
            row(4, 3, address = "5556", date = 5_000),
            row(3, 3, address = "5554", date = 4_000),
        )
        val list = Messages.threads(rows, before = null, limit = 10) { if (it == "5554") "Pat" else null }
        assertEquals(listOf(2L, 1L, 3L), list.map { it.id })
        val t2 = list[0]
        assertEquals("newest", t2.snippet)
        assertEquals(9_000, t2.dateMs)
        assertEquals(3, t2.count)
        assertEquals(2, t2.unread)
        assertEquals(listOf("+15555215556"), t2.addresses)
        assertNull(t2.name)
        // The draft is neither the snippet nor counted.
        assertEquals("one", list[1].snippet)
        assertEquals(1, list[1].count)
        assertEquals("Pat", list[1].name)
        // Several people: both addresses, in order, and no single name.
        assertEquals(listOf("5556", "5554"), list[2].addresses)
        assertNull(list[2].name)
        // Paging: threads whose newest message is older than `before`.
        assertEquals(listOf(1L, 3L), Messages.threads(rows, before = 9_000, limit = 10).map { it.id })
        assertEquals(listOf(2L), Messages.threads(rows, before = null, limit = 1).map { it.id })
    }

    @Test
    fun `one conversation is its newest messages older than before, and a missing one is null`() {
        val rows = (10L downTo 1L).map { row(it, 7) }.asSequence()
        assertEquals(listOf(10L, 9L, 8L), Messages.thread(rows, null, 3)!!.map { it.id })
        assertEquals(listOf(5L, 4L), Messages.thread(rows, before = 6_000, limit = 2)!!.map { it.id })
        assertEquals(emptyList<Messages.Message>(), Messages.thread(rows, before = 1, limit = 2))
        assertNull(Messages.thread(emptySequence(), null, 5))
        assertNull(Messages.thread(sequenceOf(row(1, 7, type = Messages.TYPE_DRAFT)), null, 5))
        assertEquals(Messages.MAX_LIMIT, Messages.thread((200L downTo 1L).map { row(it, 7) }.asSequence(), null, 500)!!.size)
    }

    @Test
    fun `what a phone shows is cleaned and capped as the computer does it`() {
        assertEquals("a b", Messages.clean("a\tb", 10, 0))
        assertEquals("a\nb c", Messages.clean("a\nb\nc", 10, 1))
        assertEquals("ab", Messages.clean("a\r\u0007‮b", 10, 0))
        assertEquals("abc…", Messages.clean("abcdef", 3, 0))
        assertEquals("", Messages.clean(null, 3, 0))
        // Characters, not UTF-16 units: an emoji is one.
        assertEquals("😀😀", Messages.clean("😀😀", 2, 0))
        val long = Messages.message(row(1, 1, body = "x".repeat(3000)))!!
        assertEquals(Messages.TEXT_CAP + 1, long.body.length)
        val t = Messages.threads(sequenceOf(row(1, 1, address = "+1 555‮", body = "s\nt")), null, 5) { "\u0007Pat" }
        assertEquals(listOf("+1 555"), t[0].addresses)
        assertEquals("s t", t[0].snippet)
        assertEquals("Pat", t[0].name)
    }

    @Test
    fun `an answer is cut into lines the link carries, all but the last saying more, and none is lost`() {
        val messages = (1L..50L).map { Messages.Message(it, it, false, "é".repeat(2000), "received") }
        val lines = Messages.messagesReply(4, 7, messages)
        assertTrue(lines.size > 1, "a large answer takes several lines")
        lines.forEach { assertTrue(it.toByteArray().size <= Messages.CHUNK_BYTES, "${it.length}") }
        val parsed = lines.map { parse(it) }
        parsed.dropLast(1).forEach { assertTrue(it["more"]!!.jsonPrimitive.boolean) }
        assertNull(parsed.last()["more"])
        parsed.forEach {
            assertEquals("messages_reply", it["cmd"]!!.jsonPrimitive.content)
            assertEquals(4, it["id"]!!.jsonPrimitive.long)
            assertEquals(7, it["thread"]!!.jsonPrimitive.long)
        }
        val ids = parsed.flatMap { o -> o["messages"]!!.jsonArray.map { it.jsonObject["id"]!!.jsonPrimitive.long } }
        assertEquals((1L..50L).toList(), ids)
        // The computer takes at most 64 lines and 512 KiB.
        assertTrue(lines.size <= 64)
        assertTrue(lines.sumOf { it.toByteArray().size } <= 512 * 1024)
    }

    @Test
    fun `a small or empty answer is one line, and threads carry their fields`() {
        val one = Messages.threadsReply(2, emptyList())
        assertEquals(1, one.size)
        assertEquals(0, parse(one[0])["threads"]!!.jsonArray.size)
        assertNull(parse(one[0])["thread"])
        val t = Messages.Thread(5, listOf("5556"), "Pat", "hi", 10, 1, 3)
        val o = parse(Messages.threadsReply(2, listOf(t, t.copy(id = 6, name = null))).single())
        val first = o["threads"]!!.jsonArray[0].jsonObject
        assertEquals(5, first["id"]!!.jsonPrimitive.long)
        assertEquals("5556", first["addresses"]!!.jsonArray[0].jsonPrimitive.content)
        assertEquals("Pat", first["name"]!!.jsonPrimitive.content)
        assertEquals(1, first["unread"]!!.jsonPrimitive.int)
        assertEquals(3, first["count"]!!.jsonPrimitive.int)
        assertNull(o["threads"]!!.jsonArray[1].jsonObject["name"])
    }

    @Test
    fun `refusals and results are the words the computer knows`() {
        assertEquals("""{"cmd":"messages_reply","id":3,"error":"off"}""", Messages.refused(3, Messages.OFF))
        assertEquals("""{"cmd":"messages_sent","id":3,"ok":true}""", Messages.sent(3, null))
        assertEquals("""{"cmd":"messages_sent","id":3,"ok":false,"error":"no_service"}""", Messages.sent(3, Messages.NO_SERVICE))
        assertEquals("""{"cmd":"messages_changed"}""", Messages.changed())
    }

    @Test
    fun `the switch, the permissions and the lock decide, in that order`() {
        // Off wins whatever else holds.
        assertEquals(Messages.OFF, Messages.refusal(false, true, true, true, send = false))
        assertEquals(Messages.OFF, Messages.refusal(false, true, true, true, send = true))
        assertEquals(Messages.NO_PERMISSION, Messages.refusal(true, false, true, true, send = false))
        assertEquals(Messages.NO_PERMISSION, Messages.refusal(true, true, false, true, send = true))
        // Reading needs no send permission and no unlock (companion mode reads).
        assertNull(Messages.refusal(true, true, false, false, send = false))
        // A send needs an unlocked app on a full link.
        assertEquals(Messages.LOCKED, Messages.refusal(true, true, true, false, send = true))
        assertNull(Messages.refusal(true, true, true, true, send = true))
        assertTrue(Messages.offered(true, true))
        assertFalse(Messages.offered(false, true))
        assertFalse(Messages.offered(true, false))
    }

    @Test
    fun `a send goes to one number, never to a group, and its text is checked`() {
        val none: (Long) -> List<String>? = { null }
        assertEquals("+15555215556", Messages.destination(listOf("+1 (555) 521-5556"), null, none).getOrThrow())
        assertEquals(Messages.GROUP, (Messages.destination(listOf("5554", "5556"), null, none).exceptionOrNull() as Messages.Refused).word)
        assertEquals(Messages.FAILED, (Messages.destination(listOf("call me"), null, none).exceptionOrNull() as Messages.Refused).word)
        assertEquals(Messages.NO_THREAD, (Messages.destination(null, 4, none).exceptionOrNull() as Messages.Refused).word)
        assertEquals("5556", Messages.destination(null, 4) { listOf("5556", "55-56") }.getOrThrow())
        assertEquals(Messages.GROUP, (Messages.destination(null, 4) { listOf("5556", "5554") }.exceptionOrNull() as Messages.Refused).word)
        assertNull(Messages.number("12"))
        assertNull(Messages.number("5+5"))
        assertEquals("a\nb", Messages.text("a\nb\n").getOrThrow())
        assertEquals(Messages.TOO_LONG, (Messages.text("x".repeat(2001)).exceptionOrNull() as Messages.Refused).word)
        assertTrue(Messages.text("x".repeat(2000)).isSuccess)
        assertTrue(Messages.text("  \n").isFailure)
        assertTrue(Messages.text("a‮b").isFailure)
        assertTrue(Messages.text("a\u0007b").isFailure)
    }

    @Test
    fun `the computer's events are read strictly`() {
        assertEquals(
            DeviceEvent.MessagesRequest(3, null, null, 20),
            Phone.event("""{"event":"messages_request","id":3,"limit":20}"""),
        )
        assertEquals(
            DeviceEvent.MessagesRequest(3, 7, 99, 50),
            Phone.event("""{"event":"messages_request","id":3,"thread":7,"before":99,"limit":500}"""),
        )
        assertNull(Phone.event("""{"event":"messages_request","id":3,"limit":0}"""))
        assertNull(Phone.event("""{"event":"messages_request","id":3,"thread":"7","limit":5}"""))
        assertNull(Phone.event("""{"event":"messages_request","id":-1,"limit":5}"""))
        assertEquals(
            DeviceEvent.MessagesSend(4, listOf("5556"), null, "hi"),
            Phone.event("""{"event":"messages_send","id":4,"to":["5556"],"text":"hi"}"""),
        )
        assertEquals(
            DeviceEvent.MessagesSend(4, null, 7, "hi"),
            Phone.event("""{"event":"messages_send","id":4,"thread":7,"text":"hi"}"""),
        )
        // Both, neither, or a number that is not text: dropped.
        assertNull(Phone.event("""{"event":"messages_send","id":4,"to":["5556"],"thread":7,"text":"hi"}"""))
        assertNull(Phone.event("""{"event":"messages_send","id":4,"text":"hi"}"""))
        assertNull(Phone.event("""{"event":"messages_send","id":4,"to":[5556],"text":"hi"}"""))
        // A message never reaches a log line.
        val s = DeviceEvent.MessagesSend(4, listOf("5556"), null, "secret").toString()
        assertFalse("secret" in s || "5556" in s, s)
    }
}
