package com.rimeos.remote.core.phone

import com.rimeos.remote.core.Base64Url
import com.rimeos.remote.core.InMemoryStaticKey
import com.rimeos.remote.core.MachineStore
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.SecretBox
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.agent.RemoteHello
import com.rimeos.remote.core.link.FakeMachine
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.util.concurrent.ConcurrentLinkedQueue

/**
 * The companion key's lifecycle: on, off, unpair, replace, and what reaches
 * the machine at each step.
 */
class CompanionKeysTest {
    /** A box per machine, keyed by a per-box byte: enough to tell two boxes apart. */
    private class TestBox(private val salt: Byte) : SecretBox {
        override fun seal(plaintext: ByteArray) = ByteArray(plaintext.size) { (plaintext[it].toInt() xor salt.toInt()).toByte() } + salt
        override fun open(ciphertext: ByteArray): ByteArray {
            check(ciphertext.last() == salt) { "the box's key was deleted" }
            return ByteArray(ciphertext.size - 1) { (ciphertext[it].toInt() xor salt.toInt()).toByte() }
        }
        override val describe = "test"
    }

    private class Vault(var store: MachineStore) : CompanionVault {
        val boxes = HashMap<String, TestBox>()
        val dropped = ArrayList<String>()
        private var nextSalt: Byte = 1
        override fun load() = store
        override fun update(change: (MachineStore) -> MachineStore): MachineStore { store = change(store); return store }
        override fun box(deviceId: String) = boxes.getOrPut(deviceId) { TestBox(nextSalt++) }
        override fun drop(deviceId: String) { boxes.remove(deviceId); dropped += deviceId }
    }

    private val machine = PairedMachine(
        deviceId = "dev-one",
        machine = "test-machine",
        desktopKey = Base64Url.encode(ByteArray(32) { 1 }),
        deviceKey = Base64Url.encode(ByteArray(32) { 2 }),
        sealed = Base64Url.encode(ByteArray(48)),
        pairedMs = 1,
    )

    private val hello = RemoteHello(version = 1, features = listOf(CompanionLink.FEATURE))

    /** A machine that answers register with [registerReply] and records every line. */
    private fun fake(registerReply: () -> String = { """{"reply":"ok"}""" }) = FakeMachine(
        control = { line ->
            when (CompanionLink.cmdOf(line.toByteArray())) {
                "companion_register" -> registerReply()
                else -> """{"reply":"ok"}"""
            }
        },
    )

    private fun link(f: FakeMachine, scope: Scope = Scope.FULL) = MachineLink(f::open, silenceMs = 0, dialled = scope)

    private fun ConcurrentLinkedQueue<String>.cmds() = map { CompanionLink.cmdOf(it.toByteArray()) }

    private fun registeredKey(line: String): String =
        Regex("\"public_key\":\"([^\"]+)\"").find(line)!!.groupValues[1]

    @Test
    fun `turning on mints a sealed key, registers it, and only then marks it registered`() {
        val vault = Vault(MachineStore().with(machine))
        val f = fake()
        val l = link(f)
        val keys = CompanionKeys(vault)
        assertFalse(CompanionKeys.shouldStart(vault.store))
        keys.turnOn(mapOf(machine.deviceId to (l to hello)))

        val stored = vault.store.find(machine.deviceId)!!.companion!!
        assertTrue(stored.registered)
        assertTrue(vault.store.settings.companionOn)
        assertEquals(listOf("companion_register"), f.requests.cmds())
        assertEquals(stored.publicKey, registeredKey(f.requests.first()))
        // The private half is stored sealed, and opens back to the same pair.
        val identity = keys.identity(vault.store.find(machine.deviceId)!!)!!
        assertEquals(stored.publicKey, identity.publicKeyText())
        assertTrue(CompanionKeys.shouldStart(vault.store))
        l.close()
    }

    @Test
    fun `a key whose registration failed is stored but not counted`() {
        val vault = Vault(MachineStore().with(machine))
        val f = fake { """{"reply":"error","kind":"internal","message":"disk full"}""" }
        val l = link(f)
        CompanionKeys(vault).turnOn(mapOf(machine.deviceId to (l to hello)))
        val stored = vault.store.find(machine.deviceId)!!.companion
        assertNotNull(stored)
        assertFalse(stored!!.registered)
        assertFalse(CompanionKeys.shouldStart(vault.store))
        assertNull(CompanionKeys(vault).identity(vault.store.find(machine.deviceId)!!))
        l.close()
    }

    @Test
    fun `every full connect re-registers the same key rather than minting another`() {
        val vault = Vault(MachineStore().with(machine))
        var mints = 0
        val keys = CompanionKeys(vault) { mints++; InMemoryStaticKey.generate() }
        val f = fake()
        val l = link(f)
        keys.turnOn(mapOf(machine.deviceId to (l to hello)))
        keys.sync(machine.deviceId, l, hello)
        keys.sync(machine.deviceId, l, hello)
        assertEquals(1, mints)
        val sent = f.requests.toList()
        assertEquals(3, sent.size)
        assertEquals(1, sent.map(::registeredKey).toSet().size)
        l.close()
    }

