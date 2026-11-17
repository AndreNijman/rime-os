package com.rimeos.remote.core.agent

import com.rimeos.remote.core.Crypto
import org.bouncycastle.crypto.InvalidCipherTextException
import org.bouncycastle.crypto.digests.SHA256Digest
import java.security.SecureRandom

/**
 * The push envelope, and the rules around it (P1-058).
 *
 * `rimed/rime-remote-core/src/push.rs` is the specification; this is the other
 * end of it. Everything that can be a rule is here, in `:core`, where a JVM
 * test can reach it — the `BroadcastReceiver` in `:app` is plumbing only, for
 * the same reason `Notifier` is.
 *
 * ## What this is for
 *
 * Until now an alert reached this phone only while the app was running and its
 * poll loop was alive. A phone in a pocket with the screen off got nothing:
 * `com.rimeos.remote` is not on Android's Doze whitelist — measured on a real
 * GrapheneOS device, `dumpsys deviceidle whitelist` has zero matches — so the
 * platform suspends its network and an agent that needs somebody reaches
 * nobody.
 *
 * A UnifiedPush distributor is what solves that, because it is the app the
 * user installed *in order to* hold a connection open, and it is the one the
 * platform exempts. Rime Remote holds nothing open, runs no foreground
 * service, and depends on no Google component.
 *
 * ## Why the payload is opaque to everything that carries it
 *
 * P1-058's second criterion asks that the push infrastructure not receive
 * sensitive prompt or code content. What crosses it is [ENVELOPE_LEN] bytes of
 * which 38 are ciphertext, and the plaintext is five fixed-width integers —
 * there is no field in it that can hold a sentence. The push server and the
 * distributor learn that a registration received something, when, and that it
 * was 51 bytes; the size is the same for every kind, so even *which* kind it
 * was does not leak.
 *
 * The key is this phone's: generated here by [newKey], handed to one machine
 * inside the already-authenticated Noise channel, and stored beside that
 * machine's record. It is deliberately **not** derived from the device
 * identity key, and that is not a shortcut — [StaticKeyIsUnreachableHere]
 * explains why it could not be.
 */
object Push {
    /** The envelope format this build speaks. */
    const val VERSION: Int = 1

    const val KEY_LEN: Int = 32
    const val NONCE_LEN: Int = 12
    const val TAG_LEN: Int = 16
    const val BODY_LEN: Int = 22
    const val ENVELOPE_LEN: Int = 1 + NONCE_LEN + BODY_LEN + TAG_LEN

    /**
     * Fixed, public, and the same bytes `push.rs` uses. KEPT through the
     * rebrand: every push a desktop in the field seals is keyed by them.
     */
    private val HKDF_SALT = "apex.push.v1".toByteArray(Charsets.US_ASCII) // rime-rename: keep (push HKDF salt, wire)
    private val HKDF_INFO = "apex.push.envelope".toByteArray(Charsets.US_ASCII) // rime-rename: keep (push HKDF info, wire)

    private val random = SecureRandom()

    /**
     * Why the push key is not the device identity key, recorded where somebody
     * about to "simplify" it will read it.
     *
     * The device's X25519 secret is wrapped by a keystore key created with
     * `setUserAuthenticationRequired(true)`, per-use. `StaticKey.agree` may
     * raise a biometric prompt and may throw. A `BroadcastReceiver` woken by a
     * distributor at three in the morning, on a locked phone, cannot satisfy
     * that — so a push key derived from it would fail to decrypt on exactly the
     * devices whose owner asked for more protection, silently, with no error
     * anybody would ever see.
     */
    const val StaticKeyIsUnreachableHere: String =
        "the device identity key is behind a per-use biometric gate and a woken receiver " +
            "cannot use it"

    /** A fresh registration key. 32 bytes from [SecureRandom]. */
    fun newKey(): ByteArray = ByteArray(KEY_LEN).also { random.nextBytes(it) }

