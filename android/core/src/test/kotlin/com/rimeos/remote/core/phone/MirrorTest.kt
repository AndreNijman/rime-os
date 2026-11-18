package com.rimeos.remote.core.phone

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** This phone's notifications on a computer (`docs/remote-live-contract.md` §2.7). */
class MirrorTest {
    private val chosen = Facts(pkg = "org.example.chat", ownPkg = "com.rimeos.remote", allowed = setOf("org.example.chat"))

    @Test
    fun `only a chosen app's plain notifications are sent`() {
        assertNull(Mirror.skip(chosen))
        assertEquals(Skip.NOT_CHOSEN, Mirror.skip(chosen.copy(allowed = emptySet())))
        // This app's own, even if somebody put it on the list.
        assertEquals(Skip.OWN, Mirror.skip(chosen.copy(pkg = "com.rimeos.remote", allowed = setOf("com.rimeos.remote"))))
        assertEquals(Skip.OTHER_PROFILE, Mirror.skip(chosen.copy(otherUser = true)))
        assertEquals(Skip.LOCAL_ONLY, Mirror.skip(chosen.copy(localOnly = true)))
        assertEquals(Skip.SECRET, Mirror.skip(chosen.copy(secret = true)))
        assertEquals(Skip.GROUP_SUMMARY, Mirror.skip(chosen.copy(groupSummary = true)))
        assertEquals(Skip.ONGOING, Mirror.skip(chosen.copy(ongoing = true)))
        assertEquals(Skip.ONGOING, Mirror.skip(chosen.copy(foregroundService = true)))
        assertEquals(Skip.MEDIA, Mirror.skip(chosen.copy(media = true)))
    }

    @Test
    fun `the key is a hash of Android's key and fits the computer's rule`() {
        val k = Mirror.key("0|org.example.chat|7|thread:42|10123")
        assertTrue(Regex("[0-9a-f]{32}").matches(k), k)
        assertTrue("chat" !in k)
        assertEquals(k, Mirror.key("0|org.example.chat|7|thread:42|10123"))
        assertTrue(k != Mirror.key("0|org.example.chat|7|thread:43|10123"))
    }

    @Test
    fun `fields are clipped in characters, never inside a surrogate pair`() {
        val face = "😀"
        val c = Content.of("A".repeat(100), face.repeat(500), "x".repeat(5000))
        assertEquals(Mirror.APP_CAP + 1, c.app.length)
        assertEquals(Mirror.TITLE_CAP, c.title.codePointCount(0, c.title.length) - 1)
        assertTrue(c.title.endsWith("$face…"), c.title.takeLast(4))
        assertEquals(Mirror.TEXT_CAP + 1, c.text.length)
        assertTrue(Content.of("App", " ", null).empty)
    }

    @Test
    fun `the verbs carry exactly the four fields`() {
        val o = Json.parseToJsonElement(Mirror.post("ab12", Content("Chat", "Sam \"S\"", "hi\nthere"))).jsonObject
        assertEquals(setOf("cmd", "key", "app", "title", "text"), o.keys)
        assertEquals("phone_notification", o["cmd"]!!.jsonPrimitive.content)
        assertEquals("hi\nthere", o["text"]!!.jsonPrimitive.content)
        assertEquals("""{"cmd":"phone_notification_removed","key":"ab12"}""", Mirror.removed("ab12"))
    }

    private fun c(t: String) = Content("Chat", t, "")

    private fun drain(o: MirrorOutbox, now: Long): List<MirrorItem> =
        generateSequence { o.next(now) }.toList().onEach(o::sent)