    @Test
    fun `a refused key is deleted and replaced once`() {
        val vault = Vault(MachineStore().with(machine))
        var calls = 0
        val f = fake { if (calls++ == 0) """{"reply":"error","kind":"key_in_use","message":"retired"}""" else """{"reply":"ok"}""" }
        val l = link(f)
        CompanionKeys(vault).turnOn(mapOf(machine.deviceId to (l to hello)))
        val sent = f.requests.toList()
        assertEquals(2, sent.size)
        assertNotEquals(registeredKey(sent[0]), registeredKey(sent[1]))
        val stored = vault.store.find(machine.deviceId)!!.companion!!
        assertEquals(registeredKey(sent[1]), stored.publicKey)
        assertTrue(stored.registered)
        assertEquals(listOf(machine.deviceId), vault.dropped)
        l.close()
    }

    @Test
    fun `turning off forgets on a connected machine and deletes the key`() {
        val vault = Vault(MachineStore().with(machine))
        val f = fake()
        val l = link(f)
        val keys = CompanionKeys(vault)
        keys.turnOn(mapOf(machine.deviceId to (l to hello)))
        keys.turnOff(mapOf(machine.deviceId to (l to hello)))
        assertEquals(listOf("companion_register", "companion_forget"), f.requests.cmds())
        val after = vault.store.find(machine.deviceId)!!
        assertNull(after.companion)
        assertFalse(after.companionForgetOwed)
        assertFalse(vault.store.settings.companionOn)
        assertTrue(machine.deviceId in vault.dropped)
        assertFalse(vault.boxes.containsKey(machine.deviceId))
        assertFalse(CompanionKeys.shouldStart(vault.store))
        l.close()
    }

    @Test
    fun `turning off while away owes a forget, sent on the next full connect`() {
        val vault = Vault(MachineStore().with(machine))
        val f = fake()
        val l = link(f)
        val keys = CompanionKeys(vault)
        keys.turnOn(mapOf(machine.deviceId to (l to hello)))
        keys.turnOff(emptyMap())
        val owed = vault.store.find(machine.deviceId)!!
        assertNull(owed.companion, "the key goes at once, connected or not")
        assertTrue(owed.companionForgetOwed)
        assertTrue(machine.deviceId in vault.dropped)

        keys.sync(machine.deviceId, l, hello)
        assertEquals(listOf("companion_register", "companion_forget"), f.requests.cmds())
        assertFalse(vault.store.find(machine.deviceId)!!.companionForgetOwed)
        // And once paid, it is not paid again.
        keys.sync(machine.deviceId, l, hello)
        assertEquals(2, f.requests.size)
        l.close()
    }

    @Test
    fun `turning back on while away replaces the old key on the next connect`() {
        val vault = Vault(MachineStore().with(machine))
        val f = fake()
        val l = link(f)
        val keys = CompanionKeys(vault)
        keys.turnOn(mapOf(machine.deviceId to (l to hello)))
        keys.turnOff(emptyMap())
        keys.turnOn(emptyMap())
        keys.sync(machine.deviceId, l, hello)
        val sent = f.requests.toList()
        assertEquals(listOf("companion_register", "companion_register"), f.requests.cmds())
        assertNotEquals(registeredKey(sent[0]), registeredKey(sent[1]))
        val after = vault.store.find(machine.deviceId)!!
        assertFalse(after.companionForgetOwed, "registering a new key retires the old one at the machine")
        assertTrue(after.companion!!.registered)
        l.close()
    }

    @Test
    fun `unpairing forgets at the machine and deletes the key here`() {
        val vault = Vault(MachineStore().with(machine))
        val f = fake()
        val l = link(f)
        val keys = CompanionKeys(vault)
        keys.turnOn(mapOf(machine.deviceId to (l to hello)))
        keys.unpair(machine.deviceId, l)
        assertEquals(listOf("companion_register", "companion_forget"), f.requests.cmds())
        assertFalse(vault.boxes.containsKey(machine.deviceId))
        l.close()
    }

    @Test
    fun `nothing is registered over a companion link or with a machine that lacks the feature`() {
        val vault = Vault(MachineStore().with(machine).let { it.copy(settings = it.settings.copy(companionOn = true)) })
        val f = fake()
        val companionLink = link(f, Scope.COMPANION)
        val keys = CompanionKeys(vault)
        keys.sync(machine.deviceId, companionLink, hello)
        val full = link(f)
        keys.sync(machine.deviceId, full, RemoteHello(version = 1, features = listOf("phone_report")))
        assertEquals(0, f.requests.size)
        assertEquals(0, companionLink.refused, "it never even tried")
        assertNull(vault.store.find(machine.deviceId)!!.companion)
        companionLink.close()
        full.close()
    }

    @Test
    fun `the stored record never holds the companion secret`() {
        val vault = Vault(MachineStore().with(machine))
        val generated = InMemoryStaticKey.generate()
        val secret = generated.exportSecretForSealing()
        val f = fake()
        val l = link(f)
        CompanionKeys(vault) { generated }.turnOn(mapOf(machine.deviceId to (l to hello)))
        val text = vault.store.encode()
        assertFalse(text.contains(Base64Url.encode(secret)))
        assertArrayEquals(
            generated.publicKey,
            Base64Url.decode(vault.store.find(machine.deviceId)!!.companion!!.publicKey),
        )
        l.close()
    }
}
