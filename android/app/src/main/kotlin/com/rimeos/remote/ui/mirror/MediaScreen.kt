package com.rimeos.remote.ui.mirror

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
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
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
import com.rimeos.remote.phone.MediaWatcher
import com.rimeos.remote.phone.MirrorService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

/**
 * "Media on your computer" (contract §2.8): the switch, and the apps whose
 * playback may show on the paired computers. The apps listed are the ones
 * with a media session now, plus those already turned on; none is on until
 * the person turns it on here.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MediaScreen(
    on: Boolean,
    onOn: (Boolean) -> Unit,
    chosen: Set<String>,
    onChoose: (pkg: String, on: Boolean) -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    var granted by remember { mutableStateOf(MirrorService.granted(context)) }
    var playing by remember { mutableStateOf<List<String>?>(null) }
    var apps by remember { mutableStateOf<List<ChoosableApp>>(emptyList()) }
    // Back from Settings: read the grant again.
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { granted = MirrorService.granted(context) }
    // While this is on screen, an app that starts playing appears.
    LaunchedEffect(granted) {
        while (true) {
            playing = if (granted) withContext(Dispatchers.IO) { MediaWatcher.sessionPackages(context) } else emptyList()
            delay(2_000)
        }
    }
    LaunchedEffect(playing, chosen) {
        val pkgs = (playing.orEmpty() + chosen.sorted()).distinct()
        apps = withContext(Dispatchers.IO) {
            pkgs.map { ChoosableApp(it, MediaWatcher.labelOf(context, it)) }.sortedBy { it.label.lowercase() }
        }
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("Media on your computer", style = MaterialTheme.typography.titleMedium) },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item {
                Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
                    Text(
                        "What the apps you turn on below are playing shows in your paired computers' " +
                            "player while this app is unlocked and connected. The computer gets the app's " +
                            "name, the title, artist and album, the length, how far in it is and whether it " +
                            "is playing. No artwork, links or queue. It only shows: play, pause and skip on " +
                            "the computer never act on this phone.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Spacer(Modifier.height(16.dp))
                    NotificationAccess(granted)
                    HorizontalDivider(Modifier.padding(top = 16.dp))
                    // Media sessions are readable only with notification access, so without it this cannot be on.
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .toggleable(value = on && granted, enabled = granted, role = Role.Switch, onValueChange = onOn)
                            .padding(vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text("Media on your computer", style = MaterialTheme.typography.titleSmall)
                            Text(
                                "Shows what is playing on this phone in the computer's player.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Switch(checked = on && granted, onCheckedChange = null, enabled = granted)
                    }
                    HorizontalDivider()
                    Text(
                        "Apps",
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.padding(top = 12.dp),
                    )
                }
            }
            when {
                playing == null -> item { Text("Looking for apps that play media…", Modifier.padding(20.dp)) }
                apps.isEmpty() -> item {
                    Text(
                        if (granted) {
                            "No app is playing anything. Start playing in an app and it appears here."
                        } else {
                            "Allow notification access to see the apps that play media."
                        },
                        Modifier.padding(20.dp),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                else -> items(apps, key = { it.pkg }) { app ->
                    val allowed = app.pkg in chosen
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .toggleable(value = allowed, role = Role.Switch, onValueChange = { onChoose(app.pkg, it) })
                            .padding(horizontal = 20.dp, vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(app.label, Modifier.weight(1f))
                        // The row is the control; the switch only shows its state.
                        Switch(checked = allowed, onCheckedChange = null)
                    }
                }
            }
        }
    }
}
