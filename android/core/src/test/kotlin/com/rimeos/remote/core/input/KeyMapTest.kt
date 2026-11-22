package com.rimeos.remote.core.input

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class KeyMapTest {
    @Test
    fun `android keys map to the evdev codes a keyboard on the computer sends`() {
        val cases = mapOf(
            29 to 30, // A
            54 to 44, // Z
            7 to 11, // 0
            8 to 2, // 1
            16 to 10, // 9
            62 to 57, // space
            66 to 28, // enter
            67 to 14, // backspace
            61 to 15, // tab
            111 to 1, // escape
            112 to 111, // forward delete
            113 to 29, // left ctrl
            59 to 42, // left shift
            57 to 56, // left alt
            58 to 100, // right alt
            117 to 125, // meta
            19 to 103, 20 to 108, 21 to 105, 22 to 106, // arrows
            122 to 102, 123 to 107, 92 to 104, 93 to 109, // home end pgup pgdn
            131 to 59, 140 to 68, 141 to 87, 142 to 88, // F1 F10 F11 F12
            144 to 82, 153 to 73, 160 to 96, // numpad 0, 9, enter
            68 to 41, 76 to 53, 73 to 43, // grave slash backslash
        )
        for ((android, evdev) in cases) assertEquals(evdev, KeyMap.android(android), "android $android")
    }

    @Test
    fun `keys the phone keeps for itself map to nothing`() {
        // Back, Home, volume up/down, power, media play/pause, camera, mute, menu.
        for (k in listOf(4, 3, 24, 25, 26, 85, 27, 164, 82, 0, 9999)) assertNull(KeyMap.android(k), "$k")
    }

    @Test
    fun `no key maps to one the computer forbids`() {
        for (k in 0..400) {
            val e = KeyMap.android(k) ?: continue
            assertTrue(Input.keyAllowed(e), "android $k -> $e")
        }
        for (e in KeyMap.F) assertTrue(Input.keyAllowed(e))
        for (m in Mod.entries) assertTrue(Input.keyAllowed(KeyMap.of(m)))
    }

    @Test
    fun `characters map to their key on a US layout, with shift when needed`() {
        assertEquals(KeyMap.Typed(46, false), KeyMap.char('c'))
        assertEquals(KeyMap.Typed(46, true), KeyMap.char('C'))
        assertEquals(KeyMap.Typed(3, true), KeyMap.char('@'))
        assertEquals(KeyMap.Typed(53, true), KeyMap.char('?'))
        assertEquals(KeyMap.Typed(41, false), KeyMap.char('`'))
        assertEquals(KeyMap.Typed(57, false), KeyMap.char(' '))
        assertNull(KeyMap.char('é'))
        for (c in ('a'..'z') + ('0'..'9')) assertEquals(false, KeyMap.char(c)?.shift, "$c")
    }

    @Test
    fun `modifiers press the keys the computer reads as them`() {
        assertEquals(29, KeyMap.of(Mod.CTRL))
        assertEquals(42, KeyMap.of(Mod.SHIFT))
        assertEquals(56, KeyMap.of(Mod.ALT))
        assertEquals(125, KeyMap.of(Mod.SUPER))
        assertEquals(100, KeyMap.of(Mod.ALTGR))
    }
}
