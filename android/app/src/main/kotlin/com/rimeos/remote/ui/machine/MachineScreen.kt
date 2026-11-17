package com.rimeos.remote.ui.machine

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
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
import android.content.ClipboardManager
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.rimeos.remote.core.machine.MachineStatus
import com.rimeos.remote.core.machine.MediaAction
import com.rimeos.remote.core.machine.MediaState
import com.rimeos.remote.ui.agent.Strip
import kotlin.math.roundToInt

/**
 * The computer itself: how it is, locking it, and what it is playing.
 *
 * Every section is drawn only when the computer offers it (its hello), so an
 * older Rime shows what it can do and nothing that would answer "unknown".
 * Lines the computer could not read are left out, not shown as zero or false.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MachineScreen(
    state: MachineUiState,
    onLock: () -> Unit,
    onMedia: (MediaAction) -> Unit,
    onVolume: (Int) -> Unit,
    onMute: (Boolean) -> Unit,
    onRefresh: () -> Unit,
    onDismiss: () -> Unit,
    onBack: () -> Unit,
    /** Called with what this phone has copied, read at the tap. */
    onSendClipboard: (String?) -> Unit = {},
) {
    val context = LocalContext.current
    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text(state.machine ?: "This computer", style = MaterialTheme.typography.titleMedium, maxLines = 1) },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                actions = { TextButton(onClick = onRefresh) { Text("Refresh") } },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState())) {
            state.away?.let { Strip(it, alarming = true) }
            state.failure?.let { Strip(it, alarming = true, onDismiss = onDismiss) }
            state.notice?.let { Strip(it, alarming = false, onDismiss = onDismiss) }

            if (!state.offersStatus && !state.offersLock && !state.offersMedia) {
                Line("This computer's Rime is too old to be checked or controlled from the phone. Update it to use this screen.")
                return@Column
            }
            if (!state.loaded) {
                Line("Asking ${state.machine ?: "the computer"}…")
            }

            if (state.offersStatus || state.offersLock) {
                Heading("Status")
                state.status?.let { StatusLines(it) }
                if (state.offersLock) {
                    val locked = state.status?.locked == true
                    OutlinedButton(
                        onClick = onLock,
                        enabled = state.acting == null && !locked,
                        shape = RoundedCornerShape(6.dp),
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                    ) {
                        Text(
                            when {
                                state.acting == MachineUiState.LOCK -> "Locking…"
                                locked -> "The screen is locked"
                                else -> "Lock the screen"
                            },
                        )
                    }
                    Line("Unlocking is only ever done at the computer.", quiet = true)
                }
            }

            if (state.offersClipboard) {
                HorizontalDivider(Modifier.padding(vertical = 8.dp))
                Heading("Clipboard")
                OutlinedButton(
                    onClick = {
                        val clip = context.getSystemService(ClipboardManager::class.java)?.primaryClip
                        val text = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(context)?.toString()
                        onSendClipboard(text)
                    },
                    enabled = state.acting == null,
                    shape = RoundedCornerShape(6.dp),
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                ) {
                    Text(if (state.acting == MachineUiState.CLIPBOARD) "Sending…" else "Send this phone's clipboard")
                }
                Line("It replaces what the computer has copied. To send a file, share it to Rime Remote.", quiet = true)
            }

            if (state.offersMedia && state.loaded) {
                HorizontalDivider(Modifier.padding(vertical = 8.dp))
                Heading("Playing")
                Media(state.media, state.acting, onMedia, onVolume, onMute)
            }
        }
    }
}

@Composable
private fun StatusLines(s: MachineStatus) {
    val lines = listOfNotNull(
        s.power?.let { "Battery" to it },
        s.mode?.let { "Power mode" to it },
        s.locked?.let { "Screen" to if (it) "Locked" else "Unlocked" },
        s.updateLine?.let { "Updates" to it },
    )
    if (lines.isEmpty()) {
        Line("The computer did not say how it is.")
        return
    }
    for ((label, value) in lines) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp).semantics(mergeDescendants = true) {},
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(value, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
private fun Media(
    media: MediaState?,
    acting: String?,
    onMedia: (MediaAction) -> Unit,
    onVolume: (Int) -> Unit,
    onMute: (Boolean) -> Unit,
) {
    val player = media?.player
    if (player == null) {
        Line("Nothing is playing.")
    } else {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
            Text(
                player.title.ifEmpty { "Untitled" },
                style = MaterialTheme.typography.titleSmall,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            val by = listOf(player.artist, player.name).filter { it.isNotEmpty() }.joinToString(" · ")
            if (by.isNotEmpty()) {
                Text(by, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        // Words, not icons: a flow row keeps all three on screen at any text size.
        FlowRow(Modifier.fillMaxWidth().padding(horizontal = 8.dp), horizontalArrangement = Arrangement.Center) {
            val free = acting == null
            TextButton(onClick = { onMedia(MediaAction.PREVIOUS) }, enabled = free) { Text("Previous") }
            TextButton(onClick = { onMedia(MediaAction.PLAY_PAUSE) }, enabled = free) {
                Text(if (player.playing) "Pause" else "Play")
            }
            TextButton(onClick = { onMedia(MediaAction.NEXT) }, enabled = free) { Text("Next") }
        }
    }

    val volume = media?.volume ?: return
    // What the slider shows while a finger is on it; null when it shows the computer's own.
    var dragging by remember { mutableStateOf<Float?>(null) }
    val shown = dragging ?: volume.percent.toFloat()
    Row(
        Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            if (volume.muted) "Muted" else "Volume ${shown.roundToInt()} %",
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = { onMute(!volume.muted) }, enabled = acting == null) {
            Text(if (volume.muted) "Unmute" else "Mute")
        }
    }
    Slider(
        value = shown,
        onValueChange = { dragging = it },
        onValueChangeFinished = {
            dragging?.let { onVolume(it.roundToInt()) }
            dragging = null
        },
        valueRange = 0f..100f,
        enabled = acting == null,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp).semantics {
            contentDescription = "Volume"
            stateDescription = if (volume.muted) "Muted, ${volume.percent} %" else "${shown.roundToInt()} %"
        },
    )
}

@Composable
private fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun Line(text: String, quiet: Boolean = false) {
    Text(
        text,
        style = if (quiet) MaterialTheme.typography.bodySmall else MaterialTheme.typography.bodyMedium,
        color = if (quiet) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
    )
}
