package com.apexos.remote.data

import android.content.Context
import com.apexos.remote.core.MachineStore
import com.apexos.remote.core.PairedMachine
import com.apexos.remote.security.KeystoreSecretBox
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Where the machine list lives on disk.
 *
 * One JSON file in `filesDir`, which is app-private storage. Not
 * `SharedPreferences` — it has no schema, its XML is read by anything with the
 * app's uid, and it is backed up by default. Not external storage, which is
 * world-readable by design. The file holds public keys, machine names and the
 * *sealed* device secrets; `SecretStorageTest` in `:core` is the proof that
 * nothing else gets into it.
 */
class MachineRepository(context: Context) {
    private val file = File(context.filesDir, FILE_NAME)

    suspend fun load(): MachineStore = withContext(Dispatchers.IO) {
        if (!file.exists()) {
            MachineStore()
        } else {
            runCatching { MachineStore.decode(file.readText()) }.getOrElse {
                // A corrupt store is not a reason to lose the app. It IS a
                // reason to make the user re-pair, which is ten seconds and
                // produces a fresh key — and silently starting empty is the
                // honest outcome, because nothing in a damaged file can be
                // trusted to name a machine correctly.
                MachineStore()
            }
        }
    }

    suspend fun save(store: MachineStore) = withContext(Dispatchers.IO) {
        // Written beside and renamed, so a kill mid-write leaves the previous
        // list rather than half of the new one. A half-written store is a
        // device that cannot connect to anything.
        val scratch = File(file.parentFile, "$FILE_NAME.new")
        scratch.writeText(store.encode())
        check(scratch.renameTo(file)) { "could not replace $FILE_NAME" }
    }

    /** Forget a machine, and the keystore key that would have unsealed it. */
    suspend fun forget(machine: PairedMachine) {
        save(load().without(machine.deviceId))
        withContext(Dispatchers.IO) { KeystoreSecretBox.forget(machine.deviceId) }
    }

    private companion object {
        const val FILE_NAME = "machines.json"
    }
}
