package com.rimeos.remote.link

import com.rimeos.remote.core.CompanionKey
import com.rimeos.remote.core.MachineStore
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Settings
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The boot receiver's decision, which is also what a lock checks before
 * dropping to companion mode instead of ending the link: the switch AND a
 * registered companion key, or nothing starts.
 */
class BootStartTest {
    private fun machine(id: String, key: CompanionKey?) = PairedMachine(
        deviceId = id,
        machine = "machine-$id",
        desktopKey = "AAAA",
        deviceKey = "BBBB",
        sealed = "CCCC",
        pairedMs = 1,
        companion = key,
    )

    private val registered = CompanionKey(publicKey = "pub", sealed = "sealed", registered = true)
    private val unregistered = registered.copy(registered = false)

    private fun store(on: Boolean, vararg machines: PairedMachine) =
        MachineStore(machines = machines.toList(), settings = Settings(companionOn = on))

    @Test
    fun `the switch on and a registered key start it`() {
        assertTrue(BootStart.shouldStart(store(true, machine("a", registered))))
        assertTrue(BootStart.shouldStart(store(true, machine("a", null), machine("b", registered))))
    }

    @Test
    fun `the switch off starts nothing, even with a key left behind`() {
        // A key with the switch off should not exist (turning it off deletes
        // them), but if one did, the switch still wins.
        assertFalse(BootStart.shouldStart(store(false, machine("a", registered))))
    }

    @Test
    fun `no registered key starts nothing`() {
        assertFalse(BootStart.shouldStart(store(true)))
        assertFalse(BootStart.shouldStart(store(true, machine("a", null))))
        assertFalse(BootStart.shouldStart(store(true, machine("a", unregistered))))
    }

    @Test
    fun `the switch is off by default`() {
        assertFalse(Settings().companionOn)
        assertFalse(BootStart.shouldStart(MachineStore(machines = listOf(machine("a", registered)))))
    }

    @Test
    fun `the notification says locked in companion mode, and never names an agent`() {
        val text = LinkService.text(listOf("desk"), connected = true, companion = true)
        assertTrue(text.startsWith("Locked."), text)
        assertTrue(text.contains("desk"), text)
        assertFalse(LinkService.text(listOf("desk"), connected = true).startsWith("Locked"))
    }
}
