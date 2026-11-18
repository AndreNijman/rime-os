package com.rimeos.remote.device

import com.rimeos.remote.core.MachineStore
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Pairing
import com.rimeos.remote.core.SecretBox
import com.rimeos.remote.core.Session
import com.rimeos.remote.core.StaticKey
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.pairing.GatedBox
import com.rimeos.remote.pairing.PairingService
import kotlinx.coroutines.runBlocking

/**
 * A phone paired with the real desktop, for every suite that needs one.
 *
 * ## The one thing on this phone that is faked, named once
 *
 * [PlainBox]. `KeystoreSecretBox` gates the device key behind a
 * `BiometricPrompt`, and no `adb` verb presents a fingerprint on real
 * hardware, so the box handed to [PairingService] here protects nothing.
 * Everything else is real: the radio, the two daemons, the Noise handshakes,
 * the device store on disk, and `PairingService` itself — the production
 * class, not a copy of it.
 *
 * It lives in one file because it is the substitution the suite has to keep
 * being honest about. Two copies would be two places for the `false` below to
 * drift into a `true`, and the whole value of that flag is that it is not
 * flattering.
 */
object Paired {

    /**
     * A box that seals nothing.
     *
     * Not `null`, and not a branch in `PairingService`: the production path has
     * to run, and the production path seals. What changes is only where the
     * protection comes from — here, nowhere.
     */
    class PlainBox : SecretBox {
        override val describe: String = "an unprotected test box"

        override fun seal(plaintext: ByteArray): ByteArray = plaintext.copyOf()

        override fun open(ciphertext: ByteArray): ByteArray = ciphertext.copyOf()
    }

    val service = PairingService()

    /** Pair this phone with the desktop, using an offer the daemon just minted. */
    fun machine(name: String, from: Computer = Desktop): PairedMachine = runBlocking {
        val offer = Pairing.decodeOffer(from.offerPayload())
        service.pair(
            offer = offer,
            deviceName = name,
            // **false**, and the honesty is the point. `user_verification`
            // says a second factor gates this device's key; a `PlainBox` gates
            // nothing, so claiming otherwise would make the desktop record a
            // protection that does not exist and then let this suite assert
            // that it had. P1-051's sixth criterion is met by the app refusing
            // to open without the prompt at all, which is observed elsewhere
            // and cannot be observed from here.
            boxFor = { GatedBox(PlainBox(), userVerification = false) },
            nowMs = System.currentTimeMillis(),
        )
    }

    /**
     * The device key back out of the record, through the SAME call the app
     * makes — `MachineStore.identityFor`, with the same box that sealed it.
     * Nothing here reaches around the store.
     */
    fun identityOf(machine: PairedMachine): StaticKey =
        MachineStore().identityFor(machine, PlainBox())

    /** A fresh authenticated connection to [machine]. Blocking; call it off the main thread. */
    fun session(machine: PairedMachine, identity: StaticKey = identityOf(machine)): Session =
        runBlocking { service.connect(machine, identity) }

    /** A control link, which opens its connections on demand. */
    fun linkTo(machine: PairedMachine): MachineLink {
        val identity = identityOf(machine)
        return MachineLink({ session(machine, identity) })
    }
}
