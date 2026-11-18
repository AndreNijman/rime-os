package com.rimeos.remote.core.phone

import com.rimeos.remote.core.Base64Url
import com.rimeos.remote.core.CompanionKey
import com.rimeos.remote.core.DeviceException
import com.rimeos.remote.core.InMemoryStaticKey
import com.rimeos.remote.core.MachineStore
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.SecretBox
import com.rimeos.remote.core.StaticKey
import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.agent.RemoteHello

/**
 * Where the companion keys live, as far as their lifecycle needs to know.
 *
 * An interface so the lifecycle below is the same code on a phone (the
 * repository and the platform keystore) and in a JVM test (a map).
 */
interface CompanionVault {
    fun load(): MachineStore

    /** Apply [change] to the stored record and return what was written. */
    fun update(change: (MachineStore) -> MachineStore): MachineStore

    /**
     * The box that seals this machine's companion key: one that opens with
     * nobody present, and never the device key's box (the alias differs).
     */
    fun box(deviceId: String): SecretBox

    /** Delete that box's key, so a sealed companion key left anywhere is dead. */
    fun drop(deviceId: String)
}

/**
 * The companion key's lifecycle: minted when the switch goes on, registered
 * over a FULL session, re-registered on every full connect while the switch
 * stays on, forgotten and deleted when it goes off or the machine is unpaired.
 *
 * ## Persist, then send
 *
 * A key is sealed and written BEFORE `companion_register` goes out, and
 * marked registered only after the machine said ok. A phone that died between
 * the two has a key the machine may or may not hold; the next full connect
 * sends it again, and the machine answers ok to a key it already has. A phone
 * that registered first and crashed before writing would have a key on the
 * machine and none here, which nothing could clean up.
 *
 * ## A key the machine will not take is replaced, once
 *
 * `key_in_use` means the machine retired this key (the switch went off and on
 * while the machine was away) or another device holds it. Either way a new key
 * is minted and registered once; a second refusal is left for the next
 * connect rather than looping.
 */
class CompanionKeys(
    private val vault: CompanionVault,
    private val mint: () -> InMemoryStaticKey = InMemoryStaticKey::generate,
) {
    /**
     * Bring [deviceId]'s companion key in line with the switch, over [link].
     *
     * Only for a FULL link whose [hello] lists [CompanionLink.FEATURE]; any
     * other link is left alone, because a companion session may neither
     * register nor forget (and the gate would refuse it anyway). Throws what
     * the link throws when the connection drops; the caller tries again on the
     * next connect.
     */
    fun sync(deviceId: String, link: MachineLink, hello: RemoteHello) {
        if (link.scope != Scope.FULL || !hello.has(CompanionLink.FEATURE)) return
        val store = vault.load()
        val machine = store.find(deviceId) ?: return
        if (!store.settings.companionOn) {
            if (machine.companion != null || machine.companionForgetOwed) forget(deviceId, link)
            return
        }
        val key = machine.companion ?: fresh(deviceId)
        try {
            register(deviceId, link, key)
        } catch (e: AgentError) {
            if (e.kind != CompanionLink.KEY_IN_USE) throw e
            replace(deviceId)
            register(deviceId, link, fresh(deviceId))
        }
    }

    /**
     * The switch went off: every key is deleted here at once, and each machine
     * is owed a `companion_forget`, sent now over [connected] where it can be
     * and on the next full connect everywhere else.
     */
    fun turnOff(connected: Map<String, Pair<MachineLink, RemoteHello>>) {
        val dropped = vault.load().machines.filter { it.companion != null }.map { it.deviceId }
        vault.update { s ->
            s.copy(
                settings = s.settings.copy(companionOn = false),
                machines = s.machines.map {
                    if (it.companion == null) it else it.copy(companion = null, companionForgetOwed = true)
                },
            )
        }
        for (id in dropped) vault.drop(id)
        for ((id, pair) in connected) runCatching { sync(id, pair.first, pair.second) }
    }

    /** The switch went on: register with every machine connected now. */
    fun turnOn(connected: Map<String, Pair<MachineLink, RemoteHello>>) {
        vault.update { it.copy(settings = it.settings.copy(companionOn = true)) }
        for ((id, pair) in connected) runCatching { sync(id, pair.first, pair.second) }
    }

    /**
     * This machine is being unpaired: tell it to forget the companion key if
     * [link] can, then delete the key here. The record itself goes with the
     * pairing; this only makes sure the keystore key goes too.
     */
    fun unpair(deviceId: String, link: MachineLink?) {
        if (link != null && link.scope == Scope.FULL) runCatching { link.companionForget() }
        vault.drop(deviceId)
    }

    /**
     * The companion identity for [machine], unsealed, or null when it has no
     * registered key. Opens with nobody present, by design.
     */
    fun identity(machine: PairedMachine): StaticKey? {
        val key = machine.companion?.takeIf { it.registered } ?: return null
        val raw = Base64Url.decode(key.sealed)
            ?: throw DeviceException("the stored companion key for ${machine.machine} is not base64url")
        return InMemoryStaticKey(vault.box(machine.deviceId).open(raw))
    }

    private fun register(deviceId: String, link: MachineLink, key: CompanionKey) {
        val public = Base64Url.decode(key.publicKey)
            ?: throw DeviceException("the stored companion public key is not base64url")
        link.companionRegister(public)
        vault.update { s ->
            s.edit(deviceId) {
                if (it.companion?.publicKey == key.publicKey) {
                    it.copy(companion = key.copy(registered = true), companionForgetOwed = false)
                } else {
                    it
                }
            }
        }
    }

    private fun forget(deviceId: String, link: MachineLink) {
        link.companionForget()
        vault.update { s -> s.edit(deviceId) { it.copy(companion = null, companionForgetOwed = false) } }
        vault.drop(deviceId)
    }

    /** The current key was refused: delete it here before minting another. */
    private fun replace(deviceId: String) {
        vault.update { s -> s.edit(deviceId) { it.copy(companion = null) } }
        vault.drop(deviceId)
    }

    /**
     * Mint, seal and store a new key for [deviceId], unregistered.
     *
     * The second of the two places shipped code calls `exportSecretForSealing`
     * (the first is `MachineStore.record`), and for the same reason: the
     * secret's one copy goes straight into a box and is wiped.
     */
    private fun fresh(deviceId: String): CompanionKey {
        val identity = mint()
        val secret = identity.exportSecretForSealing()
        val sealed = try {
            vault.box(deviceId).seal(secret)
        } finally {
            secret.fill(0)
        }
        val key = CompanionKey(publicKey = identity.publicKeyText(), sealed = Base64Url.encode(sealed))
        vault.update { s -> s.edit(deviceId) { it.copy(companion = key) } }
        return key
    }

    companion object {
        /** Whether a restarted phone should start the companion link at all. */
        fun shouldStart(store: MachineStore): Boolean =
            CompanionLink.shouldStartAtBoot(store.settings.companionOn, store.companions().size)
    }
}
