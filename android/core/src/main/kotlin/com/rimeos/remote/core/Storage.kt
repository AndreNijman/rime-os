package com.rimeos.remote.core

import com.rimeos.remote.core.term.AccessoryKey
import com.rimeos.remote.core.term.AccessoryKeys
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * Where a paired machine is remembered, and where its key material is not.
 *
 * P1-053's last criterion is "no secrets, private keys or session plaintext
 * are written to insecure app storage". Two things make that true here, and
 * only one of them is a promise:
 *
 * * **The shape of the record.** [PairedMachine] has no field that can hold a
 *   private key. The device's secret is present only as [PairedMachine.sealed]
 *   — whatever a [SecretBox] produced — and there is no code path from a
 *   `StaticKey` to a serialisable field. A future change that tried to store
 *   the raw key would have to add a field, which is a diff a reviewer sees.
 * * **The test.** `SecretStorageTest` seals a real keypair, writes the store,
 *   and searches the written bytes for the private key in every encoding it
 *   could plausibly take. That is a hostile check of the bytes that land on
 *   disk, not an assertion about where the code meant to put them.
 *
 * What a [SecretBox] *is* differs by platform, and that is the point of the
 * seam: on Android it is an AES-GCM key in the platform keystore created with
 * `setUserAuthenticationRequired(true)`, so opening the box needs a biometric
 * or the device credential and the key itself never enters the app's address
 * space. On a JVM — tests, and any future desktop client — it is whatever the
 * caller supplies. Neither this file nor the handshake changes between them.
 */
interface SecretBox {
    /**
     * Wrap plaintext so that only this device, with the user present, can
     * unwrap it.
     *
     * **The implementation must not retain [plaintext].** The caller wipes
     * that array the instant this returns — a private key living on in a
     * long-lived object is the thing this whole interface exists to prevent —
     * so an implementation that kept the reference would find its own copy
     * zeroed underneath it. Copy what you need before returning.
     */
    fun seal(plaintext: ByteArray): ByteArray

    /** Unwrap, or throw. A refused biometric is a throw, and that is correct. */
    fun open(ciphertext: ByteArray): ByteArray

    /** A short name for what is doing the sealing, for display and for logs. */
    val describe: String
}

/**
 * One Rime machine this device is paired with.
 *
 * P1-053 asks for "multiple Rime computers", so this is a record in a list and
 * not a singleton: everything that identifies a machine, and the device
 * identity used with *that* machine, live together. A device key per machine
 * rather than one shared key is deliberate — revoking the phone on a work
 * laptop should not hand anyone the key it uses with the home one.
 */
@Serializable
data class PairedMachine(
    /** The desktop's own id for this device: the first 16 characters of the device key. */
    @SerialName("device_id") val deviceId: String,
    /** What the machine calls itself. Display only. */
    val machine: String,
    /** The desktop's static public key, base64url. Pinned at pairing; never changes. */
    @SerialName("desktop_key") val desktopKey: String,
    /** This device's public key for this machine, base64url. */
    @SerialName("device_key") val deviceKey: String,
    /**
     * This device's *private* key for this machine, after a [SecretBox] has
     * had it. Opaque bytes, base64url. Never the key.
     */
    @SerialName("sealed_device_secret") val sealed: String,
    /** LAN hints from the pairing offer; a hint, re-learned on every connection. */
    val lan: List<String> = emptyList(),
    /** The relay to fall back to, or `null` for a LAN-only pairing. */
    val relay: String? = null,
    @SerialName("paired_ms") val pairedMs: Long,
    /**
     * How this machine wakes this phone, when it can (P1-058).
     *
     * `null` until a UnifiedPush distributor has given this app an endpoint
     * for this machine and the machine has accepted it. A machine that is too
     * old to know the verb stays `null` for ever, and the app falls back to
     * polling while it is open — which is what it did before push existed.
     */
    val push: PushRegistration? = null,
    /**
     * The path the last successful connection took: `lan:<address>` or
     * `relay` (`Route.key`), or null before the first one.
     *
     * Tried first next time, because most connections are from the same
     * place as the last one — see `ConnectPlan`. A hint and nothing more: it
     * names a path, never a secret, and a stale one costs one parallel dial.
     */
    @SerialName("last_route") val lastRoute: String? = null,
    /**
     * When this phone last connected to this machine, unix milliseconds, or 0.
     *
     * Decides which machine is connected on unlock without a tap, and so
     * which machine's key the one unlock prompt unwraps. See
     * [MachineStore.lastUsed].
     */
    @SerialName("last_used_ms") val lastUsedMs: Long = 0,
    /**
     * This phone's companion key for this machine (contract §2.10), or null.
     *
     * Present only while "Keep working after the phone restarts" is on. Sealed
     * like [sealed], but by a keystore key that needs NO user authentication,
     * under an alias of its own: it has to open after a restart, before
     * anybody has unlocked the app. That is why the machine lets it do so
     * little. See [CompanionKey].
     */
    val companion: CompanionKey? = null,
    /**
     * A companion key existed and this machine has not yet been told to
     * forget it. Set when the switch goes off (or the key is replaced) while
     * the machine is not connected; cleared once `companion_forget` lands on
     * the next full connection.
     */
    @SerialName("companion_forget_owed") val companionForgetOwed: Boolean = false,
) {
    /** The rendezvous this machine is reachable at through a relay. */
    fun rendezvousId(): String = Rendezvous.idFor(Device.checkKey(desktopKey))

    /**
     * Redacted even though the sealed blob is not a secret.
     *
     * A sealed key is still the thing an attacker needs half of, and a record
     * that printed it into logcat would be handing over that half for free.
     */
    override fun toString(): String =
        "PairedMachine(machine=$machine, deviceId=$deviceId, sealed=<${sealed.length} chars>)"
}

