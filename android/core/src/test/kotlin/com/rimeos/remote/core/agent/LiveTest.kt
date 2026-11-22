package com.rimeos.remote.core.agent

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

/**
 * The remote-live change set's pure rules: how a reply is submitted, what a
 * session is called, and what a peek shows.
 */
class LiveTest {

    // ---- a reply that submits -------------------------------------------

    @Test
    fun `with input_submit a reply is one request and the daemon presses Return`() {
        val plan = Reply.plan(3, "yes please\n", submitSupported = true)
        assertEquals(
            listOf(Reply.Step.Send("""{"cmd":"input","id":3,"data":"yes please","submit":true}""")),
            plan,
        )
    }

    @Test
    fun `without it the Return is a second request a hundred milliseconds later`() {
        val plan = Reply.plan(3, "a long answer  \r\n", submitSupported = false)
        assertEquals(
            listOf(
                Reply.Step.Send("""{"cmd":"input","id":3,"data":"a long answer"}"""),
                Reply.Step.Wait(100),
                Reply.Step.Send("""{"cmd":"input","id":3,"data":"\r"}"""),
            ),
            plan,
        )
        // The text half never carries a terminator. A CR there is the paste
        // failure again, followed by a second Return.
        val first = (plan.first() as Reply.Step.Send).line
        assertFalse(first.contains("\\r") || first.contains("\\n"), first)
        assertEquals(100, Reply.FALLBACK_GAP_MS)
    }

    @Test
    fun `a bare return is a single CR against either daemon`() {
        val bare = listOf(Reply.Step.Send("""{"cmd":"input","id":3,"data":"\r"}"""))
        assertEquals(bare, Reply.plan(3, "", submitSupported = true))
        assertEquals(bare, Reply.plan(3, "  \n", submitSupported = false))
        // And it is still what `bytes` produced, so a bare return did not change.
        assertEquals("\r", Reply.bytes(""))
    }

    @Test
    fun `interior line breaks are not the plan's to change`() {
        // Handoff.Clipboard decides what a multi-line box sends before it gets
        // here; the plan only trims the end.
        val plan = Reply.plan(1, "one\ntwo", submitSupported = true)
        assertEquals(
            """{"cmd":"input","id":1,"data":"one\ntwo","submit":true}""",
            (plan.single() as Reply.Step.Send).line,
        )
        assertEquals("one\ntwo", Reply.text("one\ntwo\n"))
    }

    // ---- names ------------------------------------------------------------

    private fun session(name: String? = null, title: String? = null, cwd: String = "/home/andre/Projects/rime") =
        AgentSession(id = 3, agent = "claude", cwd = cwd, name = name, title = title)

    @Test
    fun `a session is called its name, else its title, else its label`() {
        assertEquals("auth refactor", session(name = "auth refactor", title = "Claude Code").displayName)
        assertEquals("Claude Code", session(title = "Claude Code").displayName)
        assertEquals("Claude · rime", session().displayName)
        assertEquals("Claude · rime", session().label)
        assertTrue(session(name = "x").isNamed)
        assertFalse(session().isNamed)
    }

    @Test
    fun `an empty or invisible name falls through rather than drawing nothing`() {
        // The daemon never sends one, and a screen that trusted that would
        // draw a blank row the day a daemon did.
        assertEquals("Claude · rime", session(name = "", title = "  ").displayName)
        assertEquals("Claude Code", session(name = "\u0007‮", title = "Claude Code").displayName)
    }

    @Test
    fun `the label uses the project name, then the directory's last part`() {
        assertEquals("Claude · rime-os", session().copy(projectName = "rime-os").label)
        assertEquals("Claude · wt-x", session(cwd = "/var/tmp/wt-x/").label)
        assertEquals("Claude", session(cwd = "").label)
    }

    @Test
    fun `a title is cleaned again before it is drawn`() {
        // Right-to-left override, a bell and a newline: an agent chooses its
        // title, and one that could reorder the text drawn after it could make
        // a row say something else.
        val shown = session(title = "fix‮ gnirts\u0007\nbug").displayName
        assertEquals("fix gnirtsbug", shown)
        val long = "x".repeat(200)
        assertEquals(SessionNames.MAX_TITLE_CHARS, session(title = long).displayName.length)
    }