    /**
     * The AEAD key a registration key produces: RFC 5869 HKDF-SHA256, one
     * 32-byte output.
     *
     * Spelled out rather than taken from a library, exactly as
     * `Crypto.hmacBlake2s` is and for the same reason: it has to reproduce
     * what `push.rs` computes byte for byte, and a reader has to be able to
     * check it against the RFC. `PushVectorsTest` is what proves it does.
     */
    fun aeadKey(key: ByteArray): ByteArray {
        require(key.size == KEY_LEN) { "a push key is $KEY_LEN bytes; this one is ${key.size}" }
        val prk = hmacSha256(HKDF_SALT, key)
        return hmacSha256(prk, HKDF_INFO + byteArrayOf(0x01))
    }

    /** HMAC-SHA256, RFC 2104, over BouncyCastle's lightweight digest. */
    internal fun hmacSha256(key: ByteArray, data: ByteArray): ByteArray {
        val block = 64
        var k = key
        if (k.size > block) k = sha256(k)
        if (k.size < block) k = k.copyOf(block)
        val ipad = ByteArray(block) { (k[it].toInt() xor 0x36).toByte() }
        val opad = ByteArray(block) { (k[it].toInt() xor 0x5c).toByte() }
        return sha256(opad + sha256(ipad + data))
    }

    private fun sha256(bytes: ByteArray): ByteArray {
        val d = SHA256Digest()
        d.update(bytes, 0, bytes.size)
        val out = ByteArray(d.digestSize)
        d.doFinal(out, 0)
        return out
    }

    /**
     * Open an envelope, or say why it did not open.
     *
     * Returns `null` for anything that is not an envelope this registration
     * can read — a forgery, a corruption, a machine this key does not belong
     * to, or a format this build does not speak. Null rather than an exception
     * because the caller is a `BroadcastReceiver` and the only useful thing it
     * can do with a message it cannot read is drop it.
     */
    fun open(key: ByteArray, bytes: ByteArray): Body? {
        if (bytes.size != ENVELOPE_LEN) return null
        if (bytes[0].toInt() and 0xff != VERSION) return null
        val plain = try {
            Crypto.decrypt(
                aeadKey(key),
                bytes.copyOfRange(1, 1 + NONCE_LEN),
                // The version byte is the associated data, so rewriting it is
                // a tag mismatch rather than a downgrade that opens.
                byteArrayOf(VERSION.toByte()),
                bytes.copyOfRange(1 + NONCE_LEN, bytes.size),
            )
        } catch (_: InvalidCipherTextException) {
            return null
        } catch (_: IllegalArgumentException) {
            return null
        }
        return Body.unpack(plain)
    }

    /**
     * The plaintext of an envelope.
     *
     * Five integers, and there is deliberately nowhere in it for a string.
     */
    data class Body(
        val kind: Alert.Kind,
        /** The adapter, already collapsed to an allowlist by the desktop. */
        val agent: String,
        /** The session, or [Alert.NO_SESSION]. */
        val session: Int,
        /** That session's start, in unix **seconds**, or 0 when there is none. */
        val started: Long,
        /** Monotonic per registration. See [Replay]. */
        val seq: Long,
    ) {
        companion object {
            fun unpack(bytes: ByteArray): Body? {
                if (bytes.size != BODY_LEN) return null
                // A kind this build has not been taught is DROPPED, never
                // rendered with another kind's words. A notification that says
                // "Failed" for something the machine called a deployment is
                // worse than no notification.
                val kind = kindOf(bytes[0].toInt() and 0xff) ?: return null
                return Body(
                    kind = kind,
                    agent = adapterOf(bytes[1].toInt() and 0xff),
                    session = be32(bytes, 2),
                    started = be64(bytes, 6),
                    seq = be64(bytes, 14),
                )
            }

            private fun be32(b: ByteArray, at: Int): Int {
                var v = 0
                for (i in 0 until 4) v = (v shl 8) or (b[at + i].toInt() and 0xff)
                return v
            }

            private fun be64(b: ByteArray, at: Int): Long {
                var v = 0L
                for (i in 0 until 8) v = (v shl 8) or (b[at + i].toLong() and 0xff)
                return v
            }
        }
    }

