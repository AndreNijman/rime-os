package com.apexos.remote.security

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.UserNotAuthenticatedException
import com.apexos.remote.core.SecretBox
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The device key's protection: an AES key the app can use but never read.
 *
 * P1-053 asks that no private key reach insecure app storage, and P1-051 asks
 * for a real second factor. Both land here. The APEX Remote device identity is
 * an X25519 private key, and the Android keystore cannot hold an X25519 key for
 * agreement on any API level this app supports — so the key is *wrapped* with
 * an AES-GCM key that the keystore does hold, and the wrapping key never enters
 * the app's address space. What the app gets is a `Cipher` it may drive; what
 * it never gets is bytes it could copy, print or upload.
 *
 * ## Two levels of gate, and why the split is by API and not by taste
 *
 * The wrapping key is created with `setUserAuthenticationRequired(true)`, so
 * unwrapping needs the user present. *How* present differs:
 *
 * * **API 30 and above** — `setUserAuthenticationParameters(0, BIOMETRIC_STRONG
 *   or DEVICE_CREDENTIAL)`. Timeout zero means per-use: every single unwrap
 *   needs a fresh authentication, and the PIN counts. That is the behaviour
 *   this app wants everywhere.
 * * **API 28 and 29** — the parameter object does not exist, and the older
 *   `setUserAuthenticationValidityDurationSeconds` has one shape that is
 *   per-use (`-1`) and it accepts **biometrics only**. A device credential
 *   cannot satisfy a per-use gate there at all. So those two releases get the
 *   per-use biometric gate, which is strictly the stronger of the two options
 *   available, and a phone with no enrolled biometric falls back to
 *   [insecureFallbackReason] rather than silently storing a key with no gate.
 *
 * ## Per-use means the Cipher rides in the prompt
 *
 * A key gated per-use cannot be initialised and then used later: the
 * authentication authorises *one* `Cipher`. So [beginOpen] initialises the
 * cipher, [AppLock] carries it through `BiometricPrompt.CryptoObject`, and the
 * `doFinal` happens on the cipher the prompt hands back. Initialising a cipher
 * that needs authentication throws [UserNotAuthenticatedException], and that
 * throw is the signal to show the prompt — not an error.
 *
 * ## Sealing is not gated; opening is
 *
 * [seal] uses a `Cipher` in encrypt mode, which the keystore permits without
 * authentication even on a key that requires it for decryption. That is
 * deliberate rather than incidental: gating the seal as well would mean two
 * biometric prompts during a single pairing — one to store the new key, one to
 * use it — for no gain, because the thing being protected is the *reading* of
 * an existing key and there is nothing yet to read.
 */
class KeystoreSecretBox private constructor(
    private val key: SecretKey,
    val authenticationIsRequired: Boolean,
) : SecretBox {

    override val describe: String
        get() = if (authenticationIsRequired) {
            "the device keystore, behind a biometric or device credential"
        } else {
            "the device keystore, with no user gate on this device"
        }

    /**
     * Wrap the device key.
     *
     * The IV is chosen by the keystore, not by this code: a keystore key is
     * created with randomized encryption required, and supplying an IV is
     * refused. It is read back off the initialised cipher and stored in front
     * of the ciphertext, which is the only place it can come from at unwrap
     * time.
     */
    override fun seal(plaintext: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val iv = cipher.iv
        check(iv.size == IV_BYTES) { "the keystore produced a ${iv.size}-byte IV" }
        // A copy, because `SecretBox.seal` promises not to retain the array it
        // is given and the caller wipes it the instant this returns.
        return iv + cipher.doFinal(plaintext.copyOf())
    }

    /**
     * Unwrap directly. Throws [UserNotAuthenticatedException] on a gated key,
     * which is [AppLock]'s cue rather than a failure.
     */
    override fun open(ciphertext: ByteArray): ByteArray =
        finishOpen(beginOpen(ciphertext), ciphertext)

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
        fun aliasFor(deviceId: String): String = "apex-remote/device/$deviceId"

        /**
         * Load the wrapping key for a device, creating it if there is none.
         *
         * [requireAuthentication] false is for the case where the phone can
         * enrol nothing at all; the caller is expected to have told the user
         * so, and [authenticationIsRequired] records which happened so the
         * pairing request can report it honestly rather than always claiming a
         * second factor it may not have.
         */
        fun forDevice(deviceId: String, requireAuthentication: Boolean = true): KeystoreSecretBox {
            val alias = aliasFor(deviceId)
            val store = KeyStore.getInstance(PROVIDER).apply { load(null) }
            val existing = store.getKey(alias, null) as SecretKey?
            if (existing != null) {
                return KeystoreSecretBox(existing, requireAuthentication)
            }
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
                        // API 28-29. `-1` is per-use and biometrics only; there
                        // is no per-use device-credential option on these
                        // releases, so this is the strongest gate available.
                        @Suppress("DEPRECATION")
                        setUserAuthenticationValidityDurationSeconds(-1)
                    }
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
                        // A new fingerprint enrolled after pairing invalidates
                        // this key, which is the point: somebody who can add a
                        // finger to an unlocked phone must not thereby inherit
                        // a paired device identity. Re-pairing is the recovery.
                        setInvalidatedByBiometricEnrollment(true)
                    }
                }
                .build()
            generator.init(spec)
            return KeystoreSecretBox(generator.generateKey(), requireAuthentication)
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
                    "your APEX pairing cannot be gated. Set a screen lock and pair again."
            }
    }
}