    @Test
    fun `the newest state per notification wins and an unchanged one is not sent`() {
        val o = MirrorOutbox(0)
        o.offer("a", c("1"), 0)
        o.offer("a", c("2"), 0)
        o.offer("b", c("x"), 0)
        o.offer("a", c("3"), 0)
        assertEquals(listOf(MirrorItem.Post("b", c("x")), MirrorItem.Post("a", c("3"))), drain(o, 0))
        o.offer("a", c("3"), 0)
        assertEquals(0, o.pending)
        o.offer("a", c("4"), 0)
        o.offer("a", c("3"), 0)
        assertEquals(emptyList<MirrorItem>(), drain(o, 0))
    }

    @Test
    fun `a removal goes only for what was sent, cancels a waiting post, and is free`() {
        val o = MirrorOutbox(0, burst = 1, refillMs = 1000)
        o.offer("never-sent", c("x"), 0)
        o.remove("never-sent")
        assertEquals(0, o.pending)
        o.remove("unknown")
        assertEquals(0, o.pending)

        o.offer("a", c("1"), 0)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("a", c("1"))), drain(o, 0))
        // The allowance is spent, and the removal still goes.
        o.offer("b", c("2"), 0)
        o.remove("a")
        assertEquals(listOf<MirrorItem>(MirrorItem.Removed("a")), drain(o, 0))
        o.remove("a")
        assertEquals(1, o.pending)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("b", c("2"))), drain(o, 1000))
    }

    @Test
    fun `posts are allowed a burst, then one per refill, never faster than the computer takes`() {
        val o = MirrorOutbox(0)
        repeat(20) { o.offer("k$it", c("$it"), 0) }
        assertEquals(8, drain(o, 0).size)
        assertEquals(4_000, o.waitMs(0))
        assertEquals(1, o.waitMs(3_999))
        assertEquals(1, drain(o, 4_000).size)
        // Sustained: one per 4 s here, against the computer's 10 then one per
        // 3 s, so nothing the phone sends is ever dropped there.
        var now = 4_000L
        var sent = 0
        while (o.pending > 0) {
            now += 4_000
            sent += drain(o, now).size
        }
        assertEquals(11, sent)
        assertEquals(0, o.waitMs(now))
    }

    @Test
    fun `only so many wait, oldest dropped first`() {
        val o = MirrorOutbox(0, capacity = 3)
        repeat(5) { o.offer("k$it", c("$it"), 0) }
        assertEquals(3, o.pending)
        assertEquals(listOf("k2", "k3", "k4"), drain(o, 0).map { it.key })
    }

    @Test
    fun `a removal while its post is on the wire still goes, and a dropped link waits for nothing`() {
        val o = MirrorOutbox(0)
        o.offer("a", c("1"), 0)
        val post = o.next(0)!!
        o.remove("a")
        o.sent(post)
        assertEquals(listOf<MirrorItem>(MirrorItem.Removed("a")), drain(o, 0))

        o.offer("b", c("2"), 0)
        o.offer("c", c("3"), 0)
        drain(o, 0)
        o.offer("b", c("2b"), 0)
        o.drop()
        assertEquals(0, o.pending)
        // What the computer shows survived the drop: its removal still goes.
        o.remove("c")
        assertEquals(listOf<MirrorItem>(MirrorItem.Removed("c")), drain(o, 0))
    }

    @Test
    fun `a computer dismisses only what it was sent, and is not sent the removal back`() {
        val o = MirrorOutbox(0)
        o.offer("a", c("1"), 0)
        o.offer("b", c("2"), 0)
        drain(o, 0)
        o.offer("w", c("waiting"), 0)
        assertTrue(o.dismissedThere("a"))
        assertTrue(!o.dismissedThere("a"), "dismissed twice")
        assertTrue(!o.dismissedThere("never-sent"))
        // Waiting, never shown there: not this computer's to dismiss, and the
        // post no longer goes.
        assertTrue(!o.dismissedThere("w"))
        // The phone's own removal follows; the computer that dismissed it
        // is not told, another one that shows it still is.
        o.remove("a")
        o.remove("b")
        assertEquals(listOf<MirrorItem>(MirrorItem.Removed("b")), drain(o, 0))
    }

    @Test
    fun `a key named back must have the shape this phone gives`() {
        assertTrue(Mirror.validKey(Mirror.key("0|org.example.chat|1|tag|10001")))
        for (bad in listOf("", "A".repeat(32), "0".repeat(31), "0".repeat(33), "../" + "0".repeat(29))) {
            assertTrue(!Mirror.validKey(bad), bad)
        }
    }

    @Test
    fun `the reply label comes from a free-form reply action, clipped, or says Reply`() {
        assertNull(Mirror.replyLabel(emptyList()))
        // Canned choices only: nothing a computer can type into.
        assertNull(Mirror.replyLabel(listOf(ReplyAction("Mark read", false), ReplyAction("Yes / No", false))))
        assertEquals("Answer", Mirror.replyLabel(listOf(ReplyAction("Mark read", false), ReplyAction("  Answer ", true))))
        assertEquals(Mirror.REPLY_LABEL, Mirror.replyLabel(listOf(ReplyAction("   ", true))))
        assertEquals(Mirror.REPLY_LABEL, Mirror.replyLabel(listOf(ReplyAction(null, true))))
        val long = Mirror.replyLabel(listOf(ReplyAction("😀".repeat(50), true)))!!
        assertEquals(Mirror.REPLY_LABEL_CAP + 1, long.codePointCount(0, long.length))
        assertTrue(long.endsWith("😀…"))
    }

    @Test
    fun `the reply label is sent only when there is one, and changing it alone is a change`() {
        val bare = Json.parseToJsonElement(Mirror.post("ab12", Content("Chat", "Sam", "hi"))).jsonObject
        assertTrue("reply" !in bare.keys)
        val withReply = Json.parseToJsonElement(Mirror.post("ab12", Content("Chat", "Sam", "hi", "Reply"))).jsonObject
        assertEquals("Reply", withReply["reply"]!!.jsonPrimitive.content)

        val o = MirrorOutbox(0)
        o.offer("a", Content("Chat", "Sam", "hi"), 0)
        drain(o, 0)
        o.offer("a", Content("Chat", "Sam", "hi", "Reply"), 0)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("a", Content("Chat", "Sam", "hi", "Reply"))), drain(o, 0))
    }

    @Test
    fun `after a reply the app's first update within five seconds is shown there already`() {
        val o = MirrorOutbox(0)
        val asked = Content("Chat", "Ann", "Hi there", "Reply")
        o.offer("a", asked, 0)
        o.offer("plain", c("no reply field"), 0)
        drain(o, 0)
        // Only what this computer shows, and only with a reply field.
        assertTrue(!o.repliedThere("never-sent", 1_000))
        assertTrue(!o.repliedThere("plain", 1_000))
        assertTrue(o.repliedThere("a", 1_000))

        // The app shows the reply: taken as shown, not sent, and no allowance spent.
        val answered = Content("Chat", "Ann", "You: on my way", "Reply")
        o.offer("a", answered, 1_000 + Mirror.ABSORB_MS)
        assertEquals(0, o.pending)
        assertEquals(emptyList<MirrorItem>(), drain(o, 1_000 + Mirror.ABSORB_MS))
        // The same again is unchanged; a new message goes as news.
        o.offer("a", answered, 7_000)
        assertEquals(0, o.pending)
        val next = Content("Chat", "Ann", "See you", "Reply")
        o.offer("a", next, 7_000)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("a", next)), drain(o, 7_000))
        // A removal after it still goes.
        o.remove("a")
        assertEquals(listOf<MirrorItem>(MirrorItem.Removed("a")), drain(o, 7_000))
    }

    @Test
    fun `after five seconds nothing is absorbed`() {
        val o = MirrorOutbox(0)
        o.offer("a", Content("Chat", "Ann", "Hi", "Reply"), 0)
        drain(o, 0)
        assertTrue(o.repliedThere("a", 0))
        val late = Content("Chat", "Ann", "You: late", "Reply")
        o.offer("a", late, Mirror.ABSORB_MS + 1)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("a", late)), drain(o, Mirror.ABSORB_MS + 1))
    }

    @Test
    fun `an absorbed update is only the first, and a reply leaves other notifications alone`() {
        val o = MirrorOutbox(0)
        o.offer("a", Content("Chat", "Ann", "Hi", "Reply"), 0)
        o.offer("b", Content("Chat", "Bob", "Yo", "Reply"), 0)
        drain(o, 0)
        assertTrue(o.repliedThere("a", 0))
        // Another notification updates meanwhile: it goes.
        val bob = Content("Chat", "Bob", "Yo again", "Reply")
        o.offer("b", bob, 100)
        o.offer("a", Content("Chat", "Ann", "You: ok", "Reply"), 200)
        val second = Content("Chat", "Ann", "Ann: great", "Reply")
        o.offer("a", second, 300)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("b", bob), MirrorItem.Post("a", second)), drain(o, 300))
    }

    @Test
    fun `a reply that found nothing to answer holds no update back`() {
        val o = MirrorOutbox(0)
        o.offer("a", Content("Chat", "Ann", "Hi", "Reply"), 0)
        drain(o, 0)
        assertTrue(o.repliedThere("a", 0))
        o.replyFailed("a")
        val news = Content("Chat", "Ann", "Still there?", "Reply")
        o.offer("a", news, 100)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("a", news)), drain(o, 100))
    }
    @Test
    fun `a reply from another computer closes the copy here and the app's answer is not sent`() {
        val o = MirrorOutbox(0)
        o.offer("a", Content("Chat", "Ann", "Hi", "Reply"), 0)
        o.offer("b", Content("Chat", "Bob", "Yo", "Reply"), 0)
        drain(o, 0)
        // Nothing of it here: nothing to close.
        assertTrue(!o.repliedElsewhere("never-sent", 1_000))
        assertTrue(o.repliedElsewhere("a", 1_000))
        assertEquals(listOf<MirrorItem>(MirrorItem.Removed("a")), drain(o, 1_000))
        // The app shows the reply: not news here, not sent; Bob's update still goes.
        o.offer("a", Content("Chat", "Ann", "You: on my way", "Reply"), 1_000 + Mirror.ABSORB_MS)
        val bob = Content("Chat", "Bob", "Yo again", "Reply")
        o.offer("b", bob, 1_000 + Mirror.ABSORB_MS)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("b", bob)), drain(o, 1_000 + Mirror.ABSORB_MS))
        // Only the first update: a new message after it is news again.
        val next = Content("Chat", "Ann", "See you", "Reply")
        o.offer("a", next, 7_000)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("a", next)), drain(o, 7_000))
    }

    @Test
    fun `a reply from another computer cancels a waiting post, and after five seconds nothing is held back`() {
        val o = MirrorOutbox(0, burst = 0)
        o.offer("a", Content("Chat", "Ann", "Hi", "Reply"), 0)
        assertTrue(o.repliedElsewhere("a", 0))
        assertEquals(0, o.pending)
        val late = Content("Chat", "Ann", "You: late", "Reply")
        o.offer("a", late, Mirror.ABSORB_MS + 1)
        assertEquals(1, o.pending)
    }

    @Test
    fun `a reply elsewhere that found nothing to answer holds no update back`() {
        val o = MirrorOutbox(0)
        o.offer("a", Content("Chat", "Ann", "Hi", "Reply"), 0)
        drain(o, 0)
        assertTrue(o.repliedElsewhere("a", 0))
        drain(o, 0)
        o.replyFailed("a")
        val news = Content("Chat", "Ann", "Still there?", "Reply")
        o.offer("a", news, 100)
        assertEquals(listOf<MirrorItem>(MirrorItem.Post("a", news)), drain(o, 100))
    }
}