/**
 * One machine's companion key: the public half, and the private half sealed.
 *
 * The private half is wrapped by a keystore key with no authentication gate,
 * so a restarted phone can dial with it from a boot receiver. What that
 * weaker gate costs is bounded by the machine, not by this phone: a companion
 * session may send the phone's report, notifications and what is playing, and
 * receive ring, offers and focus, and nothing else (`rime-remoted`'s
 * `companion.rs`). It cannot reach an agent, a terminal, nex or the machine's
 * controls, and the phone never asks it to (`phone/CompanionLink.kt`).
 */
@Serializable
data class CompanionKey(
    /** The public key, base64url, as sent in `companion_register`. */
    @SerialName("public_key") val publicKey: String,
    /** The private key after the ungated [SecretBox] had it. Never the key. */
    @SerialName("sealed_secret") val sealed: String,
    /** The machine answered ok to `companion_register` for this key. */
    val registered: Boolean = false,
) {
    override fun toString(): String =
        "CompanionKey(public=$publicKey, registered=$registered, sealed=<${sealed.length} chars>)"
}


/**
 * What this phone told one machine about how to wake it.
 *
 * ## This holds a key in ordinary app storage, and that is a decision
 *
 * Every other secret in this app is sealed: the device identity key is wrapped
 * by a keystore key created with `setUserAuthenticationRequired(true)`, and
 * `InsecureStorageTest` walks the whole storage directory hunting for its raw
 * bytes. [key] is deliberately **not** treated that way, and the reason is
 * that it could not be.
 *
 * A push arrives at a `BroadcastReceiver` woken by a distributor, in a process
 * that was not running, on a phone that is very likely locked. A key behind a
 * per-use biometric gate is unreachable there — the prompt has nobody to show
 * itself to — so a sealed push key would mean no notification ever arrives on
 * exactly the phones whose owner asked for more protection, silently.
 *
 * What the weaker protection costs is bounded and worth stating in full.
 * Somebody who can read this file can forge a notification that this phone
 * would show, and can decrypt the 22-byte bodies this machine sends: a kind, an
 * adapter code, a session number, a start time and a counter. They cannot
 * connect to the machine, cannot type at an agent, cannot read a prompt, a
 * path or any output, and cannot approve anything — all of that needs the
 * identity key, which is still sealed. Android's own file-based encryption
 * still applies, so the file is unreadable before the phone's first unlock
 * after a boot.
 *
 * Rotating it costs one re-registration, which happens on the next connection.
 */
