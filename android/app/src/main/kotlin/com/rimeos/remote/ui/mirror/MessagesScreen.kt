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
import com.rimeos.remote.phone.Sms

/**
 * "Messages on your computer" (contract §2.11): the SMS permissions, asked
 * here and only here, and the switch, off until the person turns it on. A
 * phone that installed the app from a file gets no prompt at all on Android
 * 13 and later until restricted settings are allowed, so a refusal that came
 * with no prompt says where that is.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MessagesScreen(
    on: Boolean,
    onOn: (Boolean) -> Unit,
    /** The permissions may have changed: the machines are told the new `can`. */
    onPermissions: () -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    var read by remember { mutableStateOf(Sms.canRead(context)) }
    var send by remember { mutableStateOf(Sms.canSend(context)) }
    var names by remember { mutableStateOf(Sms.canNames(context)) }
    var blocked by remember { mutableStateOf(false) }
    fun reread() {
        read = Sms.canRead(context)
        send = Sms.canSend(context)
        names = Sms.canNames(context)
        onPermissions()
    }
    // Back from App info: read the grants again.
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { reread() }
    val askSms = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        reread()
        blocked = result.values.any { !it }
    }
    val askNames = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { reread() }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("Messages on your computer", style = MaterialTheme.typography.titleMedium) },
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
                "Your paired computers can list this phone's SMS conversations, read them and send an " +
                    "SMS from this phone's number. Each list is read from the phone when the computer " +
                    "asks, and nothing is kept on the computer. Reading works while this app is unlocked " +
                    "or keeps working after a restart; sending only while it is unlocked. Group " +
                    "conversations and picture messages are not shown or sent.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Spacer(Modifier.height(16.dp))
            if (!read || !send) {
                Text("Allow SMS", style = MaterialTheme.typography.titleSmall)
                Text(
                    "Android asks whether this app may read and send SMS.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedButton(
                    onClick = { askSms.launch(arrayOf(Manifest.permission.READ_SMS, Manifest.permission.SEND_SMS)) },
                    modifier = Modifier.padding(top = 8.dp),
                ) { Text("Allow SMS") }
                if (blocked) {
                    Text(
                        "Android did not allow it. If you installed this app from a file, open App info, " +
                            "tap the menu at the top right, choose Allow restricted settings, then allow SMS " +
                            "under Permissions.",
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
            // The store is readable only with the permission, so without it this cannot be on.
            Row(
                Modifier
                    .fillMaxWidth()
                    .toggleable(value = on && read, enabled = read, role = Role.Switch, onValueChange = onOn)
                    .padding(vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Messages on your computer", style = MaterialTheme.typography.titleSmall)
                    Text(
                        if (read && !send) {
                            "Reading only: sending SMS is not allowed."
                        } else {
                            "Read your conversations on the computer, and reply from it while this phone is unlocked."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(checked = on && read, onCheckedChange = null, enabled = read)
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
