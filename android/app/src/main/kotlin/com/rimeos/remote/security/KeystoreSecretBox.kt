package com.rimeos.remote.security

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import com.rimeos.remote.core.SecretBox
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec

/**
 * The device key's protection: an AES key the app can use but never read.
 *
 * P1-053 asks that no private key reach insecure app storage, and P1-051 asks
 * for a real second factor. Both land here. The Rime Remote device identity is
 * an X25519 private key, and the Android keystore cannot hold an X25519 key for
 * agreement on any API level this app supports — so the key is *wrapped* with
 * an AES-GCM key that the keystore does hold, and the wrapping key never enters
 * the app's address space. What the app gets is a `Cipher` it may drive; what
 * it never gets is bytes it could copy, print or upload.
 *
 * ## Two levels of gate, and why the split is by API and not by taste
 *
 * The wrapping key is created with `setUserAuthenticationRequired(true)`, so
 * using it needs the user present. *How* present differs:
 *
 * * **API 30 and above** — `setUserAuthenticationParameters(0, BIOMETRIC_STRONG
 *   or DEVICE_CREDENTIAL)`. Timeout zero means per-use: every single operation
 *   needs a fresh authentication, and the PIN counts. That is the behaviour
 *   this app wants everywhere.
 * * **API 28 and 29** — the parameter object does not exist, and the older
 *   `setUserAuthenticationValidityDurationSeconds` has one shape that is
 *   per-use (`-1`) and it accepts **biometrics only**. That is not this file's
 *   preference: `androidx.biometric` refuses the combination outright below 30,
 *   in so many words — *"Crypto-based authentication is not supported for
 *   device credential prior to API 30."* [AppLock.authenticators] makes the
 *   same split for the same reason, and the two must not drift apart. A phone
 *   with no enrolled biometric falls back to [insecureFallbackReason] rather
 *   than silently storing a key with no gate at all.
 *
 * ## Both directions are gated, and the earlier claim that sealing was free
 * ## was simply wrong
 *
 * An earlier draft of this file asserted that encryption was exempt from the
 * user-authentication requirement, and used that to avoid a prompt during
 * pairing. It is not exempt. `KeyGenParameterSpec`'s own documentation says of
 * `setUserAuthenticationRequired`: *"This authorization applies only to secret
 * key and private key operations. Public key operations are not restricted."*
 * An AES key is a secret key, so **sealing is gated exactly as opening is** —
 * the exemption is for the public half of an asymmetric pair, which this design
 * does not have. A `seal()` that skipped the prompt would have thrown on a real
 * phone, at the end of pairing, after the desktop had already burnt the token.
 *
 * So there are two prompted pairs: [beginSeal]/[finishSeal] and
 * [beginOpen]/[finishOpen].
 *
 * ## Per-use means the Cipher rides in the prompt
 *
 * A key gated per-use cannot be initialised and then used later: the
 * authentication authorises *one* `Cipher`. `Cipher.init` succeeds — it begins
 * a keystore operation — and it is `doFinal` that needs the operation to have
 * been authorised. So [beginOpen] initialises the cipher, [AppLock] carries it
 * through `BiometricPrompt.CryptoObject`, and the `doFinal` happens on the
 * cipher the prompt hands back. (`UserNotAuthenticatedException` at `init` is
 * the *time-bound* flavour of this gate, which this file deliberately does not
 * use: a window during which the key works is a window during which a phone
 * taken out of a hand works.)
 */
