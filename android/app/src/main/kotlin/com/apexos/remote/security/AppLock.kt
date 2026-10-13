package com.apexos.remote.security

import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.fragment.app.FragmentActivity
import com.apexos.remote.core.StaticKey
import com.apexos.remote.core.InMemoryStaticKey
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import javax.crypto.Cipher

/**
 * P1-053's "app lock via biometrics or device credential", and P1-051's second
 * factor, which are the same mechanism seen from two directions.
 *
 * The lock is not a screen that hides content: it is the key itself. Unwrapping
 * the device identity requires a `Cipher` the keystore authorised, and the
 * keystore authorises one only after a `BiometricPrompt` succeeds — so an app
 * that skipped the prompt would not be an app showing secrets it should not, it
 * would be an app that cannot open a session at all. That is the difference
 * between a lock and a curtain, and it is why there is no `isUnlocked` flag
 * anywhere in this file for something to get wrong.
 */
object AppLock {
    /**
     * Which authenticators this build will accept.
     *
     * `BIOMETRIC_STRONG` and not `BIOMETRIC_WEAK`: a weak biometric cannot
     * release a keystore key, so accepting one would produce a prompt that
     * succeeds and then a `Cipher` that still refuses.
     */
    const val AUTHENTICATORS: Int =
        BiometricManager.Authenticators.BIOMETRIC_STRONG or
            BiometricManager.Authenticators.DEVICE_CREDENTIAL

    /** Whether this device can gate a key at all. */
    fun canAuthenticate(manager: BiometricManager): Boolean =
        manager.canAuthenticate(AUTHENTICATORS) == BiometricManager.BIOMETRIC_SUCCESS

    /**
     * Show the prompt, and return the device identity it unlocked.
     *
     * The cipher is initialised **before** the prompt and carried through it,
     * because a per-use keystore key authorises one cipher rather than a period
     * of time. The unwrap then happens on the object the prompt hands back.
     */
    suspend fun unlock(
        activity: FragmentActivity,
        box: KeystoreSecretBox,
        sealed: ByteArray,
        machineName: String,
    ): StaticKey {
        val cipher = box.beginOpen(sealed)
        val authorised = prompt(activity, cipher, machineName)
        return InMemoryStaticKey(box.finishOpen(authorised, sealed))
    }

    private suspend fun prompt(
        activity: FragmentActivity,
        cipher: Cipher,
        machineName: String,
    ): Cipher = suspendCancellableCoroutine { continuation ->
        val info = BiometricPrompt.PromptInfo.Builder()
            .setTitle("Unlock APEX Remote")
            // Named, because the whole point of the gate is that the person
            // holding the phone knows which machine is about to be reachable.
            .setSubtitle("Connecting to $machineName")
            .setAllowedAuthenticators(AUTHENTICATORS)
            .build()
        val prompt = BiometricPrompt(
            activity,
            androidx.core.content.ContextCompat.getMainExecutor(activity),
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    val authorised = result.cryptoObject?.cipher
                    if (authorised == null) {
                        // Never expected, and refused rather than worked
                        // around: falling back to the un-authorised cipher
                        // would turn the lock into a curtain.
                        continuation.resumeWithException(
                            IllegalStateException("the prompt returned no authorised cipher"),
                        )
                    } else {
                        continuation.resume(authorised)
                    }
                }

                override fun onAuthenticationError(code: Int, message: CharSequence) {
                    continuation.resumeWithException(AppLockRefused(message.toString(), code))
                }

                // `onAuthenticationFailed` is a finger the sensor did not
                // recognise, not a refusal. The prompt stays up and the user
                // tries again, so there is nothing to resume.
            },
        )
        prompt.authenticate(info, BiometricPrompt.CryptoObject(cipher))
        continuation.invokeOnCancellation { prompt.cancelAuthentication() }
    }
}

/** The user declined, or the system refused. */
class AppLockRefused(message: String, val code: Int) : Exception(message)
