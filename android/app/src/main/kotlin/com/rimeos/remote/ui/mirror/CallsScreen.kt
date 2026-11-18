package com.rimeos.remote.ui.mirror

import android.Manifest
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import com.rimeos.remote.phone.PhoneCalls

/**
 * "Calls on your computer" (contract §2.12): the call permissions, asked here
 * and only here, and the switch, off until the person turns it on. As for
 * Messages, a phone that installed the app from a file gets no prompt for the
 * call log on Android 13 and later until restricted settings are allowed, so
 * a refusal that came with no prompt says where that is.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CallsScreen(
    on: Boolean,
    onOn: (Boolean) -> Unit,
    /** The permissions may have changed: the machines are told the new `can`. */
    onPermissions: () -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    var state by remember { mutableStateOf(PhoneCalls.canState(context)) }
    var history by remember { mutableStateOf(PhoneCalls.canHistory(context)) }
    var answer by remember { mutableStateOf(PhoneCalls.canAnswer(context)) }
    var dial by remember { mutableStateOf(PhoneCalls.canDial(context)) }
    var names by remember { mutableStateOf(PhoneCalls.canNames(context)) }
    var blocked by remember { mutableStateOf(false) }
    fun reread() {
        state = PhoneCalls.canState(context)
        history = PhoneCalls.canHistory(context)
        answer = PhoneCalls.canAnswer(context)
        dial = PhoneCalls.canDial(context)
        names = PhoneCalls.canNames(context)
        onPermissions()
    }
    // Back from App info: read the grants again.
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { reread() }
    val askCalls = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        reread()
        blocked = result.values.any { !it }
    }
    val askNames = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { reread() }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("Calls on your computer", style = MaterialTheme.typography.titleMedium) },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp, vertical = 8.dp),
        ) {
            Text(
                "Your paired computers show a call while it rings on this phone. From the computer you " +
                    "can answer it, decline it or end it, see recent calls, and call a number from this " +
                    "phone. The sound stays on the phone: talk on the phone or a headset paired with it. " +
                    "Answering and ending work while this app is locked, as on the phone's lock screen; " +
                    "calling a number only while it is unlocked. Nothing is kept on the computer.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Spacer(Modifier.height(16.dp))
            if (!state || !history || !answer || !dial) {
                Text("Allow calls", style = MaterialTheme.typography.titleSmall)
                Text(
                    "Android asks whether this app may see calls, read the call log, answer calls and make calls.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedButton(
                    onClick = {
                        askCalls.launch(
                            arrayOf(
                                Manifest.permission.READ_PHONE_STATE,
                                Manifest.permission.READ_CALL_LOG,
                                Manifest.permission.ANSWER_PHONE_CALLS,
                                Manifest.permission.CALL_PHONE,
                            ),
                        )
                    },
                    modifier = Modifier.padding(top = 8.dp),
                ) { Text("Allow calls") }
                if (blocked) {
                    Text(
                        "Android did not allow all of it. If you installed this app from a file, open App info, " +
                            "tap the menu at the top right, choose Allow restricted settings, then allow Call logs " +
                            "and Phone under Permissions.",
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                    TextButton(onClick = {
                        runCatching {
                            context.startActivity(
                                Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null))
                                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                            )
                        }
                    }) { Text("Open App info") }
                }
                HorizontalDivider(Modifier.padding(top = 16.dp))
            }
            // Without seeing its calls the phone has nothing to show, so this cannot be on.
            Row(
                Modifier
                    .fillMaxWidth()
                    .toggleable(value = on && state, enabled = state, role = Role.Switch, onValueChange = onOn)
                    .padding(vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Calls on your computer", style = MaterialTheme.typography.titleSmall)
                    Text(
                        missing(history, answer, dial)
                            ?: "See calls on the computer, answer and end them there, and call from it while this phone is unlocked.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(checked = on && state, onCheckedChange = null, enabled = state)
            }
            HorizontalDivider()
            Row(Modifier.fillMaxWidth().padding(vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Names from your contacts", style = MaterialTheme.typography.titleSmall)
                    Text(
                        if (names) {
                            "The computer shows a contact's name instead of the number."
                        } else {
                            "Optional. Without it the computer shows numbers."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (!names) {
                    TextButton(onClick = { askNames.launch(Manifest.permission.READ_CONTACTS) }) { Text("Allow") }
                }
            }
        }
    }
}

/** What the computer cannot do with the grants as they are, or null when it can do all of it. */
private fun missing(history: Boolean, answer: Boolean, dial: Boolean): String? {
    val off = buildList {
        if (!answer) add("answering")
        if (!history) add("recent calls")
        if (!dial) add("calling")
    }
    return if (off.isEmpty()) null else "Not allowed: ${off.joinToString(", ")}."
}