@Serializable
data class PushRegistration(
    /**
     * The UnifiedPush instance token for this machine.
     *
     * One instance per machine, which is what lets the envelope carry no
     * machine identifier at all: this token comes back on every message and
     * never leaves the phone.
     */
    val token: String,
    /**
     * The endpoint the distributor last gave for this machine.
     *
     * Empty between asking a distributor to register and its answer arriving —
     * which is a real state a phone sits in, and is distinguishable from
     * "never asked", which is a `null` [PairedMachine.push].
     */
    val endpoint: String,
    /** The registration key, base64url of 32 bytes. See the class note. */
    val key: String,
    /**
     * The highest sequence number seen from this machine.
     *
     * Persisted so that a process restart does not accept a replay of
     * everything the phone has already shown.
     */
    @SerialName("last_seq") val lastSeq: Long = 0,
    /**
     * The endpoint this machine has actually been told about.
     *
     * Separate from [endpoint] because the two are written by different things
     * at different times: a distributor rotates [endpoint] by broadcasting to
     * a receiver that cannot reach the machine — the Noise channel needs the
     * device key, which is behind a biometric gate a woken receiver cannot
     * satisfy — so the telling happens on the next connection.
     *
     * Empty means "nothing has been sent yet". Comparing the two is what makes
     * the send idempotent: a phone that reconnects twenty times a day must not
     * spend a round trip re-registering an endpoint the machine already has,
     * and a phone whose distributor rotated its endpoint must send exactly
     * once. Inferring it from [endpoint] alone cannot express either.
     */
    @SerialName("sent_endpoint") val sentEndpoint: String = "",
    /**
     * The package name of the distributor that carries this registration.
     *
     * Remembered rather than re-resolved, and only because of acknowledgements.
     * A message must be acknowledged to the distributor that delivered it, and
     * Android does not say which app sent a broadcast — so on a phone with two
     * distributors installed the only way to answer the right one is to
     * remember which one was asked. An acknowledgement that goes to the wrong
     * app is not an error anybody sees: the message is redelivered, and after
     * thirty seconds the endpoint itself may be dropped.
     *
     * Empty for a registration made before this field existed, which resolves
     * to the same guess as before.
     */
    @SerialName("distributor") val distributor: String = "",
) {
    /** Redacted, for the same reason [PairedMachine.toString] is. */
    override fun toString(): String =
        "PushRegistration(endpoint=$endpoint, key=<${key.length} chars>, lastSeq=$lastSeq, " +
            "sent=${sentEndpoint.isNotEmpty()}, distributor=$distributor)"
}

/**
 * What the person holding the phone has chosen about how the app looks.
 *
 * On [MachineStore] rather than in a `SharedPreferences` file, and the reason
 * is the leak test rather than tidiness. `AppStorage` is the only writer to app
 * storage precisely so that `InsecureStorageTest` can walk the whole directory
 * and account for every byte in it; a preferences file would be a second write
 * path, and one the scan never looks at. A boolean is cheap to carry here.
 */