    @Test
    fun `names are checked by the daemon's rule, counted in characters`() {
        assertEquals(SessionNames.Checked.Ok("auth refactor"), SessionNames.check("  auth refactor \n"))
        assertEquals(SessionNames.Checked.Ok(null), SessionNames.check("   "), "empty clears")
        // Sixty-four emoji are 128 UTF-16 units and 64 characters. Rust counts
        // characters; counting Kotlin's way would refuse what the machine takes.
        val emoji = "🚀".repeat(64)
        assertEquals(128, emoji.length)
        assertEquals(SessionNames.Checked.Ok(emoji), SessionNames.check(emoji))
        assertTrue(SessionNames.check("🚀".repeat(65)) is SessionNames.Checked.Refused)
        assertTrue(SessionNames.check("a".repeat(65)) is SessionNames.Checked.Refused)
        assertEquals(SessionNames.Checked.Ok("a".repeat(64)), SessionNames.check("a".repeat(64)))
        // Refused, not sanitised.
        for (bad in listOf("tab\there", "line\nbreak", "bell\u0007", "c1\u0085")) {
            val r = SessionNames.check(bad)
            assertTrue(r is SessionNames.Checked.Refused, "$bad was accepted")
        }
        assertEquals(64, SessionNames.MAX_CHARS)
    }

    @Test
    fun `a session reply carries its name and title, and an old one carries neither`() {
        val named = Agentd.readSession(
            """{"reply":"session","id":3,"agent":"claude","cwd":"/x","state":"working",""" +
                """"name":"auth refactor","title":"Claude Code"}""",
        )
        assertEquals("auth refactor", named.name)
        assertEquals("Claude Code", named.title)
        val cleared = Agentd.readSession("""{"reply":"session","id":3,"name":null,"title":null}""")
        assertNull(cleared.name)
        val old = Agentd.readSession("""{"reply":"session","id":3}""")
        assertNull(old.name)
        assertNull(old.title)
    }

    // ---- features --------------------------------------------------------

    @Test
    fun `features are what hello lists, and nothing when it lists nothing`() {
        val new = Agentd.readHello(
            """{"reply":"hello","version":11,"agents":["claude"],"default_agent":"claude",""" +
                """"features":["input_submit","rename","peek","session_name","session_title","size_restore"]}""",
        )
        assertTrue(new.has(Features.INPUT_SUBMIT))
        assertTrue(new.has(Features.RENAME))
        assertTrue(new.has(Features.PEEK))
        val old = Agentd.readHello("""{"reply":"hello","version":10,"agents":["claude"]}""")
        assertFalse(old.has(Features.INPUT_SUBMIT), "an old daemon was credited with a feature")
        assertTrue(old.features.isEmpty())
    }

    @Test
    fun `remote_hello's answer, and an old rime-remoted's refusal`() {
        val r = Agentd.readRemoteHello(
            """{"reply":"remote","version":1,"features":["nodelay","control_worker","mux_attach","liveness"],""" +
                """"lan":["192.168.1.232:7717"]}""",
        )
        assertTrue(r.has(Features.MUX_ATTACH))
        assertEquals(listOf("192.168.1.232:7717"), r.lanHints())
        assertEquals(null, r.input, "a computer without the touchpad says nothing about it")
        val touchpad = Agentd.readRemoteHello(
            """{"reply":"remote","version":1,"features":["mux_attach","phone_input"],"input":"off"}""",
        )
        assertTrue(touchpad.has(com.rimeos.remote.core.input.Input.FEATURE))
        assertEquals("off", touchpad.input)
        // What an old rime-remoted produces by forwarding the verb to agentd.
        val e = assertThrows<AgentError> {
            Agentd.readRemoteHello(
                """{"reply":"error","kind":"bad_request","message":"unparseable request: unknown variant `remote_hello`"}""",
            )
        }
        assertEquals("bad_request", e.kind)
        assertFalse(RemoteHello.NONE.has(Features.MUX_ATTACH))
    }

