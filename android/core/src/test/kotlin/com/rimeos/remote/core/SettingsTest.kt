package com.rimeos.remote.core

import com.rimeos.remote.core.term.AccessoryKey
import com.rimeos.remote.core.term.AccessoryKeys
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path

/**
 * P1-055: "…and a **configurable** accessory row."
 *
 * The configuration is a field on [MachineStore] rather than a preferences
 * file, and that is not tidiness: `AppStorage` being the only writer to app
 * storage is what lets `InsecureStorageTest` walk the whole directory and
 * account for every byte in it. A `DataStore` for the accessory row would be a
 * second write path, and one the walk never looks at —
 * `tools/no-second-write-path.sh` fails the build if one appears.
 *
 * So these are the tests that the row survives a save, and that an empty one
 * means "whatever ships" rather than "no keys at all".
 */
class SettingsTest {
    @Test
    fun `an accessory row survives a save and a reload`(@TempDir dir: Path) {
        val storage = AppStorage(dir.toFile())
        val mine = listOf(
            AccessoryKey("Esc", "key:ESCAPE"),
            AccessoryKey(":", "text::"),
            AccessoryKey("Ctrl", "ctrl"),
        )
        storage.save(MachineStore(settings = Settings(accessory = mine, terminalTextSp = 15f)))
        val back = storage.load()
        assertEquals(mine, back.settings.accessory)
        assertEquals(15f, back.settings.terminalTextSp)
        assertEquals(mine, back.settings.accessoryRow)
    }

    @Test
    fun `an empty row means what this build ships, not a row with no keys`() {
        // A stored empty list and an absent key are indistinguishable after a
        // kotlinx.serialization default, so the two must mean the same thing —
        // and the safe meaning is "the shipped row". A phone whose accessory
        // row vanished because a migration dropped a field is a phone you
        // cannot press Escape on.
        assertTrue(Settings().accessory.isEmpty())
        assertEquals(AccessoryKeys.DEFAULT, Settings().accessoryRow)
        assertTrue(Settings().accessoryRow.isNotEmpty())
    }

    @Test
    fun `a store written by an older build still opens, with the shipped row`(@TempDir dir: Path) {
        // The migration that will actually happen: a device that paired before
        // P1-055 has a settings object with neither key in it.
        val file = dir.resolve("machines.json").toFile()
        file.parentFile.mkdirs()
        file.writeText("""{"v":1,"machines":[],"settings":{"dynamic_colour":true}}""")
        val store = AppStorage(dir.toFile()).load()
        assertTrue(store.settings.dynamicColour)
        assertEquals(AccessoryKeys.DEFAULT, store.settings.accessoryRow)
        assertEquals(Settings.DEFAULT_TERMINAL_TEXT_SP, store.settings.terminalTextSp)
    }

    @Test
    fun `every key in the shipped row is one this build can act on`() {
        for (key in Settings().accessoryRow) {
            assertTrue(
                AccessoryKeys.parse(key.action) != null,
                "the shipped row has `${key.label}` with an action this build cannot parse: ${key.action}",
            )
            assertFalse(key.label.isEmpty(), "a button with no label is a button nobody can aim at")
        }
    }

    @Test
    fun `the text size is a number of columns, not a matter of taste`() {
        // Documented as measured: eighty columns at 0.6 em advance is 576 px,
        // inside the short edge of every phone this supports. The default is
        // pinned so a change to it is a change somebody made on purpose.
        assertEquals(12f, Settings.DEFAULT_TERMINAL_TEXT_SP)
    }