@Serializable
data class Settings(
    /**
     * Material You instead of the Rime palette.
     *
     * Off by default. Dynamic colour is genuinely nicer on a phone whose owner
     * has chosen a wallpaper, and it is by construction *not* Rime's colours —
     * and this is the app that shows which machine is about to run something as
     * root. So it is offered, and it is not what ships.
     */
    @SerialName("dynamic_colour") val dynamicColour: Boolean = false,

    /**
     * The row of keys above the software keyboard.
     *
     * P1-055's criterion says *configurable*, and here is where the
     * configuration lives — for the same reason `dynamicColour` does, and with
     * more force: a scrollback cache, a preferences file or a `DataStore` for
     * the accessory row would each be a second write path into app storage
     * that `InsecureStorageTest`'s walk never looks at.
     *
     * Empty means "whatever this build ships", not "no keys": a stored empty
     * list and an absent key are indistinguishable after a
     * `kotlinx.serialization` default, and a row that vanished because a
     * migration dropped a field would be the worse failure. [accessoryRow]
     * resolves it.
     */
    @SerialName("accessory") val accessory: List<AccessoryKey> = emptyList(),

    /**
     * Terminal text size in scaled points.
     *
     * Not a taste setting. It is the one control that decides how many columns
     * fit, and eighty columns is what every one of these TUIs lays out for —
     * so on a phone held in portrait this is the difference between reading
     * Claude Code's output and reading a wrapped smear of it.
     */
    @SerialName("terminal_text_sp") val terminalTextSp: Float = DEFAULT_TERMINAL_TEXT_SP,

    /**
     * Keep a report on this phone when the app crashes (P1-060).
     *
     * **False by default, and the default is the criterion.** "Crash reporting
     * is consent-based" is met by a switch that starts off, not by one that
     * starts on and can be found. It gates the WRITE, not a send: with this
     * off, a crash produces nothing on disk at all, which is a stronger
     * position than a file kept back — a file that was never created cannot be
     * read by whatever reaches this phone next.
     *
     * Nothing uploads it either way. There is no reporting SDK in this app; see
     * [CrashReport] for why one would be the wrong shape here.
     *
     * On [Settings] and not in a preferences file, for the reason
     * [dynamicColour] is: `AppStorage` is the only writer to app storage so
     * that `InsecureStorageTest` can account for every byte in it.
     */
    @SerialName("crash_reports") val crashReports: Boolean = false,

    /**
     * The apps whose notifications are shown on paired computers
     * (`docs/remote-live-contract.md` §2.7), by package name.
     *
     * Empty by default, and the default is the point: granting this app
     * notification access lets it read every notification on the phone, and
     * this list is what decides that only the ones the person picked, app by
     * app, ever leave it. Package names only; no notification content is ever
     * stored, here or anywhere.
     */
    @SerialName("mirror_apps") val mirrorApps: Set<String> = emptySet(),

    /**
     * Whether a reply typed on a computer to one of those notifications is
     * sent from this phone, through the app's own reply action, as the person
     * (§2.7). Off by default: it lets a computer speak for them in their apps.
     */
    @SerialName("mirror_reply") val mirrorReply: Boolean = false,

    /**
     * Whether what this phone is playing is shown on paired computers
     * (`docs/remote-live-contract.md` §2.8). Off by default.
     */
    @SerialName("media_on") val mediaOn: Boolean = false,

    /**
     * The apps whose playback may show there, by package name: a list of its
     * own, apart from [mirrorApps], because an app whose notifications a
     * person wants on the computer is not always one whose listening they
     * want shown, and the other way round. Empty by default. Package names
     * only; nothing about what played is ever stored.
     */
    @SerialName("media_apps") val mediaApps: Set<String> = emptySet(),

    /**
     * "Control from your computer" (§2.8): while [mediaOn], a paired computer
     * may play, pause, skip and seek what is shown there, as far as the app
     * allows. On by default, because it acts only on what the person already
     * chose to show, and only while the phone is unlocked; [mediaOn] itself
     * stays off until chosen.
     */
    @SerialName("media_control") val mediaControl: Boolean = true,

    /**
     * "Do Not Disturb follows your computer" (`docs/remote-live-contract.md`
     * §2.9): a paired computer may turn this phone's Do Not Disturb on and
     * off, and may follow it. Off by default: it lets a computer change a
     * setting on the phone.
     */
    @SerialName("focus_on") val focusOn: Boolean = false,

    /**
     * "Messages on your computer" (`docs/remote-live-contract.md` §2.11): a
     * paired computer may read this phone's SMS conversations and, while the
     * app is unlocked, send an SMS from it. Off by default: it shows a
     * computer the person's messages and lets it speak for them.
     */
    @SerialName("messages_on") val messagesOn: Boolean = false,

    /**
     * "Keep working after the phone restarts" (`docs/remote-live-contract.md`
     * §2.10): each paired computer holds a second, companion key for this
     * phone, and after a restart, or while the app is locked, the phone keeps
     * the device features running with it. Off by default: it leaves a key
     * on the phone that opens without the person.
     */
    @SerialName("companion_on") val companionOn: Boolean = false,
) {
    /** The configured row, or the shipped one when nothing has been configured. */
    val accessoryRow: List<AccessoryKey> get() = accessory.ifEmpty { AccessoryKeys.DEFAULT }

    companion object {
        /**
         * Twelve points.
         *
         * Measured rather than chosen: at `FontFamily.Monospace` on Android the
         * advance width is 0.6 em, so eighty columns need 576 px, which is
         * inside the 1080 px short edge of every phone this app supports at
         * any reasonable density — and legible at arm's length, which a size
         * that made eighty columns fit a 720 px device would not be.
         */
        const val DEFAULT_TERMINAL_TEXT_SP: Float = 12f
    }
}