    @Test
    fun `stored addresses are filtered, deduplicated and capped`() {
        val r = RemoteHello(
            lan = listOf(" 192.168.1.232:7717 ", "192.168.1.232:7717", "", "bad address", "a\nb") +
                (1..20).map { "10.0.0.$it:7717" },
        )
        val hints = r.lanHints()
        assertEquals("192.168.1.232:7717", hints.first())
        assertEquals(RemoteHello.MAX_ADDRESSES, hints.size)
        assertFalse(hints.any { it.isBlank() || it.contains(' ') || it.contains('\n') })
    }

    // ---- peek ---------------------------------------------------------------

    private fun peek(text: String, cols: Int = 40, rows: Int = 10, requested: Int = 8192) =
        Peek(3, text.toByteArray(Charsets.UTF_8), cols, rows, "working", requested)

    @Test
    fun `a peek reply decodes its base64`() {
        val data = java.util.Base64.getEncoder().encodeToString("hello\r\nworld".toByteArray())
        val p = Agentd.readPeek(
            """{"reply":"peek","id":3,"data":"$data","cols":120,"rows":40,"state":"working"}""",
        )
        assertEquals("hello\r\nworld", String(p.data))
        assertEquals(120, p.cols)
        assertEquals(40, p.rows)
        assertEquals("working", p.state)
        assertThrows<AgentError> {
            Agentd.readPeek("""{"reply":"peek","id":3,"data":"!!!not base64","cols":80,"rows":24}""")
        }
    }

    @Test
    fun `the bottom lines are what the screen shows, not what the bytes say`() {
        // A spinner redrawn in place with CR, the way an agent's UI does it.
        val p = peek("building\r\n\u001b[32m✻ Thinking\u001b[0m\r✻ Done    \r\n> ")
        assertEquals(listOf("building", "✻ Done", ">"), LiveTail.lines(p))
        assertEquals(">", LiveTail.lastLine(p))
    }

    @Test
    fun `trailing blank rows go, blank rows between text stay, and only the last N are kept`() {
        val many = (1..30).joinToString("\r\n") { "line $it" }
        val lines = LiveTail.lines(peek(many, rows = 40), max = LiveTail.LINES)
        assertEquals(15, lines.size)
        assertEquals("line 30", lines.last())
        assertEquals("line 16", lines.first())
        assertEquals(listOf("a", "", "b"), LiveTail.lines(peek("a\r\n\r\nb\r\n\r\n\r\n")))
    }

    @Test
    fun `a tail that starts mid-escape or mid-character does not print the fragment`() {
        // "[31;1m" is the rest of an SGR whose ESC was cut off by the ring;
        // the leading 0x9C is the tail of a torn three-byte character.
        val torn = byteArrayOf(0x9C.toByte()) + "31;1mnoise\r\nreal output\r\n".toByteArray()
        val p = Peek(3, torn, 40, 10, "working", requested = torn.size)
        assertTrue(p.truncated)
        assertEquals(listOf("real output"), LiveTail.lines(p))
        // The same bytes as a WHOLE output (shorter than asked for) are kept:
        // nothing was cut, so the first line is real.
        val whole = Peek(3, "first\r\nsecond".toByteArray(), 40, 10, "working", requested = 8192)
        assertFalse(whole.truncated)
        assertEquals(listOf("first", "second"), LiveTail.lines(whole))
    }

    @Test
    fun `an agent's input box border is not its last line`() {
        val p = peek("the answer is 42\r\n╭────────╮\r\n│ >      │\r\n╰────────╯\r\n")
        assertEquals("│ >      │", LiveTail.lastLine(p))
        val borderOnly = peek("hello\r\n────────\r\n")
        assertEquals("hello", LiveTail.lastLine(borderOnly))
        assertNull(LiveTail.lastLine(peek("")))
    }

    @Test
    fun `a peek never answers the program`() {
        // A cursor-position request in the tail. On an attached terminal the
        // answer is written back to the PTY; a preview must write nothing,
        // and there is nothing here that could — this asserts the replay
        // survives the request and shows the text around it.
        val p = peek("before\u001b[6nafter")
        assertEquals(listOf("beforeafter"), LiveTail.lines(p))
    }

    @Test
    fun `a size the daemon should never send is bounded, not allocated`() {
        val p = Peek(3, "ok".toByteArray(), Int.MAX_VALUE, -5, "working", 8192)
        assertEquals(listOf("ok"), LiveTail.lines(p))
    }
}
