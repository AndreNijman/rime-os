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
object Desktop : Computer("broker", "lan")

/**
 * The second computer (TwoComputersOnDeviceTest): its own rime-agentd,
 * rime-remoted and broker, started by the runner in a login session of its
 * own, reached with `-e broker2` / `-e lan2`.
 */
object SecondDesktop : Computer("broker2", "lan2")

/** One computer the suite can reach: [Desktop] is the first, [SecondDesktop] the second. */
open class Computer(private val brokerArg: String, private val lanArg: String) {

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
    fun lanAddress(): String = arg(lanArg)

    /** The broker: `host:port`. */
    private fun broker(): Pair<String, Int> {
        val a = arg(brokerArg)
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
    fun ask(request: String, readTimeoutMs: Int = 30_000): JSONObject {
        val (host, port) = broker()
        Socket().use { socket ->
            socket.connect(InetSocketAddress(host, port), 8_000)
            socket.soTimeout = readTimeoutMs
            socket.getOutputStream().write((request + "\n").toByteArray())
            socket.getOutputStream().flush()
            val line = socket.getInputStream().bufferedReader().let(BufferedReader::readLine)
                ?: error("the broker closed without answering $request")
            return JSONObject(line)
        }
    }

    /** This computer going away: its rime-remoted killed and not started again. */
    fun stopRemoted() {
        val reply = ask("""{"cmd":"stop_remoted"}""")
        check(reply.optString("reply") == "ok") { "the broker did not stop rime-remoted: $reply" }
    }

    /** rime-remoted (re)started on the same port with the same state, as `systemctl --user restart` does. */
    fun restartRemoted() {
        val reply = ask("""{"cmd":"restart_remoted"}""", readTimeoutMs = 60_000)
        check(reply.optString("reply") == "ok") { "the broker did not restart rime-remoted: $reply" }
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

    /**
     * Set what the stub machine will report next (the runner always starts
     * rime-remoted with `RIME_REMOTED_MACHINE_STUB`, so the machine verbs
     * never touch the desktop the suite runs on).
     */
    fun machineStubSeed(state: JSONObject) {
        val reply = ask(JSONObject().put("cmd", "machine_stub_seed").put("state", state).toString())
        check(reply.optString("reply") == "ok") { "the broker did not seed the stub machine: $reply" }
    }

    /** Every action the stub machine was asked to take, oldest first. */
    fun machineStubActions(): List<JSONObject> {
        val reply = ask(JSONObject().put("cmd", "machine_stub_actions").toString())
        check(reply.optString("reply") == "ok") { "the broker did not read the stub machine's actions: $reply" }
        val a = reply.getJSONArray("actions")
        return (0 until a.length()).map { a.getJSONObject(it) }
    }

    /**
     * What the stub desktop's clipboard holds, or null when nothing wrote it.
     * The runner always starts rime-agentd with `RIME_AGENTD_DESKTOP_STUB`,
     * so "Send to Rime" never touches the clipboard of the computer the
     * suite runs on.
     */
    fun deskClipboard(): String? {
        val reply = ask(JSONObject().put("cmd", "desk_clipboard").toString())
        check(reply.optString("reply") == "ok") { "the broker did not read the stub clipboard: $reply" }
        return if (reply.isNull("text")) null else reply.getString("text")
    }

    /** One finished file in the stub Downloads folder. */
    data class DeskFile(val name: String, val size: Long, val sha256: String)

    /** The stub Downloads folder: where it is, and every file in it (dot-files included). */
    fun deskDownloads(): Pair<String, List<DeskFile>> {
        val reply = ask(JSONObject().put("cmd", "desk_downloads").toString())
        check(reply.optString("reply") == "ok") { "the broker did not list the stub Downloads: $reply" }
        val a = reply.getJSONArray("files")
        return reply.getString("dir") to (0 until a.length()).map {
            val f = a.getJSONObject(it)
            DeskFile(f.getString("name"), f.getLong("size"), f.getString("sha256"))
        }
    }

    /** Empty the stub clipboard and Downloads, so a test sees only its own writes. */
    fun deskReset() {
        val reply = ask(JSONObject().put("cmd", "desk_reset").toString())
        check(reply.optString("reply") == "ok") { "the broker did not reset the stub desktop: $reply" }
    }

    /**
     * Every call rime-remoted made on the stub notification server
     * (`RIME_REMOTED_NOTIFY_STUB`), oldest first: `{"op":"notify",...}` or
     * `{"op":"close","id":N}`. The suite never reaches this desktop's own
     * notifications.
     */
    fun notifyLog(): List<JSONObject> {
        val reply = ask(JSONObject().put("cmd", "notify_log").toString())
        check(reply.optString("reply") == "ok") { "the broker did not read the stub notifications: $reply" }
        val a = reply.getJSONArray("entries")
        return (0 until a.length()).map { a.getJSONObject(it) }
    }

    /**
     * Every change rime-remoted made to the phones' media players on the stub
     * (`RIME_REMOTED_MEDIA_STUB`), oldest first: `{"op":"show","player":…,
     * "identity":…,"status":…,"title":…,…}` or `{"op":"gone","player":…}`.
     * The suite never puts a player on this desktop's bus.
     */
    fun mediaLog(): List<JSONObject> {
        val reply = ask(JSONObject().put("cmd", "media_log").toString())
        check(reply.optString("reply") == "ok") { "the broker did not read the stub media players: $reply" }
        val a = reply.getJSONArray("entries")
        return (0 until a.length()).map { a.getJSONObject(it) }
    }

    /**
     * A method call on a phone's stub media player, as a reader on the
     * computer's bus makes it (contract §2.8): [call] is
     * `{"player":…,"method":"PlayPause"|"Next"|…}`, with `offset_us` for
     * `Seek` and `track` + `position_us` for `SetPosition`. Its outcome shows
     * in [mediaLog] as `{"op":"call",…,"result":…}`.
     */
    fun mediaCall(call: JSONObject) {
        val reply = ask(JSONObject().put("cmd", "media_call").put("call", call).toString())
        check(reply.optString("reply") == "ok") { "the broker did not call the stub media player: $reply" }
    }

    /**
     * What rime-remoted wrote to `focus.json` for the Shell (contract §2.9):
     * `{"dnd":…,"device":…,"at_ms":…}`, or null before any phone's change.
     * Read from the run's own runtime directory, never this desktop's.
     */
    fun focusFile(): JSONObject? {
        val reply = ask(JSONObject().put("cmd", "focus_file").toString())
        check(reply.optString("reply") == "ok") { "the broker did not read focus.json: $reply" }
        return reply.optJSONObject("focus")
    }

    /**
     * `rime remote focus on|off`: the person at the computer turning Do Not
     * Disturb on or off with Focus sync on. Returns how many phones were
     * sent the line (`reached`).
     */
    fun focus(dnd: Boolean): Int {
        val reply = ask(JSONObject().put("cmd", "focus").put("dnd", dnd).toString())
        check(reply.optString("reply") == "focused") { "rime-remoted did not take the focus request: $reply" }
        return reply.getInt("reached")
    }

    /**
     * The stub notification server closes notification [id] with [reason]
     * (2: the person dismissed it, 1: it timed out), as a real one would say
     * in `NotificationClosed`.
     */
    fun notifyClosed(id: Int, reason: Int) {
        val reply = ask(JSONObject().put("cmd", "notify_closed").put("id", id).put("reason", reason).toString())
        check(reply.optString("reply") == "ok") { "the broker did not close the stub notification: $reply" }
    }

    /**
     * The person typed [text] into stub notification [id]'s reply field, as
     * a real server says in `NotificationReplied`. A real server then closes
     * it; the caller says so with [notifyClosed].
     */
    fun notifyReply(id: Int, text: String) {
        val reply = ask(JSONObject().put("cmd", "notify_reply").put("id", id).put("text", text).toString())
        check(reply.optString("reply") == "ok") { "the broker did not reply to the stub notification: $reply" }
    }

    /**
     * `rime remote devices --json` on the computer, run by the broker against
     * this run's rime-remoted: the device list as the person at the computer
     * sees it, `companion` included.
     */
    fun remoteDevicesJson(): List<JSONObject> {
        val reply = ask(JSONObject().put("cmd", "remote_devices_json").toString())
        check(reply.optString("reply") == "ok") { "the broker did not run rime remote devices: $reply" }
        val a = reply.getJSONArray("devices")
        return (0 until a.length()).map { a.getJSONObject(it) }
    }

    /**
     * An SMS arriving on the EMULATOR from [number] (an emulator number:
     * 5554, 5556, 15555215556), through its console. The broker refuses on
     * any serial that is not an emulator.
     */
    fun emuSms(number: String, text: String) {
        val reply = ask(JSONObject().put("cmd", "emu_sms").put("number", number).put("text", text).toString())
        check(reply.optString("reply") == "ok") { "the broker did not inject the SMS: $reply" }
    }

    /**
     * `rime remote messages` against this run's rime-remoted, asked by the
     * broker, which runs at the computer's login session: the conversation
     * list, or one conversation's messages when [thread] is set. The reply
     * as the daemon gave it, `messages` or `error`.
     */
    fun messages(device: String, thread: Long? = null, limit: Int = 20): JSONObject {
        val req = JSONObject().put("cmd", "messages").put("device", device).put("limit", limit)
        if (thread != null) req.put("thread", thread)
        return ask(req.toString())
    }

    /** `rime remote messages send --to [to]` with [text] on its input. The reply as given. */
    fun messagesSend(device: String, to: String, text: String): JSONObject =
        ask(JSONObject().put("cmd", "messages_send").put("device", device).put("to", to).put("text", text).toString())

    /**
     * The EMULATOR's modem: an incoming call from [number] (`call`), or that
     * call answered, ended or refused by the far end (`accept`, `cancel`,
     * `busy`), through its console. Emulator numbers only; the broker refuses
     * on any serial that is not an emulator.
     */
    fun emuGsm(action: String, number: String) {
        val reply = ask(JSONObject().put("cmd", "emu_gsm").put("action", action).put("number", number).toString())
        check(reply.optString("reply") == "ok") { "the broker did not $action the emulator's call: $reply" }
    }

    /**
     * The person pressed button [action] on stub notification [id], as a real
     * server says in `ActionInvoked`. A real server then closes it; the
     * caller says so with [notifyClosed].
     */
    fun notifyAction(id: Int, action: String) {
        val reply = ask(JSONObject().put("cmd", "notify_action").put("id", id).put("action", action).toString())
        check(reply.optString("reply") == "ok") { "the broker did not press the stub notification's button: $reply" }
    }

    /**
     * `rime remote calls` against this run's rime-remoted: the phone's call
     * now and its recent calls. The reply as the daemon gave it, `calls` or
     * `error`.
     */
    fun calls(device: String, limit: Int = 20): JSONObject =
        ask(JSONObject().put("cmd", "calls").put("device", device).put("limit", limit).toString())

    /** `rime remote calls answer|decline|hangup|dial NUMBER`. The reply as given, `call_done` or `error`. */
    fun call(device: String, action: String, number: String? = null): JSONObject {
        val req = JSONObject().put("cmd", "call").put("device", device).put("action", action)
        if (number != null) req.put("number", number)
        return ask(req.toString())
    }

    /** One of the photo tests' images, as the broker made and pushed it. */
    data class TestPhoto(val name: String, val len: Long, val sha256: String, val gps: Boolean)

    /**
     * Three test images made on the computer, the last with a GPS location
     * in its EXIF, pushed into the EMULATOR's Pictures/RimeTest and indexed
     * in its MediaStore; any older RimeTest images are deleted first. The
     * broker refuses on any serial that is not an emulator, and fails with a
     * message naming PIL when the computer cannot make them.
     */
    fun photosPush(): List<TestPhoto> {
        val reply = ask(JSONObject().put("cmd", "photos_push").toString(), readTimeoutMs = 120_000)
        check(reply.optString("reply") == "ok") { "the broker did not push the test photos: $reply" }
        val a = reply.getJSONArray("photos")
        return (0 until a.length()).map { a.getJSONObject(it) }.map {
            TestPhoto(it.getString("name"), it.getLong("len"), it.getString("sha256"), it.getBoolean("gps"))
        }
    }

    /** Delete the photo tests' folder on the emulator and wait for its MediaStore rows to go. */
    fun photosCleanup() {
        val reply = ask(JSONObject().put("cmd", "photos_cleanup").toString(), readTimeoutMs = 60_000)
        check(reply.optString("reply") == "ok") { "the broker did not delete the test photos: $reply" }
    }

    /**
     * `rime remote photos --device [device] --json --limit [limit]`, run by
     * the broker at the computer: `code` (the exit status), `out` (its JSON,
     * on a failure too), `stderr`, and `thumbs`, the computer-side facts
     * about each thumbnail it named (`exists`, `under_run`, `mode`, `exif`,
     * `app1`).
     */
    fun photosList(device: String, limit: Int = 10): JSONObject {
        val reply = ask(JSONObject().put("cmd", "photos_list").put("device", device).put("limit", limit).toString(), readTimeoutMs = 120_000)
        check(reply.optString("reply") == "ok") { "the broker did not run rime remote photos: $reply" }
        return reply
    }

    /**
     * `rime remote photos save [photo] --device [device] --to <run>/saved
     * --json` at the computer: `code`, `out`, `stderr`, and `saved`, the
     * saved file's `len`, `sha256` and `gps` (whether it still says where it
     * was taken).
     */
    fun photosSave(device: String, photo: Long): JSONObject {
        val reply = ask(JSONObject().put("cmd", "photos_save").put("device", device).put("photo", photo).toString(), readTimeoutMs = 120_000)
        check(reply.optString("reply") == "ok") { "the broker did not run rime remote photos save: $reply" }
        return reply
    }

    /** Whether this run's rime-remoted holds a thumbnail folder for [device]. */
    fun photosThumbsDir(device: String): Boolean {
        val reply = ask(JSONObject().put("cmd", "photos_thumbs_dir").put("device", device).toString())
        check(reply.optString("reply") == "ok") { "the broker did not look for the thumbnails: $reply" }
        return reply.getBoolean("exists")
    }

    private fun requestReply(line: String): JSONObject {
        val reply = ask(line)
        check(reply.optString("reply") == "request") {
            "rime-agentd did not answer $line with a privilege request: $reply"
        }
        return reply
    }
}
