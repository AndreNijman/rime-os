package com.rimeos.remote.core.agent

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Every request this app builds, against a fixture the daemon's serde parses.
 *
 * This half asserts the builders produce the fixture. The other half —
 * `rimed/rime-agent-core/tests/android_requests_wire.rs` — deserializes the
 * same file into `Request` and asserts which variant each becomes. Neither
 * half alone is worth much: a Kotlin test comparing a builder to a string a
 * Kotlin author wrote proves the two agree with each other, and that is
 * exactly the failure this unit has already had twice. The fixture being
 * `Request`'s own input is what makes it evidence.
 *
 * Compared as parsed JSON rather than as text: key order and spacing are not
 * the contract, and a comparison that failed on reformatting would be
 * retrained away the first time somebody tidied the file.
 */
class AgentdRequestWireTest {
    private val json = Json { ignoreUnknownKeys = false }

    private val fixture: JsonObject by lazy {
        val stream = javaClass.classLoader.getResourceAsStream("requests.json")
            ?: error("the shared request fixture is missing from the test resources")
        json.parseToJsonElement(stream.bufferedReader().readText()).jsonObject
    }

    private fun expect(name: String, built: String) {
        val want = fixture[name] ?: error("the fixture has no request called `$name`")
        assertEquals(
            want,
            json.parseToJsonElement(built),
            "`$name` is not the request the daemon's own fixture says it is",
        )
        // A request travels as ONE line. `rime-remote-core`'s wire refuses an
        // embedded newline in both directions, because a payload carrying its
        // own terminator would let a client put a second request in one frame.
        assertFalse(built.contains('\n'), "`$name` carries a literal newline")
    }

    @Test
    fun `the session verbs are the shapes the daemon parses`() {
        expect("hello", Agentd.hello())
        expect("list", Agentd.list())
        expect("info", Agentd.info(7))
        expect("attach", Agentd.attach(7, 80, 24))
        expect("resize", Agentd.resize(7, 100, 40))
        expect("signal", Agentd.interrupt(7))
    }

    @Test
    fun `clipboard reads the computer's clipboard and names no session`() {
        expect("clipboard", Agentd.clipboard())
        // The absence of an `id` is the assertion, not an accident of the
        // fixture. One Wayland seat has one clipboard, so a version of this
        // that carried a session id would be claiming a per-session clipboard
        // that does not exist — and would invite the UI to hide the action
        // behind a session, which is exactly where a user cannot reach it.
        val built = json.parseToJsonElement(Agentd.clipboard()).jsonObject
        assertEquals(setOf("cmd"), built.keys, "`clipboard` grew a field")

        // It is the OPPOSITE direction from `input`, which is the confusion
        // that kept this verb from being built for three rounds: `input`
        // carries the phone's clipboard to the computer, this carries the
        // computer's to the phone. Two requests, two directions, one criterion.
        assertFalse(
            Agentd.clipboard() == Agentd.input(7, "x"),
            "the send half and the receive half are not the same request",
        )
    }

    @Test
    fun `receive is the second takeover verb, and it never travels as a control frame`() {
        // It is in this fixture because the fixture is about what the daemon
        // PARSES, not about what goes on channel zero — `rime-remoted` refuses
        // it there by name, exactly as it refuses `attach` there.
        expect("receive", Agentd.receive(7, "shot.png", 204_800))
        // A name that is a path is sent as it came. The reduction is the
        // daemon's (`inject::safe_name`), and a phone that pre-reduced it
        // would be a second implementation of the rule — the case where the
        // two differ is the case where the user is told a filename that is
        // not the one on disk. `Handoff.Files.preview` shows the reduction
        // without performing it.
        expect("receive_hostile_name", Agentd.receive(7, "../../.ssh/authorized_keys", 5))
    }

    @Test
    fun `send to rime puts the phone's clipboard and files onto the computer itself`() {
        // Contract §1.8. `clipboard_write` is a control frame; `save_file` is
        // the third takeover verb, and like `receive` its name crosses as the
        // share sheet gave it: the daemon reduces and numbers it.
        expect("clipboard_write", Agentd.clipboardWrite("two\nlines \"quoted\""))
        expect("save_file", Agentd.saveFile("IMG_2041.jpg", 2_097_152))
        expect("save_file_hostile_name", Agentd.saveFile("../../.bashrc", 5))
    }

    @Test
    fun `input is a verb, and it is the shape write_input reads`() {
        // The reply carries its own CR: `session::write_input` writes raw bytes
        // and appends nothing, so a reply without one is never submitted.
        expect("input", Agentd.input(7, Reply.bytes("yes")))
    }


