package com.rimeos.remote.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.ui.theme.MachineText
import java.text.DateFormat
import java.util.Date
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.text.font.FontFamily
import com.rimeos.remote.core.CrashReport
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.PowerManager
import androidx.compose.ui.platform.LocalContext

/**
 * The list of computers this phone is paired with, which is the app's home.
 *
 * P1-053 asks for "multiple Rime computers", so a list is the front page rather
 * than a detail somewhere — an app whose home screen is one machine teaches
 * people it holds one machine. Each row shows the machine's name, the device id
 * the *desktop* displays for this phone, and when the pairing was made; those
 * three together are what lets somebody match a row here against a row in
 * `rime remote devices` on the other end, which is the only way to be sure the
 * thing being revoked is the thing they meant.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MachinesScreen(
    state: UiState,
    onPair: () -> Unit,
    onConnect: (PairedMachine) -> Unit,
    onPing: (PairedMachine) -> Unit,
    onForget: (PairedMachine) -> Unit,
    onCrashReports: (Boolean) -> Unit,
    onShareCrash: (String) -> Unit,
    onClearCrash: () -> Unit,
    onDynamicColour: (Boolean) -> Unit,
    onLock: () -> Unit,
    onDismiss: () -> Unit,
    /**
     * The app's own update row, passed in rather than built here.
     *
     * A slot, because this screen is about the machines a phone is paired with
     * and knows nothing about GitHub, `PackageInstaller` or versionCodes —
     * and because a default of "draw nothing" keeps every existing caller,
     * including the on-device Compose tests, compiling and behaving exactly as
     * before.
     */
    updateBanner: @Composable () -> Unit = {},
    /** "Notifications on your computer". A default, as for [updateBanner], so existing callers keep compiling. */
    onMirror: () -> Unit = {},
    onMedia: () -> Unit = {},
    /** "Messages on your computer" (contract §2.11). */
    onMessages: () -> Unit = {},
    /** "Calls on your computer" (contract §2.12). */
    onCalls: () -> Unit = {},
    /** "Keep working after the phone restarts" (contract §2.10). */
    onCompanion: (Boolean) -> Unit = {},
) {
    var forgetting by remember { mutableStateOf<PairedMachine?>(null) }
    var settingsOpen by remember { mutableStateOf(false) }
    var askBattery by remember { mutableStateOf(false) }
    val context = LocalContext.current

    if (askBattery) {
        BatteryAsk(
            onAllow = {
                askBattery = false
                runCatching {
                    context.startActivity(
                        Intent(
                            android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                            Uri.parse("package:${context.packageName}"),
                        ),
                    )
                }
            },
            onNotNow = { askBattery = false },
        )
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("Rime Remote", style = MaterialTheme.typography.titleMedium) },
                actions = {
                    TextButton(onClick = { settingsOpen = true }) { Text("Display") }
                    TextButton(onClick = onLock) { Text("Lock") }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.background,
                ),
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .background(MaterialTheme.colorScheme.background)
                .padding(padding),
        ) {
            // First, above every other notice. It is the only one that is
            // about the app rather than about a machine, and the reason a
            // person is seeing an odd message from a machine may well be that
            // this app is the half that is out of date.
            updateBanner()
            state.gateWarning?.let { Notice(it, alarming = true) }
            state.busy?.let { Notice(it, alarming = false) }
            state.failure?.let { Notice(it, alarming = true, onDismiss = onDismiss) }
            state.message?.let { Notice(it, alarming = false, onDismiss = onDismiss) }
            state.connection?.let {
                // The path and its disclosure, not just the round trip.
                // `docs/remote.md`: "A relay path prints its disclosure — what
                // the relay can and cannot see — rather than leaving the reader
                // to assume." A person who has walked out of the house is owed
                // the fact that a third party is now on the path, at the moment
                // it becomes true and not in a settings screen.
                Notice(
                    "Connected to ${it.machine}" +
                        (it.roundTripMs?.let { ms -> " — round trip ${ms}ms" } ?: "") +
                        "\n${it.path.disclosure().replaceFirstChar { c -> c.uppercase() }}.",
                    alarming = false,
                    onDismiss = onDismiss,
                )
            }

            if (state.machines.isEmpty() && !state.loading) {
                Empty(onPair)
            } else {
                LazyColumn(Modifier.weight(1f)) {
                    items(state.machines, key = { it.deviceId }) { machine ->
                        MachineRow(
                            machine = machine,
                            link = state.links[machine.deviceId],
                            onClick = { onConnect(machine) },
                            onPing = { onPing(machine) },
                            onForget = { forgetting = machine },
                        )
                    }
                }
                Button(
                    onClick = onPair,
                    shape = RoundedCornerShape(6.dp),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.primary,
                        contentColor = MaterialTheme.colorScheme.onPrimary,
                    ),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(20.dp),
                ) { Text("Add a computer") }
            }
        }
    }

    forgetting?.let { machine ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Forget ${machine.machine}?") },
            text = {
                // Both halves, because only one of them is this phone's to do.
                Text(
                    "This phone will delete the key it uses with ${machine.machine}, and the " +
                        "stored pairing becomes unreadable. It does not tell ${machine.machine} " +
                        "anything — to revoke this phone there, run `rime remote revoke " +
                        "${machine.deviceId}` on the machine itself.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    onForget(machine)
                    forgetting = null
                }) { Text("Forget", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { forgetting = null }) { Text("Keep") }
            },
        )
    }

    if (settingsOpen) {
        AlertDialog(
            onDismissRequest = { settingsOpen = false },
            title = { Text("Display") },
            text = {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("Use this phone's colours")
                            Text(
                                "Material You instead of Rime's own palette.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Switch(
                            checked = state.settings.dynamicColour,
                            onCheckedChange = onDynamicColour,
                        )
                    }
                    Spacer(Modifier.height(12.dp))
                    Text(
                        "Light and dark follow the system setting.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )

                    HorizontalDivider(Modifier.padding(vertical = 14.dp))

                    TextButton(onClick = {
                        settingsOpen = false
                        onMirror()
                    }) { Text("Notifications on your computer") }
                    TextButton(onClick = {
                        settingsOpen = false
                        onMedia()
                    }) { Text("Media on your computer") }
                    TextButton(onClick = {
                        settingsOpen = false
                        onMessages()
                    }) { Text("Messages on your computer") }
                    TextButton(onClick = {
                        settingsOpen = false
                        onCalls()
                    }) { Text("Calls on your computer") }

                    Spacer(Modifier.height(6.dp))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("Keep working after the phone restarts")
                            Text(
                                COMPANION_EXPLANATION,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Switch(
                            checked = state.settings.companionOn,
                            onCheckedChange = { on ->
                                onCompanion(on)
                                // The exemption is asked for once, when the
                                // switch goes on, and only if not already held.
                                if (on && !ignoresBatteryOptimisation(context)) askBattery = true
                            },
                        )
                    }

                    HorizontalDivider(Modifier.padding(vertical = 14.dp))

                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("Keep a report when this app crashes")
                            Text(
                                // From :core, where a test asserts it says what
                                // is kept and what is dropped. Consent to
                                // "crash reporting" with no statement of
                                // contents is not consent to anything.
                                CrashReport.CONSENT_EXPLANATION,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Switch(
                            checked = state.settings.crashReports,
                            onCheckedChange = onCrashReports,
                        )
                    }

                    state.crash?.let { report ->
                        Spacer(Modifier.height(10.dp))
                        Text(
                            "A report from the last crash is on this phone.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        Box(
                            Modifier
                                .padding(top = 6.dp)
                                .heightIn(max = 200.dp)
                                .verticalScroll(rememberScrollState()),
                        ) {
                            // Shown in full and never summarised. A report the
                            // user cannot read before sending is a report they
                            // are agreeing to blind.
                            Text(
                                report,
                                style = MaterialTheme.typography.bodySmall,
                                fontFamily = FontFamily.Monospace,
                            )
                        }
                        Row(Modifier.padding(top = 6.dp)) {
                            TextButton(onClick = { onShareCrash(report) }) { Text("Share") }
                            TextButton(onClick = onClearCrash) { Text("Delete") }
                        }
                    }
                }
            },
            confirmButton = { TextButton(onClick = { settingsOpen = false }) { Text("Done") } },
        )
    }
}