    /**
     * The wire code for each kind. **The discriminants are the protocol** —
     * `push.rs`'s `Kind` — so this map is the agreement and not a convenience.
     */
    private val KINDS: Map<Int, Alert.Kind> = mapOf(
        1 to Alert.Kind.WAITING,
        2 to Alert.Kind.PERMISSION,
        3 to Alert.Kind.FAILED,
        4 to Alert.Kind.FINISHED,
        5 to Alert.Kind.TEST_FAILED,
        6 to Alert.Kind.APPROVAL,
        7 to Alert.Kind.DEPLOYED,
        8 to Alert.Kind.DEPLOY_FAILED,
        9 to Alert.Kind.NEX_APPROVAL,
    )

    fun kindOf(code: Int): Alert.Kind? = KINDS[code]

    /** The code for a kind, for a test that walks the vocabulary both ways. */
    fun codeOf(kind: Alert.Kind): Int =
        KINDS.entries.first { it.value == kind }.key

    /**
     * The adapter an envelope's code names.
     *
     * An allowlist on the desktop side, so an adapter this build has never
     * heard of arrives as 0 and renders as "Agent" — a site-local agent's name
     * is a fact about the owner's setup and does not go past the machine.
     */
    private val ADAPTERS: Map<Int, String> = mapOf(
        1 to "claude",
        2 to "opencode",
        3 to "codex",
        4 to "gemini",
        5 to "kimi",
        6 to "generic",
    )

    fun adapterOf(code: Int): String = ADAPTERS[code] ?: ""

    /**
     * The endpoint a machine still has to be told about, or `null`.
     *
     * ## Why this decision is in `:core` and not next to the receiver
     *
     * It is the join between three things that each look fine alone and are
     * wrong together: a distributor that has answered, a machine that has been
     * told, and a machine that has been told something *else*. Nothing about
     * it needs Android, and the failure it prevents is the one nobody sees —
     * a phone that believes it has a push path it never sent, and therefore
     * never rings, with no error on either side.
     *
     * `null` covers four states and none of them is an error:
     *
     * * no registration at all — this phone has no distributor, or has never
     *   asked one for this machine;
     * * a registration whose endpoint has not arrived yet (it is empty while
     *   the distributor is thinking);
     * * one that arrived unusable — [UnifiedPush.checkEndpoint] applies the
     *   same rules `push.rs`'s `Endpoint::parse` does, so a phone does not
     *   burn a round trip to be told what it could see for itself;
     * * an endpoint this machine has already been told, which is the ordinary
     *   case on every reconnection after the first.
     *
     * A distributor rotating the endpoint writes
     * [com.rimeos.remote.core.PushRegistration.endpoint] and leaves
     * [com.rimeos.remote.core.PushRegistration.sentEndpoint] behind, so the
     * next connection sends once and then stops. That is why the two fields
     * are separate: compared against the live endpoint alone, "already sent"
     * and "just rotated" are the same value.
     */
    fun endpointToSend(machine: com.rimeos.remote.core.PairedMachine): String? {
        val push = machine.push ?: return null
        val endpoint = UnifiedPush.checkEndpoint(push.endpoint) ?: return null
        if (endpoint == push.sentEndpoint) return null
        return endpoint
    }

    /**
     * Whether an envelope is new, or one this phone has already acted on.
     *
     * A distributor may deliver the same message twice: the UnifiedPush
     * specification says an unacknowledged message MAY be retried, and the
     * acknowledgement can be lost. The sequence number is what makes that
     * harmless — it is monotonic per registration on the desktop and persisted
     * there across restarts.
     *
     * Note what this is **not**: it is not the deduplication P1-058's fourth
     * criterion asks for. That one is about two notifications for one moment,
     * and it is resolved by the notification **id** — see
     * [NotificationContent.idFor] — so that a pushed alert and a polled alert
     * for the same (machine, session, kind) replace each other rather than
     * stacking. This is only about the same bytes arriving twice.
     */
    class Replay {
        private var last: Long = 0