    @Test
    fun `media on the computer is off, with no apps, until chosen, and survives a save`(@TempDir dir: Path) {
        // Contract §2.8: nothing is sent until the switch is on and an app is
        // chosen, so both defaults are the point. A store from before the
        // feature has neither key and must read as off.
        assertFalse(Settings().mediaOn)
        assertTrue(Settings().mediaApps.isEmpty())
        val file = dir.resolve("machines.json").toFile()
        file.parentFile.mkdirs()
        file.writeText("""{"v":1,"machines":[],"settings":{"mirror_apps":["org.example.chat"]}}""")
        val old = AppStorage(dir.toFile()).load().settings
        assertFalse(old.mediaOn)
        assertTrue(old.mediaApps.isEmpty(), "the notification apps leaked into the media apps")

        val storage = AppStorage(dir.toFile())
        storage.save(MachineStore(settings = Settings(mirrorApps = setOf("a"), mediaOn = true, mediaApps = setOf("b"))))
        val back = storage.load().settings
        assertTrue(back.mediaOn)
        assertEquals(setOf("b"), back.mediaApps)
        assertEquals(setOf("a"), back.mirrorApps)
    }

    @Test
    fun `control from the computer is on by default, also for a store from before it, and off survives a save`(@TempDir dir: Path) {
        assertTrue(Settings().mediaControl)
        val file = dir.resolve("machines.json").toFile()
        file.parentFile.mkdirs()
        file.writeText("""{"v":1,"machines":[],"settings":{"media_on":true,"media_apps":["org.example.music"]}}""")
        assertTrue(AppStorage(dir.toFile()).load().settings.mediaControl, "a store from before the switch reads as on")
        val storage = AppStorage(dir.toFile())
        storage.save(MachineStore(settings = Settings(mediaOn = true, mediaControl = false)))
        val back = storage.load().settings
        assertFalse(back.mediaControl)
        assertTrue(back.mediaOn)
        assertTrue(file.readText().contains("\"media_control\":false"), file.readText())
    }

    @Test
    fun `the touchpad scrolls naturally at normal speed by default, also for a store from before it`(@TempDir dir: Path) {
        assertTrue(Settings().touchpadNatural)
        assertEquals(1f, Settings().touchpadSpeed)
        val file = dir.resolve("machines.json").toFile()
        file.parentFile.mkdirs()
        file.writeText("""{"v":1,"machines":[],"settings":{"media_on":true}}""")
        assertTrue(AppStorage(dir.toFile()).load().settings.touchpadNatural)
        val storage = AppStorage(dir.toFile())
        storage.save(MachineStore(settings = Settings(touchpadNatural = false, touchpadSpeed = 1.5f)))
        val back = storage.load().settings
        assertFalse(back.touchpadNatural)
        assertEquals(1.5f, back.touchpadSpeed)
    }

    @Test
    fun `Do Not Disturb following the computer is off until turned on, and survives a save`(@TempDir dir: Path) {
        // Contract §2.9: a computer changes nothing on the phone until the
        // person says so. A store from before the feature reads as off.
        assertFalse(Settings().focusOn)
        val file = dir.resolve("machines.json").toFile()
        file.parentFile.mkdirs()
        file.writeText("""{"v":1,"machines":[],"settings":{"mirror_reply":true}}""")
        assertFalse(AppStorage(dir.toFile()).load().settings.focusOn)

        val storage = AppStorage(dir.toFile())
        storage.save(MachineStore(settings = Settings(focusOn = true)))
        assertTrue(storage.load().settings.focusOn)
        assertTrue(file.readText().contains("\"focus_on\":true"), "the switch is not stored under its contract name")
    }

    @Test
    fun `getting the computer's clipboard is off until turned on, and survives a save`(@TempDir dir: Path) {
        // Contract §2.14: nothing lands on this phone's clipboard until the
        // person says so. A store from before the feature reads as off.
        assertFalse(Settings().clipboardOn)
        val file = dir.resolve("machines.json").toFile()
        file.parentFile.mkdirs()
        file.writeText("""{"v":1,"machines":[],"settings":{"focus_on":true}}""")
        assertFalse(AppStorage(dir.toFile()).load().settings.clipboardOn)

        val storage = AppStorage(dir.toFile())
        storage.save(MachineStore(settings = Settings(clipboardOn = true)))
        assertTrue(storage.load().settings.clipboardOn)
        assertTrue(file.readText().contains("\"clipboard_on\":true"), "the switch is not stored under its contract name")
    }
}
