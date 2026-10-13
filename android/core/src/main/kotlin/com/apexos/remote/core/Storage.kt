package com.apexos.remote.core

import com.apexos.remote.core.term.AccessoryKey
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
 * One APEX machine this device is paired with.
 *
 * P1-053 asks for "multiple APEX computers", so this is a record in a list and
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
     * Material You instead of the APEX palette.
     *
     * Off by default. Dynamic colour is genuinely nicer on a phone whose owner
     * has chosen a wallpaper, and it is by construction *not* APEX's colours —
     * and this is the app that shows which machine is about to run something as
     * root. So it is offered, and it is not what ships.
     */
    @SerialName("dynamic_colour") val dynamicColour: Boolean = false,

    /**
     * The accessory row above the keyboard, which P1-055 asks to be
     * configurable.
     *
     * Here and not in a `SharedPreferences` file, and that is the whole reason
     * this field exists on [Settings] rather than anywhere more convenient:
     * `AppStorage` is the single writer to app storage, `InsecureStorageTest`
     * proves what reaches the disk by walking every file in the directory, and
     * a scan is only evidence about the bytes it can see.
     * `tools/no-second-write-path.sh` fails the build if a second writer
     * appears.
     *
     * Empty means "the shipped row", which is `AccessoryKeys.DEFAULT`. Storing
     * the default explicitly would freeze it: a later build that improved the
     * row would leave every existing phone on the old one.
     */
    @SerialName("accessory_row") val accessoryRow: List<AccessoryKey> = emptyList(),

    /**
     * Terminal text size in scaled pixels.
     *
     * Small, because eighty columns on a phone is the point: a terminal that
     * reflowed to forty columns would make every TUI redraw itself into a
     * shape none of them are designed for.
     */
    @SerialName("terminal_sp") val terminalSp: Int = 11,
)

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

        private val json = Json {
            encodeDefaults = true
            ignoreUnknownKeys = true
            prettyPrint = false
        }

        fun decode(text: String): MachineStore = json.decodeFromString(serializer(), text)

        /**
         * Build the record for a machine that has just been paired.
         *
         * The only place a device secret is ever handed to a [SecretBox], and
         * the only place `exportSecretForSealing` is called in shipped code.
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