/** What the companion switch does, beside it. */
const val COMPANION_EXPLANATION: String =
    "While the app is locked, and after a restart, your computers keep getting this phone's " +
        "battery, notifications and what is playing, and can still ring it and send it files. " +
        "Agents, terminals and computer controls wait until you unlock."

private fun ignoresBatteryOptimisation(context: Context): Boolean =
    runCatching {
        context.getSystemService(PowerManager::class.java)?.isIgnoringBatteryOptimizations(context.packageName) == true
    }.getOrDefault(false)

/**
 * Asks to leave the app out of battery optimisation, and says why. Optional:
 * "Not now" keeps the switch on, and the link still works, later.
 */
@Composable
private fun BatteryAsk(onAllow: () -> Unit, onNotNow: () -> Unit) {
    AlertDialog(
        onDismissRequest = onNotNow,
        title = { Text("Let Rime Remote run in the background?") },
        text = {
            Text(
                "To save battery, Android can hold this app's connection off for long stretches " +
                    "while the phone is idle, and your computers would then hear nothing until it wakes. " +
                    "Leaving Rime Remote out of battery optimisation keeps notifications and status " +
                    "arriving promptly. You can say no: the link still works, just later.",
            )
        },
        confirmButton = { TextButton(onClick = onAllow) { Text("Allow") } },
        dismissButton = { TextButton(onClick = onNotNow) { Text("Not now") } },
    )
}