        /** True when this sequence is new; false when it has been seen. */
        fun accept(seq: Long): Boolean {
            if (seq <= last) return false
            last = seq
            return true
        }

        /** Restore from storage, so a process restart does not accept a replay. */
        fun restore(seq: Long) {
            if (seq > last) last = seq
        }

        val highest: Long get() = last
    }
}

/**
 * The UnifiedPush Android specification, as this app speaks it.
 *
 * Spec **AND_3.0.0**. The action and extra names are the specification's, read
 * from it rather than remembered: a wrong extra name fails silently, with no
 * error, no log line and no device here to notice.
 *
 * ## What this app does and does not implement
 *
 * It implements the connector side of the broadcast protocol: register,
 * unregister, and the four messages a distributor sends back. It does **not**
 * implement RFC 8291 Web Push encryption, and does not send a `vapid` extra.
 *
 * That is a deliberate scope decision rather than an omission. The payload is
 * already ciphertext before it reaches the push server — [Push] seals it with
 * a key the server has never had — so RFC 8291 would be a second, redundant
 * layer, and implementing it would mean a P-256 keypair and an ECDH in a
 * `BroadcastReceiver` for no gain in confidentiality. The consequence is named
 * rather than hidden: a distributor that *requires* VAPID answers
 * `REGISTRATION_FAILED` with reason [REASON_VAPID_REQUIRED], and
 * [Failure.explain] turns that into a sentence that says which distributor is
 * refusing and why, instead of a phone that silently never rings.
 */
object UnifiedPush {
    // ── application → distributor ───────────────────────────────────────────
    const val ACTION_REGISTER: String = "org.unifiedpush.android.distributor.REGISTER"
    const val ACTION_UNREGISTER: String = "org.unifiedpush.android.distributor.UNREGISTER"
    const val ACTION_MESSAGE_ACK: String = "org.unifiedpush.android.distributor.MESSAGE_ACK"

    // ── distributor → application ───────────────────────────────────────────
    const val ACTION_NEW_ENDPOINT: String = "org.unifiedpush.android.connector.NEW_ENDPOINT"
    const val ACTION_REGISTRATION_FAILED: String =
        "org.unifiedpush.android.connector.REGISTRATION_FAILED"
    const val ACTION_UNREGISTERED: String = "org.unifiedpush.android.connector.UNREGISTERED"
    const val ACTION_MESSAGE: String = "org.unifiedpush.android.connector.MESSAGE"

    // ── extras ──────────────────────────────────────────────────────────────
    const val EXTRA_APPLICATION: String = "application"
    const val EXTRA_TOKEN: String = "token"
    const val EXTRA_ENDPOINT: String = "endpoint"
    const val EXTRA_MESSAGE_ID: String = "id"
    const val EXTRA_BYTES_MESSAGE: String = "bytesMessage"
    const val EXTRA_REASON: String = "reason"
    const val EXTRA_PI: String = "pi"

    /** The deep link that opens the system's distributor picker. */
    const val LINK: String = "unifiedpush://link"

    /** The four actions this app's receiver must declare to work at all. */
    val CONNECTOR_ACTIONS: List<String> = listOf(
        ACTION_NEW_ENDPOINT,
        ACTION_REGISTRATION_FAILED,
        ACTION_UNREGISTERED,
        ACTION_MESSAGE,
    )

    /** A registration token may be at most this many bytes. */
    const val MAX_TOKEN: Int = 100

    /** An endpoint may be at most this many bytes. */
    const val MAX_ENDPOINT: Int = 1000

    /**
     * The largest message a distributor will carry.
     *
     * A Rime envelope is 51 bytes, so this is never close — it is here so a
     * test can assert the margin rather than assume it.
     */
    const val MAX_MESSAGE: Int = 4096

