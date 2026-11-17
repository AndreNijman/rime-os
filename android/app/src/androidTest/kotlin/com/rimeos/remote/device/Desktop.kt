package com.rimeos.remote.device

import androidx.test.platform.app.InstrumentationRegistry
import java.io.BufferedReader
import java.net.InetSocketAddress
import java.net.Socket
import org.json.JSONObject

/**
 * The desktop, as the on-device suite can reach it.
 *
 * `rime-remoted`'s control socket is a unix socket on the computer and a phone
 * can never touch one, so `tools/run-device-suite.sh` puts a line-JSON broker
 * in front of it and passes this test the address. Every offer the suite pairs
 * with, and every revocation it asserts against, is minted by the REAL daemon
 * through that broker — the broker itself decides nothing.
 *
 * A missing argument is a **failure and never a skip**. An instrumented suite
 * that quietly passed when nobody told it where the desktop was would be a
 * green run asserting nothing, which is the failure mode this whole program
 * has shipped more than once.
 */
object Desktop {

    /** An instrumentation argument, or a failure that names it. */
    fun arg(name: String): String {
        val value = InstrumentationRegistry.getArguments().getString(name)
        check(!value.isNullOrBlank()) {
            "this suite needs -e $name. It is an END-TO-END suite against a real " +
                "rime-remoted, so without it there is nothing to test and a pass would " +
                "mean nothing. Run it through android/tools/run-device-suite.sh."
        }
        return value
    }

    /** `host:port` of the daemon's LAN listener, as the runner measured it. */
    fun lanAddress(): String = arg("lan")

    /** The broker: `host:port`. */
    private fun broker(): Pair<String, Int> {
        val a = arg("broker")
        return a.substringBeforeLast(':') to a.substringAfterLast(':').toInt()
    }

    /**
     * One request on the broker; the reply is one JSON line.
     *
     * `org.json`, which Android ships, rather than `kotlinx.serialization`:
     * these replies are the DESKTOP's shapes and this side should not need a
     * model of them to read one field. A test harness that had to be kept in
     * step with `serde` would be one more thing that can disagree.
     */
    fun ask(request: String): JSONObject {
        val (host, port) = broker()
        Socket().use { socket ->
            socket.connect(InetSocketAddress(host, port), 8_000)
            socket.soTimeout = 30_000
            socket.getOutputStream().write((request + "\n").toByteArray())
            socket.getOutputStream().flush()
            val line = socket.getInputStream().bufferedReader().let(BufferedReader::readLine)
                ?: error("the broker closed without answering $request")
            return JSONObject(line)
        }
    }

    /** A fresh pairing offer, minted by the real daemon. */
    fun offerPayload(): String {
        val reply = ask("""{"cmd":"pair"}""")
        check(reply.has("qr")) { "rime-remoted did not mint an offer: $reply" }
        return reply.getString("qr")
    }

    /** Every device the desktop knows, by id. */
    fun deviceIds(): List<String> = devices().map { it.getString("id") }

    private fun devices(): List<JSONObject> {
        val reply = ask("""{"cmd":"devices"}""")
        val array = reply.optJSONArray("devices") ?: return emptyList()
        return (0 until array.length()).map { array.getJSONObject(it) }
    }

    /** The desktop's view of one device, or null. */
    fun device(id: String): JSONObject? = devices().firstOrNull { it.optString("id") == id }

    /** Take a device's access away, as `rime remote revoke` does. */
    fun revoke(id: String): JSONObject = ask("""{"cmd":"revoke","device":"$id"}""")

    // ── the human at the computer, which this phone is not ──────────────────
    //
    // §7 reserves filing and deciding a privilege request for a local origin,
    // and `rime-agentd` enforces that on the wire. So the state an approvals
    // screen exists to show cannot be produced from here at all, and these two
    // ask the RUNNER to produce it — from a process in a login session on the
    // computer, which is a human as far as the daemon is concerned.
    //
    // They are two named verbs with fixed shapes and not a general forwarder.
    // A "send anything to rime-agentd" verb would be a way for a test to borrow
    // a local origin for any request at all, and the value of this whole suite
    // is that the phone's own origin is real.

    /**
     * File a privilege request at the computer. Returns the daemon's record.
     *
     * `Response::Request` is an internally tagged newtype variant, so the
     * record's own fields sit beside `"reply":"request"` rather than under a
     * key of their own — the same flattening `Approvals.kt` documents for
     * `verb`. The reply object IS the request.
     */
    fun filePrivilegeRequest(verb: String, args: List<String>, reason: String): JSONObject {
        val list = args.joinToString(",") { """"$it"""" }
        return requestReply(
            """{"cmd":"file_privilege_request","verb":"$verb","args":[$list],"reason":"$reason"}""",
        )
    }

    /** Decide one, at the computer, the way `rime request allow|deny` does. */
    fun decideLocally(id: Int, decision: String): JSONObject =
        requestReply("""{"cmd":"decide_locally","id":$id,"decision":"$decision"}""")

    /** The folder the runner's nexd works in, on the computer. */
    fun nexRoot(): String = arg("nexroot")

    /**
     * A nex session in [nexRoot], made at the computer. nexd lets a phone
     * start sessions only in a folder somebody at the computer already chose,
     * so this is that choice; everything after it is the phone's.
     */
    fun nexSessionAtComputer(title: String): String {
        val reply = ask(JSONObject().put("cmd", "nex_session_at_computer").put("title", title).toString())
        check(reply.optString("reply") == "ok") { "nexd did not make a session at the computer: $reply" }
        return reply.getString("session_id")
    }

    /** Type [text] into [session] at the computer: nothing the phone sent. */
    fun nexSubmitAtComputer(session: String, text: String) {
        val reply = ask(
            JSONObject().put("cmd", "nex_submit_at_computer").put("session", session).put("text", text).toString(),
        )
        check(reply.optString("reply") == "ok") { "nexd did not take the message at the computer: $reply" }
    }

    private fun requestReply(line: String): JSONObject {
        val reply = ask(line)
        check(reply.optString("reply") == "request") {
            "rime-agentd did not answer $line with a privilege request: $reply"
        }
        return reply
    }
}