@Composable
private fun MachineRow(
    machine: PairedMachine,
    /** The held connection's state, when this machine has one. */
    link: LinkHub.LinkState?,
    onClick: () -> Unit,
    onPing: () -> Unit,
    onForget: () -> Unit,
) {
    Column {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable(onClick = onClick)
                .padding(horizontal = 20.dp, vertical = 14.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                Modifier
                    .size(10.dp)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.primary),
            )
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    machine.machine,
                    style = MaterialTheme.typography.bodyLarge,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                // Monospace, because this is the string somebody compares
                // character by character against `rime remote devices`.
                Text(
                    machine.deviceId,
                    style = MachineText,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    "paired ${DateFormat.getDateInstance().format(Date(machine.pairedMs))}" +
                        if (machine.relay != null) " · relay available" else " · LAN only",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                // Now that the phone connects on unlock without being asked,
                // the path is said here, on the row, and a relay path carries
                // its disclosure (`docs/remote.md`) at the moment it is true.
                linkLine(link)?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.labelSmall,
                        color = if (link?.status == LinkHub.Status.CONNECTED) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
            }
            // "Does this still answer" is a different question from "show me
            // the agents", and it is the one somebody asks after changing a
            // network. It costs one handshake and one frame.
            TextButton(onClick = onPing) {
                Text("Test", color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            TextButton(onClick = onForget) {
                Text("Forget", color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        HorizontalDivider(
            Modifier.padding(start = 44.dp),
            thickness = 1.dp,
            color = MaterialTheme.colorScheme.outline.copy(alpha = 0.4f),
        )
    }
}

/** One line about a held connection, or null when there is none. */
private fun linkLine(link: LinkHub.LinkState?): String? = when (link?.status) {
    null, LinkHub.Status.IDLE -> null
    LinkHub.Status.CONNECTING -> "connecting…"
    LinkHub.Status.RECONNECTING -> "reconnecting" + (link.failure?.let { " — $it" } ?: "…")
    LinkHub.Status.CONNECTED -> buildString {
        append("connected")
        when (link.via) {
            "lan" -> append(" on this network")
            "relay" -> append(" through the relay, which sees when you connect but not what you send")
        }
        link.connectMs?.let { append(" · ${it} ms") }
    }
}

@Composable
private fun Empty(onPair: () -> Unit) {
    Column(
        Modifier
            .fillMaxSize()
            .padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("No computers yet", style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(10.dp))
        Text(
            "On the machine you want to reach, run `rime remote pair`. It prints a QR code " +
                "that stands for three minutes.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        OutlinedButton(onClick = onPair, shape = RoundedCornerShape(6.dp)) {
            Text("Scan the code")
        }
    }
}

/** A line of what just happened, in the colour that says how to feel about it. */
@Composable
fun Notice(text: String, alarming: Boolean, onDismiss: (() -> Unit)? = null) {
    Row(
        Modifier
            .fillMaxWidth()
            .background(
                if (alarming) {
                    MaterialTheme.colorScheme.error.copy(alpha = 0.12f)
                } else {
                    MaterialTheme.colorScheme.primary.copy(alpha = 0.12f)
                },
            )
            .padding(horizontal = 20.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text,
            style = MaterialTheme.typography.bodySmall,
            color = if (alarming) {
                MaterialTheme.colorScheme.error
            } else {
                MaterialTheme.colorScheme.onBackground
            },
            modifier = Modifier.weight(1f),
        )
        if (onDismiss != null) {
            TextButton(onClick = onDismiss) { Text("OK") }
        }
    }
}