class KeystoreSecretBox private constructor(
    private val key: SecretKey,
    /**
     * Whether this key really is gated — read back off the key, not echoed
     * from what the caller asked for.
     *
     * The difference matters: [describe] and the `user_verification` flag in
     * the pairing request are both claims made to somebody else, and a claim
     * assembled from the caller's own intention is the exact "I put it
     * somewhere safe" statement this design refuses to make.
     */
    val authenticationIsRequired: Boolean,
) : SecretBox {

    override val describe: String
        get() = if (authenticationIsRequired) {
            "the device keystore, behind a biometric or device credential"
        } else {
            "the device keystore, with no user gate on this device"
        }

    /**
     * Wrap the device key, on a box that needs no prompt.
     *
     * Only reachable when [authenticationIsRequired] is false — a phone with no
     * screen lock. On a gated key the keystore refuses this at `doFinal`, which
     * is why [beginSeal] exists and why `MachineStore.record` is handed an
     * already-authorised box rather than this one.
     */
    override fun seal(plaintext: ByteArray): ByteArray =
        finishSeal(beginSeal(), plaintext)

    /**
     * Unwrap directly. Same rule: only for an ungated key.
     */
    override fun open(ciphertext: ByteArray): ByteArray =
        finishOpen(beginOpen(ciphertext), ciphertext)

    /** An initialised encrypting cipher, ready to go into a `CryptoObject`. */
    fun beginSeal(): Cipher = Cipher.getInstance(TRANSFORMATION).apply {
        // No IV is supplied. A keystore key created with randomized encryption
        // required refuses one; the keystore chooses it, and it is read back
        // off the cipher afterwards.
        init(Cipher.ENCRYPT_MODE, key)
    }

    /**
     * Finish a wrap on the cipher the prompt authorised.
     *
     * The IV goes in front of the ciphertext because that is the only place it
     * can come from at unwrap time — the keystore chose it and nothing else
     * recorded it.
     */
    fun finishSeal(cipher: Cipher, plaintext: ByteArray): ByteArray {
        val iv = cipher.iv
        check(iv.size == IV_BYTES) { "the keystore produced a ${iv.size}-byte IV" }
        // A copy, because `SecretBox.seal` promises not to retain the array it
        // is given and the caller wipes it the instant this returns.
        return iv + cipher.doFinal(plaintext.copyOf())
    }

    /**
     * An initialised decrypting cipher for [ciphertext], ready to go into a
     * `BiometricPrompt.CryptoObject`.
     */
    fun beginOpen(ciphertext: ByteArray): Cipher {
        require(ciphertext.size > IV_BYTES) {
            "a sealed device key is at least ${IV_BYTES + 1} bytes; this one is ${ciphertext.size}"
        }
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, ciphertext, 0, IV_BYTES))
        return cipher
    }

    /** Finish an unwrap on the cipher the prompt authorised. */
    fun finishOpen(cipher: Cipher, ciphertext: ByteArray): ByteArray =
        cipher.doFinal(ciphertext, IV_BYTES, ciphertext.size - IV_BYTES)

    companion object {
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val PROVIDER = "AndroidKeyStore"
        private const val IV_BYTES = 12
        private const val TAG_BITS = 128

        /** One alias per machine, so revoking one pairing cannot open another. */
        fun aliasFor(deviceId: String): String = "rime-remote/device/$deviceId"

        /**
         * Load the wrapping key for a device, creating it if there is none.
         *
         * [requireAuthentication] false is for the case where the phone can
         * enrol nothing at all; the caller is expected to have told the user
         * so. What comes back reports the gate the key *has*, read off the key
         * itself, which is not always the gate that was asked for — a key
         * created by an older build of this app is the obvious case.
         */
        fun forDevice(deviceId: String, requireAuthentication: Boolean = true): KeystoreSecretBox =
            load(aliasFor(deviceId), requireAuthentication)

        /**
         * The companion key's alias for a machine: never the device key's.
         *
         * Its own alias because [forDevice] returns whatever key already sits
         * under an alias, gate and all: a companion key sealed under the
         * device alias would either be gated (and never open after a restart)
         * or would leave the device key ungated. Neither is acceptable.
         */
        fun companionAliasFor(deviceId: String): String = "rime-remote/companion/$deviceId"

        /**
         * The box for a machine's companion key: NO user authentication, by
         * design, so a restarted phone can open it from a boot receiver before
         * anyone has unlocked the app. Refuses to hand back a gated key.
         */
        fun forCompanion(deviceId: String): KeystoreSecretBox {
            val box = load(companionAliasFor(deviceId), requireAuthentication = false)
            check(!box.authenticationIsRequired) { "the companion key's alias holds a gated key" }
            return box
        }

        /** Forget a machine's companion wrapping key. A sealed companion key becomes noise. */
        fun forgetCompanion(deviceId: String) {
            val store = KeyStore.getInstance(PROVIDER).apply { load(null) }
            val alias = companionAliasFor(deviceId)
            if (store.containsAlias(alias)) store.deleteEntry(alias)
        }

        private fun load(alias: String, requireAuthentication: Boolean): KeystoreSecretBox {
            val store = KeyStore.getInstance(PROVIDER).apply { load(null) }
            val existing = store.getKey(alias, null) as SecretKey?
            if (existing != null) return KeystoreSecretBox(existing, gateOn(existing))
            val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
            val spec = KeyGenParameterSpec.Builder(
                alias,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                // The keystore picks the IV. Anything else is refused on a key
                // that requires randomized encryption, which this one does.
                .setRandomizedEncryptionRequired(true)
                .apply {
                    if (!requireAuthentication) return@apply
                    setUserAuthenticationRequired(true)
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                        // Timeout 0 is per-use, and DEVICE_CREDENTIAL means a
                        // PIN counts — which matters on a phone whose owner has
                        // no usable fingerprint.
                        setUserAuthenticationParameters(
                            0,
                            KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL,
                        )
                    } else {
                        // API 28-29. `-1` is per-use and biometrics only, which
                        // is not a preference: androidx.biometric refuses a
                        // CryptoObject with DEVICE_CREDENTIAL below API 30.
                        @Suppress("DEPRECATION")
                        setUserAuthenticationValidityDurationSeconds(-1)
                    }
                    // A new fingerprint enrolled after pairing invalidates this
                    // key, which is the point: somebody who can add a finger to
                    // an unlocked phone must not thereby inherit a paired
                    // device identity. Re-pairing is the recovery.
                    setInvalidatedByBiometricEnrollment(true)
                }
                .build()
            generator.init(spec)
            val created = generator.generateKey()
            return KeystoreSecretBox(created, gateOn(created))
        }

        /**
         * The gate a key actually carries.
         *
         * Asked of the keystore rather than remembered, because the two can
         * differ — an alias created by an older build, a device whose secure
         * lock screen was removed and re-added — and because a security
         * property the app merely believes in is not one.
         *
         * A key the factory will not describe is treated as **ungated**. That
         * is the pessimistic reading and it is the right one: the consequence
         * is an honest [describe] and a pairing request that does not promise a
         * second factor, where the optimistic reading would promise one that
         * may not exist.
         */
        private fun gateOn(key: SecretKey): Boolean = try {
            val factory = SecretKeyFactory.getInstance(key.algorithm, PROVIDER)
            (factory.getKeySpec(key, KeyInfo::class.java) as KeyInfo).isUserAuthenticationRequired
        } catch (_: Exception) {
            false
        }

        /** Forget a machine's wrapping key. The sealed bytes become noise. */
        fun forget(deviceId: String) {
            val store = KeyStore.getInstance(PROVIDER).apply { load(null) }
            if (store.containsAlias(aliasFor(deviceId))) store.deleteEntry(aliasFor(deviceId))
        }

        /**
         * Why this device cannot gate a key, or `null` when it can.
         *
         * Stated rather than assumed: a phone with no screen lock has nothing
         * to gate with, and pretending otherwise would be the exact "I put it
         * somewhere safe" claim this design refuses to make.
         */
        fun insecureFallbackReason(canAuthenticate: Boolean): String? =
            if (canAuthenticate) {
                null
            } else {
                "this device has no screen lock or enrolled biometric, so the key protecting " +
                    "your Rime pairing cannot be gated. Set a screen lock and pair again."
            }
    }
}