    // Reasons a registration can fail, by the specification's names.
    const val REASON_INTERNAL_ERROR: String = "INTERNAL_ERROR"
    const val REASON_NETWORK: String = "NETWORK"
    const val REASON_ACTION_REQUIRED: String = "ACTION_REQUIRED"
    const val REASON_VAPID_REQUIRED: String = "VAPID_REQUIRED"

    /**
     * A per-machine registration token.
     *
     * **One UnifiedPush instance per paired machine**, and that is what makes
     * the envelope able to carry no machine identifier at all: the token comes
     * back on every message, it never leaves this phone, and it is what says
     * which machine sent one. It is random rather than derived from the device
     * id so that two paired machines cannot be correlated by anybody who sees
     * both tokens.
     */
    fun newToken(): String {
        val raw = ByteArray(16)
        SecureRandom().nextBytes(raw)
        return com.rimeos.remote.core.Base64Url.encode(raw)
    }

    /** Whether a token is one this app could have issued. */
    fun isToken(token: String?): Boolean =
        !token.isNullOrEmpty() &&
            token.toByteArray(Charsets.UTF_8).size <= MAX_TOKEN &&
            token.all { it.isLetterOrDigit() || it == '-' || it == '_' }

    /**
     * What to do with an endpoint a distributor just handed over.
     *
     * Checked here rather than at the desktop alone, so a phone does not burn
     * a round trip to be told what it could have seen: `push.rs`'s
     * `Endpoint::parse` applies the same rules, and both must refuse the same
     * things. https-only is the one worth stating — a plaintext endpoint is a
     * capability to notify this phone, travelling in the clear.
     */
    fun checkEndpoint(endpoint: String?): String? {
        val e = endpoint?.trim().orEmpty()
        if (e.isEmpty()) return null
        if (e.toByteArray(Charsets.UTF_8).size > MAX_ENDPOINT) return null
        if (!e.startsWith("https://")) return null
        if (e.any { it.isISOControl() || it == ' ' }) return null
        if (e.contains('#')) return null
        val authority = e.removePrefix("https://").substringBefore('/').substringBefore('?')
        if (authority.isEmpty() || authority.contains('@')) return null
        return e
    }

    /** Why a registration failed, and what the user can do about it. */
    data class Failure(val reason: String?) {
        /**
         * A sentence naming the cause.
         *
         * Every branch says what the user can actually do. "Registration
         * failed" on its own is the message that makes somebody reinstall the
         * app to fix a distributor that is not signed in.
         */
        fun explain(distributor: String): String = when (reason) {
            REASON_NETWORK ->
                "$distributor could not reach its push server. Notifications will arrive when it can."
            REASON_ACTION_REQUIRED ->
                "$distributor needs you to finish setting it up — open it and sign in."
            REASON_VAPID_REQUIRED ->
                "$distributor requires VAPID, which Rime Remote does not use: its notifications " +
                    "are already encrypted end to end. Choose another distributor, such as ntfy."
            REASON_INTERNAL_ERROR ->
                "$distributor refused the registration and did not say why."
            else ->
                "$distributor refused the registration."
        }

        /**
         * Whether waiting will fix it.
         *
         * Only a network failure is worth retrying; the other three need the
         * user, and an app that retried them would be an app that spins.
         */
        val isTransient: Boolean get() = reason == REASON_NETWORK
    }

    /**
     * Whether a message must be acknowledged, and with what.
     *
     * The specification: a message carrying an `id` MUST be acknowledged, and
     * a distributor may retry one that is not — and may drop an *endpoint*
     * whose acknowledgement does not arrive within thirty seconds. So an ack
     * that is skipped costs notifications later rather than now, which is
     * exactly the kind of failure nobody connects to its cause.
     *
     * Returns the id to acknowledge, or `null` when there is nothing to do.
     */
    fun ackFor(messageId: String?): String? =
        messageId?.takeIf { it.isNotEmpty() && it.toByteArray(Charsets.UTF_8).size <= MAX_TOKEN }
}
