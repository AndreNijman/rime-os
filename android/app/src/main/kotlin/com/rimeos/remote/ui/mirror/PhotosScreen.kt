package com.rimeos.remote.ui.mirror

import android.content.Intent
import android.net.Uri
import android.os.Build
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
import com.rimeos.remote.core.phone.Photos
import com.rimeos.remote.phone.PhotoStore

/**
 * "Photos on your computer" (contract §2.13): photo access, asked here and
 * only here, and the switch, off until the person turns it on. On Android 14
 * and later the person may allow only some photos; the screen says so
 * plainly, because the computer then shows those and nothing else, and
 * asking again is how they pick more or allow all.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PhotosScreen(
    on: Boolean,
    onOn: (Boolean) -> Unit,
    /** The access may have changed: the machines are told the new `can`. */
    onPermissions: () -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    var access by remember { mutableStateOf(PhotoStore.access(context)) }
    var refused by remember { mutableStateOf(false) }
    fun reread() {
        access = PhotoStore.access(context)
        onPermissions()
    }
    // Back from App info or the photo picker: read the access again.
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { reread() }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
        reread()
        refused = access == Photos.Access.NONE
    }
    val readable = access != Photos.Access.NONE

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("Photos on your computer", style = MaterialTheme.typography.titleMedium) },
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
                "Your paired computers can list this phone's photos, newest first, with a small preview " +
                    "of each, and save the ones you choose. The list is read from the phone when the " +
                    "computer asks, and only while this app is unlocked. The previews carry no location " +
                    "or camera details. " +
                    // Android 9 hands an app the file as it is; from 10 on, without the media location
                    // permission, which this app never asks for, it blanks the location first.
                    (if (Build.VERSION.SDK_INT >= 29) "A saved photo has its location removed by Android. " else "") +
                    "Videos are not shown.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Spacer(Modifier.height(16.dp))
            when (access) {
                Photos.Access.NONE -> {
                    Text("Allow photos", style = MaterialTheme.typography.titleSmall)
                    Text(
                        "Android asks whether this app may see your photos. You can allow all of them " +
                            "or pick some.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OutlinedButton(
                        onClick = { ask.launch(PhotoStore.permissions()) },
                        modifier = Modifier.padding(top = 8.dp),
                    ) { Text("Allow photos") }
                    if (refused) {
                        Text(
                            "Android did not allow it. If it no longer asks, open App info and allow " +
                                "photos under Permissions.",
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
                Photos.Access.PARTIAL -> {
                    Text("Only the photos you picked", style = MaterialTheme.typography.titleSmall)
                    Text(
                        "Only the photos you picked are shown on your computer. Photos you take later " +
                            "are not added until you pick them too.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OutlinedButton(
                        onClick = { ask.launch(PhotoStore.permissions()) },
                        modifier = Modifier.padding(top = 8.dp),
                    ) { Text("Pick more or allow all") }
                    HorizontalDivider(Modifier.padding(top = 16.dp))
                }
                Photos.Access.FULL -> Unit
            }
            // The store is readable only with access, so without it this cannot be on.
            Row(
                Modifier
                    .fillMaxWidth()
                    .toggleable(value = on && readable, enabled = readable, role = Role.Switch, onValueChange = onOn)
                    .padding(vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Photos on your computer", style = MaterialTheme.typography.titleSmall)
                    Text(
                        "Browse and save this phone's photos on the computer while this app is unlocked.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(checked = on && readable, onCheckedChange = null, enabled = readable)
            }
        }
    }
}