    @Test
    fun `a reply's Return is its own write, in every shape the phone sends`() {
        // The contract's own example, byte for byte (section 1.2). With
        // `submit` the daemon writes the CR itself, 80 ms after the text,
        // because a long burst that ENDS in CR is taken as a paste and the CR
        // becomes a newline in the agent's prompt instead of a submit.
        expect("input_submit", Agentd.input(3, "yes please", submit = true))
        // The older daemon's fallback: text alone, 100 ms on the phone, then
        // this. It is also what a bare return is, against either daemon.
        expect("input_bare", Agentd.input(3, "\r"))
        // And the plan that decides between them emits exactly these.
        assertEquals(
            listOf(Reply.Step.Send(Agentd.input(3, "yes please", submit = true))),
            Reply.plan(3, "yes please\n", submitSupported = true),
        )
        // `submit:false` is not a key an older daemon would accept, so it is
        // never written: false is the absence.
        assertFalse(Agentd.input(3, "x", submit = false).contains("submit"))
    }

    @Test
    fun `rename sends an explicit null to clear, the one builder that does`() {
        expect("rename", Agentd.rename(3, "auth refactor"))
        expect("rename_clear", Agentd.rename(3, null))
        // The mirror of `revoke`'s rule, asserted so nobody "tidies" one into
        // the other: there the absent key is the wide action, here the null
        // is the instruction and an absent key would say nothing at all.
        val cleared = json.parseToJsonElement(Agentd.rename(3, null)).jsonObject
        assertTrue("name" in cleared.keys, "a clear that omits the name says nothing")
        assertEquals(kotlinx.serialization.json.JsonNull, cleared["name"])
        // A name with a quote in it is escaped, not truncated.
        val quoted = json.parseToJsonElement(Agentd.rename(3, "say \"hi\"")).jsonObject
        assertEquals("say \"hi\"", quoted["name"]!!.jsonPrimitive.content)
    }

    @Test
    fun `peek asks for the tail by size, at the daemon's cap by default`() {
        expect("peek", Agentd.peek(3))
        assertEquals(8192, Agentd.PEEK_MAX)
    }

    @Test
    fun `remote_hello is rime-remoted's and so is not in the daemon's fixture`() {
        // The Rust half deserialises EVERY entry into agentd's `Request`, and
        // `remote_hello` is answered by rime-remoted before agentd ever sees
        // it — exactly like push_register, which is not here either.
        assertEquals("""{"cmd":"remote_hello"}""", Agentd.remoteHello())
        assertFalse(
            fixture.values.any { (it as? JsonObject)?.get("cmd")?.jsonPrimitive?.content == "remote_hello" },
            "remote_hello was put in the fixture agentd must parse",
        )
    }

    @Test
    fun `run omits what it has no value for, rather than sending nulls`() {
        // `RunRequest`'s optional fields are `#[serde(default)]`, and a key
        // holding null is not the same as an absent key to every one of them.
        expect("run_minimal", Agentd.run("/home/andre/Projects/rime", 80, 24))
    }

    @Test
    fun `run carries the checkpoint flag only when one was asked for`() {
        expect(
            "run_full",
            Agentd.run(
                cwd = "/home/andre/Projects/rime",
                cols = 80,
                rows = 24,
                agent = "claude",
                prompt = "review the diff",
                worktree = "wt-review",
                checkpoint = true,
            ),
        )
        // False is the daemon's default and the absence means the same thing —
        // but a daemon predating the field rejects the key outright, and this
        // app runs against whatever the machine has.
        assertFalse(
            Agentd.run("/x", 80, 24, checkpoint = false).contains("checkpoint"),
            "a request that wants no checkpoint must not mention one",
        )
    }

    @Test
    fun `the generic adapter gets the program it needs, and nobody else gets an empty list`() {
        // `RunRequest.args` — "extra arguments appended after the adapter's
        // own". Without it a phone could not start `generic` AT ALL, and
        // `generic` is in every runtime's `Hello.agents`. Measured against a
        // real daemon from a Pixel 7a: "the generic adapter needs a program to
        // run; pass one after `--`".
        expect(
            "run_generic_with_args",
            Agentd.run(
                cwd = "/home/andre",
                cols = 80,
                rows = 24,
                agent = "generic",
                args = listOf("/bin/cat", "-v"),
            ),
        )
        // Absent when empty, like every other optional key: `args` has a serde
        // default, and a daemon that predates the field rejects the key.
        assertFalse(
            Agentd.run("/x", 80, 24, agent = "claude").contains("args"),
            "a request with no extra arguments must not mention any",
        )
        // And the rule the screen and the link both consult. With no profiles
        // listing — a machine older than the `profiles` verb — it is still the
        // id rule, because that machine still branches on `generic`.
        assertTrue(Agentd.commandIsRequired("generic"), "generic carries no program")
        assertFalse(Agentd.commandIsRequired("claude"), "claude does")
        assertFalse(Agentd.commandIsRequired(null), "the machine's default is not generic by name")
    }

