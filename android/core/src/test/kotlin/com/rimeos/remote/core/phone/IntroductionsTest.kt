package com.rimeos.remote.core.phone

import com.rimeos.remote.core.Base64Url
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Pairing
import com.rimeos.remote.core.PairingOffer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The phone's side of introductions (contract §2.15), every refusal included. */
class IntroductionsTest {
    private fun key(seed: Int): String = Base64Url.encode(ByteArray(32) { seed.toByte() })

    private fun machine(id: String, seed: Int) = PairedMachine(
        deviceId = id,
        machine = "m-$id",
        desktopKey = key(seed),
        deviceKey = key(100 + seed),
        sealed = "x",
        pairedMs = 1,
    )

    private fun cardReply(seed: Int, name: String, vararg known: Int): String =
        """{"reply":"computer_card","card":{"key":"${key(seed)}","name":"$name","lan":["192.168.1.9:7341"]},""" +
            """"known":[${known.joinToString(",") { "\"${key(it)}\"" }}]}"""

    private fun offer(seed: Int, expiresMs: Long = 10_000) =
        PairingOffer(v = 1, machine = "laptop", key = key(seed), token = key(50), expiresMs = expiresMs)

    @Test
    fun `join is reported only with the switch on, a computer that takes it, a full link and an unlocked app`() {
        assertTrue(Introductions.offered(true, remoteHas = true, companion = false, unlocked = true))
        assertFalse(Introductions.offered(null, remoteHas = true, companion = false, unlocked = true), "unset is off")
        assertFalse(Introductions.offered(false, remoteHas = true, companion = false, unlocked = true))
        assertFalse(Introductions.offered(true, remoteHas = false, companion = false, unlocked = true))
        assertFalse(Introductions.offered(true, remoteHas = true, companion = true, unlocked = true))
        assertFalse(Introductions.offered(true, remoteHas = true, companion = false, unlocked = false))
    }

    @Test
    fun `a card is read with its key and what it knows, and a refusal is no card`() {
        val c = Introductions.readCard(cardReply(1, "desk", 3))!!
        assertEquals(key(1), c.key)
        assertEquals(setOf(key(3)), c.known)
        assertEquals("desk", c.card["name"]!!.jsonPrimitive.content)
        assertNull(Introductions.readCard("""{"reply":"error","kind":"not_allowed","message":"no"}"""))
        assertNull(Introductions.readCard("""{"reply":"computer_card","card":{"key":"short","name":"x"}}"""))
        assertNull(Introductions.readCard("not json"))
    }

    @Test
    fun `two computers that do not know each other are each handed the other's card, and nothing else`() {
        val a = Introductions.Holder("a", key(1), Introductions.readCard(cardReply(1, "desk"))!!)
        val b = Introductions.Holder("b", key(2), Introductions.readCard(cardReply(2, "laptop"))!!)
        val plan = Introductions.plan(listOf(a, b))
        assertEquals(listOf("a" to b.card.card, "b" to a.card.card), plan)
        // The request carries the card as the other computer wrote it.
        val sent = Json.parseToJsonElement(Introductions.introduce(b.card.card)).jsonObject
        assertEquals("computer_introduce", sent["cmd"]!!.jsonPrimitive.content)
        assertEquals(b.card.card, sent["card"] as JsonObject)
    }

    @Test
    fun `computers that already know each other, paired or removed on either side, are left alone`() {
        val a = Introductions.Holder("a", key(1), Introductions.readCard(cardReply(1, "desk", 2))!!)
        val b = Introductions.Holder("b", key(2), Introductions.readCard(cardReply(2, "laptop"))!!)
        assertEquals(emptyList<Pair<String, JsonObject>>(), Introductions.plan(listOf(a, b)))
        assertEquals(emptyList<Pair<String, JsonObject>>(), Introductions.plan(listOf(b, a)))
    }

    @Test
    fun `a computer whose card is not the key this phone pinned is neither introduced nor told`() {
        val honest = Introductions.Holder("a", key(1), Introductions.readCard(cardReply(1, "desk"))!!)
        val liar = Introductions.Holder("b", key(2), Introductions.readCard(cardReply(9, "someone else"))!!)
        assertEquals(emptyList<Pair<String, JsonObject>>(), Introductions.plan(listOf(honest, liar)))
    }

