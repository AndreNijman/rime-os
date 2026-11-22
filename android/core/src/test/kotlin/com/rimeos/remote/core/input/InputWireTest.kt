package com.rimeos.remote.core.input

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

/** Frames as the computer reads them, for the tests. */
internal object Frames {
    fun seq(frame: String): Long = Json.parseToJsonElement(frame).jsonObject["seq"]!!.jsonPrimitive.long

    fun events(frame: String): List<JsonObject> =
        Json.parseToJsonElement(frame).jsonObject["ev"]!!.jsonArray.map { it.jsonObject }

    fun t(e: JsonObject): String = e["t"]!!.jsonPrimitive.content
}

class InputWireTest {
    @Test
    fun `events are written as the contract's table says`() {
        assertEquals("""{"t":"m","dx":1.5,"dy":-2.0}""", InputEvent.Motion(1.5, -2.0).json().toString())
        assertEquals("""{"t":"b","b":"right","p":true}""", InputEvent.Button(MouseButton.RIGHT, true).json().toString())
        assertEquals(
            """{"t":"s","dx":0.0,"dy":-40.0,"src":"finger","stop":true}""",
            InputEvent.Scroll(0.0, -40.0, stop = true).json().toString(),
        )
        assertEquals("""{"t":"k","code":30,"p":false}""", InputEvent.Key(30, false).json().toString())
        assertEquals(
            """{"t":"tap","code":46,"mods":["ctrl","shift"]}""",
            InputEvent.Tap(46, setOf(Mod.SHIFT, Mod.CTRL)).json().toString(),
        )
        assertEquals("""{"t":"tap","code":1}""", InputEvent.Tap(1).json().toString())
        assertEquals("""{"t":"text","s":"hé\n"}""", InputEvent.Text("hé\n").json().toString())
        assertEquals("""{"t":"hold"}""", InputEvent.Hold.json().toString())
        assertEquals("""{"seq":7,"ev":[{"t":"hold"}]}""", Input.frame(7, listOf(InputEvent.Hold)))
    }

    @Test
    fun `a key the computer forbids cannot even be made`() {
        for (code in listOf(0, 116, 142, 143, 205, 256)) {
            assertFalse(Input.keyAllowed(code), "$code")
            assertThrows<IllegalArgumentException> { InputEvent.Key(code, true) }
            assertThrows<IllegalArgumentException> { InputEvent.Tap(code) }
        }
        assertTrue(Input.keyAllowed(1))
        assertTrue(Input.keyAllowed(255))
    }

    @Test
    fun `the reply to open is read`() {
        assertEquals(Opened.Ready, Input.readOpen("""{"reply":"input_ready"}""".toByteArray()))
        assertTrue(Input.accepted("""{"reply":"input_ready"}""".toByteArray()))
        assertFalse(Input.accepted("""{"reply":"error","kind":"locked","message":"x"}""".toByteArray()))
        val words = mapOf(
            "not_allowed" to Refusal.NOT_ALLOWED,
            "bad_request" to Refusal.BAD_REQUEST,
            "stopped" to Refusal.STOPPED,
            "unavailable" to Refusal.UNAVAILABLE,
            "busy" to Refusal.BUSY,
            "locked" to Refusal.LOCKED,
            "something new" to Refusal.OTHER,
        )
        for ((word, kind) in words) {
            val r = Input.readOpen("""{"reply":"error","kind":"$word","message":"m"}""".toByteArray())
            assertEquals(Opened.Refused(kind, "m"), r, word)
        }
        assertEquals(Opened.Refused(Refusal.OTHER, ""), Input.readOpen("not json".toByteArray()))
    }

    @Test
    fun `the computer's event lines are read`() {
        assertEquals(Notice.Paused("locked"), Input.readEvent("""{"event":"input_paused","why":"locked"}"""))
        assertEquals(Notice.Resumed, Input.readEvent("""{"event":"input_resumed"}"""))
        assertEquals(Notice.Released("lease"), Input.readEvent("""{"event":"input_released","why":"lease"}"""))
        assertEquals(
            Notice.Closed(Refusal.STOPPED, "stopped at the computer"),
            Input.readEvent("""{"event":"input_closed","why":"stopped","message":"stopped at the computer"}"""),
        )
        assertEquals(
            Notice.Closed(Refusal.REFUSED, "more than 250 frames in a second"),
            Input.readEvent("""{"event":"input_closed","why":"refused","message":"more than 250 frames in a second"}"""),
        )
        assertEquals(Notice.Closed(Refusal.STOPPING, ""), Input.readEvent("""{"event":"input_closed","why":"stopping"}"""))
        assertNull(Input.readEvent("""{"event":"later"}"""))
        assertNull(Input.readEvent("garbage"))
    }

    @Test
    fun `the cool-down and the name are read from the computer's own words`() {
        val m = "this touchpad was stopped at the computer; try again in 27 s, or allow it there at once with " +
            "`rime remote devices grant Pixel 8 input on`"
        assertEquals(27, Input.secondsLeft(m))
        assertNull(Input.secondsLeft("stopped at the computer"))
        assertEquals("Pixel 8", Input.nameInRefusal(m))
        val n = "this phone may not use this computer as a touchpad and keyboard; at the computer run " +
            "`rime remote devices grant sdk_gphone64_x86_64 input on`"
        assertEquals("sdk_gphone64_x86_64", Input.nameInRefusal(n))
        assertNull(Input.nameInRefusal("a touchpad needs the phone's own key; unlock the phone to use it"))
    }

    @Test
    fun `the command quotes a name a shell would split`() {
        assertEquals("rime remote devices grant pixel-8 input on", Input.grantCommand("pixel-8"))
        assertEquals("rime remote devices grant 'Pixel 8' input on", Input.grantCommand("Pixel 8"))
        assertEquals("rime remote devices grant 'Andre'\\''s phone' input on", Input.grantCommand("Andre's phone"))
        assertEquals("rime remote devices grant '\$HOME' input on", Input.grantCommand("\$HOME"))
        assertEquals("rime remote devices grant 'Téléphone' input on", Input.grantCommand("Téléphone"))
    }
}