/** Every machine this device knows, and how to put one back together. */
@Serializable
data class MachineStore(
    val v: Int = VERSION,
    val machines: List<PairedMachine> = emptyList(),
    val settings: Settings = Settings(),
) {
    fun with(machine: PairedMachine): MachineStore =
        copy(machines = machines.filterNot { it.deviceId == machine.deviceId } + machine)

    fun without(deviceId: String): MachineStore =
        copy(machines = machines.filterNot { it.deviceId == deviceId })

    fun find(deviceId: String): PairedMachine? = machines.firstOrNull { it.deviceId == deviceId }

    /** Change one machine's record, or return this store when it is not here. */
    fun edit(deviceId: String, change: (PairedMachine) -> PairedMachine): MachineStore =
        if (find(deviceId) == null) this else copy(machines = machines.map { if (it.deviceId == deviceId) change(it) else it })

    /** The machines a restarted phone can reach with a companion key. */
    fun companions(): List<PairedMachine> = machines.filter { it.companion?.registered == true }

    /**
     * The machine to connect on unlock: the one used most recently, else the
     * only one there is, else none.
     *
     * Only one, and that is the key design rather than a shortcut: each
     * machine's device key is wrapped by a keystore key gated PER USE, so one
     * biometric prompt authorises exactly one unwrap. Connecting every paired
     * machine on unlock would mean one prompt each.
     */
    fun lastUsed(): PairedMachine? =
        machines.filter { it.lastUsedMs > 0 }.maxByOrNull { it.lastUsedMs }
            ?: machines.singleOrNull()

    /**
     * Record a successful connection: the path it took and when.
     *
     * A no-op store (the same instance) when nothing would change except the
     * time and the time moved by less than [USED_GRANULARITY_MS] — the caller
     * compares instances to skip a write, because a phone that reconnects
     * after every network change should not rewrite its store every time.
     */
    fun connected(deviceId: String, route: String, nowMs: Long): MachineStore {
        val m = find(deviceId) ?: return this
        if (m.lastRoute == route && nowMs - m.lastUsedMs in 0 until USED_GRANULARITY_MS) return this
        return copy(machines = machines.map { if (it.deviceId == deviceId) it.copy(lastRoute = route, lastUsedMs = nowMs) else it })
    }

    /**
     * Replace a machine's LAN hints with what the machine says now.
     *
     * The same instance back when they already match, or when [lan] is empty:
     * an empty list is as likely a machine momentarily between networks as
     * one with no network at all, and a stale hint costs one parallel dial
     * where an empty one could cost the LAN path entirely.
     */
    fun withLan(deviceId: String, lan: List<String>): MachineStore {
        if (lan.isEmpty()) return this
        val m = find(deviceId) ?: return this
        if (m.lan == lan) return this
        return copy(machines = machines.map { if (it.deviceId == deviceId) it.copy(lan = lan) else it })
    }

    /**
     * The device identity for a machine, unsealed.
     *
     * Returns a [StaticKey] and not bytes. The caller gets something it can
     * hand to a handshake and nothing it can write down, which is the same
     * distinction [StaticKey] itself draws and the reason this returns through
     * that interface rather than through a `ByteArray`.
     */
    fun identityFor(machine: PairedMachine, box: SecretBox): StaticKey {
        val raw = Base64Url.decode(machine.sealed)
            ?: throw DeviceException("the stored key for ${machine.machine} is not base64url")
        return InMemoryStaticKey(box.open(raw))
    }

    fun encode(): String = json.encodeToString(serializer(), this)

    companion object {
        const val VERSION = 1

        /** A connection within a minute of the last one does not rewrite the store. */
        const val USED_GRANULARITY_MS: Long = 60_000

        private val json = Json {
            encodeDefaults = true
            ignoreUnknownKeys = true
            prettyPrint = false
        }

        fun decode(text: String): MachineStore = json.decodeFromString(serializer(), text)

        /**
         * Build the record for a machine that has just been paired.
         *
         * One of the two places a device secret is ever handed to a
         * [SecretBox] and `exportSecretForSealing` is called in shipped code.
         * The other is `CompanionKeys.mint`, for the companion key.
         */
        fun record(
            identity: InMemoryStaticKey,
            offer: PairingOffer,
            answer: PairingAnswer,
            box: SecretBox,
            nowMs: Long,
        ): PairedMachine {
            val secret = identity.exportSecretForSealing()
            val sealed = try {
                box.seal(secret)
            } finally {
                // The one copy this code makes, wiped as soon as it is
                // sealed. It does not make the JVM's earlier copies go away —
                // `ByteArray` is not `SecretValue` and a garbage collector
                // moves things — but leaving a live reference to a private key
                // in a long-lived object would be a different and avoidable
                // problem.
                secret.fill(0)
            }
            return PairedMachine(
                deviceId = identity.deviceId(),
                machine = answer.machine ?: offer.machine,
                desktopKey = offer.key,
                deviceKey = identity.publicKeyText(),
                sealed = Base64Url.encode(sealed),
                lan = offer.lan,
                relay = offer.relay,
                pairedMs = nowMs,
            )
        }
    }
}