    @Test
    fun `an outcome is read from the reply and a refusal has none`() {
        assertEquals("added", Introductions.outcome("""{"reply":"computer_introduced","outcome":"added","id":"x"}"""))
        assertEquals("removed", Introductions.outcome("""{"reply":"computer_introduced","outcome":"removed"}"""))
        assertNull(Introductions.outcome("""{"reply":"error","kind":"not_allowed","message":"no"}"""))
    }

    @Test
    fun `an offer is joined only on a full link of an unlocked app with the switch on, live, for a new computer`() {
        val held = listOf(machine("a", 1))
        assertTrue(Introductions.accept(false, true, true, offer(2), 1_000, held))
        assertFalse(Introductions.accept(true, true, true, offer(2), 1_000, held), "a companion link")
        assertFalse(Introductions.accept(false, null, true, offer(2), 1_000, held), "the switch unset")
        assertFalse(Introductions.accept(false, false, true, offer(2), 1_000, held), "the switch off")
        assertFalse(Introductions.accept(false, true, false, offer(2), 1_000, held), "locked")
        assertFalse(Introductions.accept(false, true, true, null, 1_000, held), "no offer")
        assertFalse(Introductions.accept(false, true, true, offer(2, expiresMs = 999), 1_000, held), "expired")
        assertFalse(Introductions.accept(false, true, true, offer(1), 1_000, held), "already paired")
    }

    @Test
    fun `an offer is decoded from its payload and an oversized one is not`() {
        val payload = Pairing.SCHEME + Base64Url.encode(Pairing.json.encodeToString(PairingOffer.serializer(), offer(2)).toByteArray())
        assertEquals(key(2), Introductions.decode(payload)?.key)
        assertNull(Introductions.decode("garbage"))
        assertNull(Introductions.decode(Pairing.SCHEME + "A".repeat(Introductions.MAX_OFFER)))
    }

    @Test
    fun `a removal forgets a computer this phone holds, never the sender, and only on a full link with the switch on`() {
        val desk = machine("a", 1)
        val laptop = machine("b", 2)
        val all = listOf(desk, laptop)
        assertEquals(laptop, Introductions.toForget(false, true, desk, key(2), all))
        assertNull(Introductions.toForget(true, true, desk, key(2), all), "a companion link")
        assertNull(Introductions.toForget(false, null, desk, key(2), all), "the switch unset")
        assertNull(Introductions.toForget(false, false, desk, key(2), all), "the switch off")
        assertNull(Introductions.toForget(false, true, desk, key(1), all), "the sender itself")
        assertNull(Introductions.toForget(false, true, desk, key(7), all), "a computer this phone never had")
    }

    @Test
    fun `the switch turns on with the first pairing made by hand, and nothing else changes it`() {
        assertEquals(true, Introductions.afterPairing(null, storeWasEmpty = true, byHand = true))
        assertNull(Introductions.afterPairing(null, storeWasEmpty = false, byHand = true), "an existing install")
        assertNull(Introductions.afterPairing(null, storeWasEmpty = true, byHand = false), "a join")
        assertEquals(false, Introductions.afterPairing(false, storeWasEmpty = true, byHand = true), "turned off stays off")
    }

    @Test
    fun `the events are read, and a join offer never prints its token`() {
        val e = Phone.event("""{"event":"join_offer","offer":"apex-remote:abc","computer":"laptop","by":"desk"}""")
        assertEquals(DeviceEvent.JoinOffer("apex-remote:abc", "laptop", "desk"), e)
        assertFalse(e.toString().contains("abc"), e.toString())
        assertNull(Phone.event("""{"event":"join_offer","offer":"","computer":"laptop","by":"desk"}"""))
        assertNull(Phone.event("""{"event":"join_offer","offer":"${"a".repeat(Introductions.MAX_OFFER + 1)}"}"""))
        assertEquals(
            DeviceEvent.DeviceRemoved(key(2), "laptop"),
            Phone.event("""{"event":"device_removed","key":"${key(2)}","name":"laptop"}"""),
        )
        assertNull(Phone.event("""{"event":"device_removed","key":"short","name":"laptop"}"""))
    }
}