    @Test
    fun `the daemon's own flag beats the id guess when the daemon states one`() {
        // The guess this retires: `id == "generic"` was hard-coded here
        // because the daemon published no flag, and its own comment said so.
        // `Request::Profiles` publishes `command_required`, derived from the
        // adapter table — so an adapter added later that needs a program named
        // by the caller works with no release of this app.
        //
        // Asserted with a row that DISAGREES with the id rule in both
        // directions, because a test using only rows that agree would pass
        // against an implementation that ignored the listing entirely.
        val stated = listOf(
            AgentProfile(agent = "someagent", commandRequired = true),
            AgentProfile(agent = "generic", commandRequired = false),
        )
        assertTrue(
            Agentd.commandIsRequired("someagent", stated),
            "an adapter the daemon says needs a command was offered without one",
        )
        assertFalse(
            Agentd.commandIsRequired("generic", stated),
            "the id rule overrode the daemon's own answer",
        )

        // An adapter the listing does not mention falls back to the id rule
        // rather than to `false`. False would be the dangerous default: it
        // offers the button that can only fail.
        assertTrue(Agentd.commandIsRequired("generic", listOf(AgentProfile(agent = "claude"))))
    }

    @Test
    fun `worktrees is a verb — the note that said it was not read the wrong enum`() {
        expect("worktrees_all", Agentd.worktrees())
        expect("worktrees_one", Agentd.worktrees("rime"))
    }

    @Test
    fun `projects and profiles are verbs, and neither carries an argument`() {
        // P1-054's second criterion — "start a new agent with selected
        // profile/project/worktree" — had no picker for two of those three
        // because neither verb existed. `StartAgentScreen`'s doc comment said
        // so, correctly, and this is the assertion that it is no longer true:
        // the fixture these are compared against is the one the daemon's own
        // serde parses in `android_requests_wire.rs`.
        expect("projects", Agentd.projects())
        expect("profiles", Agentd.profiles())

        // The tag alone is the whole request. A stray key here would still
        // parse on the daemon side — serde ignores unknown fields on a unit
        // variant — so the assertion has to be made on this side, where it can
        // fail.
        assertEquals(setOf("cmd"), json.parseToJsonElement(Agentd.projects()).jsonObject.keys)
        assertEquals(setOf("cmd"), json.parseToJsonElement(Agentd.profiles()).jsonObject.keys)

        // And `projects` is not `worktrees` under another name: they answer
        // different questions at very different costs, and a screen that
        // opened by calling the expensive one is why there was no picker.
        assertFalse(
            Agentd.projects() == Agentd.worktrees(),
            "the cheap listing and the git walk are the same request",
        )
    }

    @Test
    fun `the approval verbs a phone is allowed`() {
        expect("requests", Agentd.requests())
        expect("grants", Agentd.grants())
        expect("system_grants", Agentd.systemGrants())
        expect("revoke_one", Agentd.revoke("/home/andre/Projects/rime", "pkg-upgrade"))
        expect("revoke_all", Agentd.revoke("/home/andre/Projects/rime"))
        expect("revoke_system_grant", Agentd.revokeSystemGrant(3))
    }

    @Test
    fun `omitting a grant key means every grant for the project, and is not a null`() {
        // `Request::Revoke.key` is `Option<String>` with `#[serde(default)]`,
        // and absent means "everything for this project" — a wide action. A
        // caller that meant one key and sent a null would revoke them all.
        assertFalse(Agentd.revoke("/p").contains("key"), "a wide revoke must not name a key")
        assertTrue(Agentd.revoke("/p", "pin").contains(""""key":"pin""""))
    }

    @Test
    fun `nothing here builds a decide request`() {
        // Inverted on purpose, and it is the one assertion in this file that
        // is about something NOT existing. `privilege.rs:1176` refuses
        // `decide` from any non-local origin BEFORE the pending check, so a
        // paired phone can neither approve nor deny, and `Request::Decide`
        // carries no second factor. Adding the builder is the obvious move on
        // seeing a list of pending approvals; it would be refused every time
        // it was pressed. If this test ever fails, read `Approvals.kt` before
        // deleting it.
        val builders = Agentd::class.java.methods.map { it.name }
        assertFalse("decide" in builders, "a decide builder was added; see Approvals.kt")
        assertFalse(
            fixture.keys.any { it.contains("decide") },
            "the shared request fixture grew a decide request",
        )
    }

    @Test
    fun `the fixture and the builders cover the same set`() {
        // So that a request added to one half and not the other is caught,
        // rather than the fixture quietly drifting into a list of things
        // nothing sends.
        val named = fixture.keys.filterNot { it.startsWith("_") }.toSet()
        assertEquals(
            setOf(
                "hello", "list", "info", "attach", "resize", "signal", "input",
                "input_submit", "input_bare", "rename", "rename_clear", "peek",
                "clipboard", "clipboard_write", "save_file", "save_file_hostile_name",
                "receive", "receive_hostile_name",
                "run_minimal", "run_full", "run_generic_with_args",
                "worktrees_all", "worktrees_one",
                "projects", "profiles",
                "requests", "grants", "system_grants",
                "revoke_one", "revoke_all", "revoke_system_grant",
            ),
            named,
            "the fixture no longer matches the requests this test exercises",
        )
    }
}
