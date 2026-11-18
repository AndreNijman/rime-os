package com.rimeos.remote.ui.mirror

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Button
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
import androidx.compose.runtime.LaunchedEffect
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
import com.rimeos.remote.phone.MirrorService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** One app a person can choose. */
data class ChoosableApp(val pkg: String, val label: String)

/** The apps with a launcher icon, by name, without this one. */
fun launcherApps(context: Context): List<ChoosableApp> {
    val pm = context.packageManager
    val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
    val found = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        pm.queryIntentActivities(launcher, PackageManager.ResolveInfoFlags.of(0))
    } else {
        @Suppress("DEPRECATION")
        pm.queryIntentActivities(launcher, 0)
    }
    return found
        .map { it.activityInfo.packageName to it.loadLabel(pm).toString() }
        .filter { (pkg, _) -> pkg != context.packageName }
        .distinctBy { it.first }
        .map { (pkg, label) -> ChoosableApp(pkg, label) }
        .sortedBy { it.label.lowercase() }
}

/**
 * "Notifications on your computer" (contract §2.7): notification access, and
 * the apps whose notifications are shown on the paired computers. None are on
 * until the person turns them on here.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MirrorScreen(
    chosen: Set<String>,
    onChoose: (pkg: String, on: Boolean) -> Unit,
    reply: Boolean,
    onReply: (Boolean) -> Unit,
    focus: Boolean,
    onFocus: (Boolean) -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    var granted by remember { mutableStateOf(MirrorService.granted(context)) }
    var apps by remember { mutableStateOf<List<ChoosableApp>?>(null) }
    // Back from Settings: read the grant again.
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { granted = MirrorService.granted(context) }
    LaunchedEffect(Unit) { apps = withContext(Dispatchers.IO) { launcherApps(context) } }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("Notifications on your computer", style = MaterialTheme.typography.titleMedium) },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item {
                Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
                    Text(
                        "Notifications from the apps you turn on below appear on your paired computers " +
                            "while this app is unlocked and connected. The computer gets the app's name, " +
                            "the title and the text (and, with replying on, the name of the app's reply " +
                            "button), nothing else, and keeps nothing after you dismiss it. " +
                            "Removing one here removes it there.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Spacer(Modifier.height(16.dp))
                    NotificationAccess(granted)
                    HorizontalDivider(Modifier.padding(top = 16.dp))
                    // Without notification access there is nothing to answer, so it cannot be turned on.
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .toggleable(value = reply && granted, enabled = granted, role = Role.Switch, onValueChange = onReply)
                            .padding(vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text("Reply from your computer", style = MaterialTheme.typography.titleSmall)
                            Text(
                                "Replies you type on the computer are sent from this phone by the app that " +
                                    "posted the notification, as you.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Switch(checked = reply && granted, onCheckedChange = null, enabled = granted)
                    }
                    HorizontalDivider()
                    // The same grant is what lets this app change Do Not Disturb (contract §2.9).
                    SwitchRow(
                        title = "Do Not Disturb follows your computer",
                        text = "Turning Do Not Disturb on or off on a paired computer does the same here, " +
                            "and the other way round, when that computer follows this phone too. " +
                            "A computer can turn off only a Do Not Disturb it turned on.",
                        on = focus && granted,
                        enabled = granted,
                        onChange = onFocus,
                    )
                    HorizontalDivider()
                    Text(
                        "Apps",
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.padding(top = 12.dp),
                    )
                }
            }
            val list = apps
            if (list == null) {
                item { Text("Reading the apps on this phone…", Modifier.padding(20.dp)) }
            } else {
                items(list, key = { it.pkg }) { app ->
                    val on = app.pkg in chosen
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .toggleable(value = on, role = Role.Switch, onValueChange = { onChoose(app.pkg, it) })
                            .padding(horizontal = 20.dp, vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(app.label, Modifier.weight(1f))
                        // The row is the control; the switch only shows its state.
                        Switch(checked = on, onCheckedChange = null)
                    }
                }
            }
        }
    }
}

/** A row whose whole width is the switch, with a line saying what it does. */
@Composable
private fun SwitchRow(title: String, text: String, on: Boolean, enabled: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .toggleable(value = on, enabled = enabled, role = Role.Switch, onValueChange = onChange)
            .padding(vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Switch(checked = on, onCheckedChange = null, enabled = enabled)
    }
}

/**
 * Whether this app holds notification access, and the way to grant it: the
 * one grant behind both "Notifications on your computer" and "Media on your
 * computer" (contract §2.7, §2.8).
 */
@Composable
fun NotificationAccess(granted: Boolean) {
    val context = LocalContext.current
    Column {
        if (granted) {
            Text("Notification access is on.", style = MaterialTheme.typography.titleSmall)
            TextButton(onClick = { context.startActivity(MirrorService.settingsIntent(context)) }) {
                Text("Turn it off in Settings")
            }
        } else {
            Text("Notification access is off.", style = MaterialTheme.typography.titleSmall)
            Text(
                "Android asks you to allow it in Settings. Nothing is sent until you do, " +
                    "and then only from the apps you turn on here.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))
            Button(onClick = { context.startActivity(MirrorService.settingsIntent(context)) }) {
                Text("Allow notification access")
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                Spacer(Modifier.height(10.dp))
                Text(
                    "If Android says the setting is restricted: open App info, tap the menu " +
                        "in the corner, choose \"Allow restricted settings\", then come back " +
                        "and try again. Android does this for apps installed from a file.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedButton(onClick = { context.startActivity(MirrorService.appInfoIntent(context)) }) {
                    Text("Open App info")
                }
            }
        }
    }
}
