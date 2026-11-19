package com.rimeos.remote.core.phone

import com.rimeos.remote.core.Device
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Pairing
import com.rimeos.remote.core.PairingOffer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * "Join my other computers automatically" (`docs/remote-live-contract.md`
 * §2.15, Introductions): what the phone decides, kept here where a JVM test
 * can reach every branch. `LinkHub` only wires these to links.
 *
 * Two flows. In the first the phone introduces two computers it holds full
 * links to: it asks each for its card and hands each the other's. In the
 * second a computer brings the phone to a computer it is paired with: it
 * sends `join_offer`, and the phone pairs with the offer as if its code had
 * been scanned. A computer also tells the phone when it removed another
 * (`device_removed`), and the phone forgets that one too.
 *
 * Everything here is refused unless the switch is on, the app is unlocked and
 * the link is a full one: a companion link (the app locked, or this phone not
 * yet allowed on that computer) never introduces, joins or forgets.
 */
object Introductions {
    /** The feature a computer offers once it takes cards and introductions. */
    const val FEATURE: String = "computer_introductions"

    /** The word in `can` that says the switch is on, on a full link of an unlocked app. */
    const val CAN_JOIN: String = "join"

    /** The longest offer a `join_offer` carries; a pairing code is far shorter. */
    const val MAX_OFFER: Int = 4096

    private val json = Json { ignoreUnknownKeys = true }

    /** The request for a computer's card. */
    fun cardRequest(): String = """{"cmd":"computer_card"}"""

    /** The introduction of the computer on [card]. The card is passed on as the other computer wrote it. */
    fun introduce(card: JsonObject): String =
        buildJsonObject {
            put("cmd", "computer_introduce")
            put("card", card)
        }.toString()

    /**
     * Whether the report says [CAN_JOIN]: the switch on, a computer that offers
     * [FEATURE], a full link, and an unlocked app.
     */
    fun offered(autoJoin: Boolean?, remoteHas: Boolean, companion: Boolean, unlocked: Boolean): Boolean =
        autoJoin == true && remoteHas && !companion && unlocked

    /** One computer's card: its key, the card to pass on, and the keys it already knows. */
    data class Card(val key: String, val card: JsonObject, val known: Set<String>)

    /** A `computer_card` reply, or null for a refusal or anything that is not one. */
    fun readCard(reply: String): Card? {
        val o = runCatching { json.parseToJsonElement(reply) }.getOrNull() as? JsonObject ?: return null
        if ((o["reply"] as? JsonPrimitive)?.content != "computer_card") return null
        val card = o["card"] as? JsonObject ?: return null
        val key = (card["key"] as? JsonPrimitive)?.takeIf { it.isString }?.content ?: return null
        if (runCatching { Device.checkKey(key) }.isFailure) return null
        val known = (o["known"] as? JsonArray)
            ?.mapNotNull { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.content }
            ?.toSet()
            ?: emptySet()
        return Card(key, card, known)
    }

    /** The `outcome` of a `computer_introduce` reply, or null for a refusal. */
    fun outcome(reply: String): String? {
        val o = runCatching { json.parseToJsonElement(reply) }.getOrNull() as? JsonObject ?: return null
        if ((o["reply"] as? JsonPrimitive)?.content != "computer_introduced") return null
        return (o["outcome"] as? JsonPrimitive)?.content
    }

    /** A computer the phone holds a full link to, with the key it pinned at pairing and the card it gave. */
    data class Holder(val deviceId: String, val pinned: String, val card: Card)

    /**
     * Which computer is handed which card: every pair of computers that do
     * not know each other (paired, or removed: a removal is never undone
     * here), each given the other's card.
     *
     * A card whose key is not the key this phone pinned for that computer is
     * passed to nobody and sent nothing: a computer that answers with someone
     * else's card is not introduced, and does not get to introduce.
     */
    fun plan(holders: List<Holder>): List<Pair<String, JsonObject>> {
        val honest = holders.filter { it.card.key == it.pinned }.distinctBy { it.card.key }
        val out = mutableListOf<Pair<String, JsonObject>>()
        for (x in honest) {
            for (y in honest) {
                if (x === y) continue
                if (y.card.key in x.card.known || x.card.key in y.card.known) continue
                out += x.deviceId to y.card.card
            }
        }
        return out
    }

    /**
     * Whether to join on a `join_offer` that arrived from a computer: a full
     * link, the switch on, the app unlocked, an offer that decodes and is
     * still live, for a computer this phone is not paired with.
     *
     * The phone cannot check that the offer came from the computer it names:
     * any computer with a full link can offer any computer. What stands in
     * the way is that joining needs the person (the prompt that seals the new
     * key) and says so on the screen.
     */
    fun accept(
        companion: Boolean,
        autoJoin: Boolean?,
        unlocked: Boolean,
        offer: PairingOffer?,
        nowMs: Long,
        machines: List<PairedMachine>,
    ): Boolean {
        if (companion || autoJoin != true || !unlocked || offer == null) return false
        if (offer.expiresMs in 1..<nowMs) return false
        return machines.none { it.desktopKey == offer.key }
    }

    /** The offer in a `join_offer`, or null when it does not decode. */
    fun decode(payload: String): PairingOffer? =
        if (payload.length > MAX_OFFER) null else runCatching { Pairing.decodeOffer(payload) }.getOrNull()

    /**
     * The computer to forget on a `device_removed` from [sender]: one this
     * phone is paired with, never the sender itself, and only over a full
     * link with the switch on. Null for anything else.
     */
    fun toForget(
        companion: Boolean,
        autoJoin: Boolean?,
        sender: PairedMachine,
        key: String,
        machines: List<PairedMachine>,
    ): PairedMachine? {
        if (companion || autoJoin != true) return null
        if (key == sender.desktopKey) return null
        return machines.firstOrNull { it.desktopKey == key && it.deviceId != sender.deviceId }
    }

    /**
     * The line under a computer this phone joined through another: who
     * brought it there, and, while that computer's hello on a full key still
     * says companion, that a person has to allow it there. Null for a
     * pairing made with a code.
     */
    fun joinedLine(joinedBy: String?, helloScope: String?, unlocked: Boolean): String? = when {
        joinedBy == null -> null
        unlocked && helloScope == "companion" ->
            "Added by $joinedBy. Phone features only until you allow this phone there: run " +
                "`rime remote devices allow` on that computer."
        else -> "Added by $joinedBy"
    }

    /**
     * What becomes of the switch when a pairing lands. Unset (an install that
     * predates it) stays unset, except on the first pairing made by hand,
     * which turns it on: the setup flow. A join never changes it.
     */
    fun afterPairing(autoJoin: Boolean?, storeWasEmpty: Boolean, byHand: Boolean): Boolean? =
        if (autoJoin == null && storeWasEmpty && byHand) true else autoJoin
}
