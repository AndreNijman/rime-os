package com.apexos.remote.ui.terminal

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.rememberScrollableState
import androidx.compose.foundation.gestures.scrollable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.nativeKeyEvent
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.apexos.remote.core.term.AccessoryAction
import com.apexos.remote.core.term.AccessoryKey
import com.apexos.remote.core.term.AccessoryKeys
import com.apexos.remote.core.term.Key
import com.apexos.remote.core.term.Keys
import com.apexos.remote.core.term.Match
import com.apexos.remote.core.term.Mods
import com.apexos.remote.core.term.Pos

/**
 * A terminal, with the things a phone needs to be one.
 *
 * ## The input problem, and what is done about it
 *
 * Android's soft keyboard does not deliver key events. It edits a text buffer
 * and tells the app what the buffer now says, which is fine for a chat box and
 * useless for a terminal: there is no "the user pressed backspace", only "the
 * text is now one character shorter".
 *
 * So the field below holds a run of sentinel characters that the user cannot
 * see, and every change is read as a **diff** against it: longer means text
 * was typed, shorter means that many backspaces. The sentinel run is restored
 * afterwards, so there is always something left to delete — a field that was
 * empty would swallow the first backspace of every burst, which is the bug
 * every terminal app on this platform has had at some point.
 *
 * Hardware keys take the other path: [Keymap] turns a key code into a [Key]
 * and `onPreviewKeyEvent` sends it before the field sees it.
 *
 * **None of this is verified on a device.** There is no phone and the
 * project's rule forbids an emulator. [Keymap] is tested on the JVM because it
 * is a table; the IME behaviour above is the standard approach and is where
 * the first real defects will be.
 */
@Composable
@Suppress("LongMethod", "CyclomaticComplexMethod")
fun TerminalScreen(
    state: TerminalUiState,
    accessoryRow: List<AccessoryKey>,
    fontSizeSp: Int,
    onInput: (ByteArray) -> Unit,
    onResize: (cols: Int, rows: Int) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val clipboard = LocalClipboardManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    val focus = remember { FocusRequester() }
    val palette = rememberTerminalPalette()

    var armed by remember { mutableStateOf(Mods.NONE) }
    var scrollBack by remember { mutableIntStateOf(0) }
    var selection by remember { mutableStateOf<Pair<Pos, Pos>?>(null) }
    var searching by remember { mutableStateOf(false) }
    var needle by remember { mutableStateOf("") }
    var matches by remember { mutableStateOf(emptyList<Match>()) }
    var matchAt by remember { mutableIntStateOf(0) }
    var metrics by remember { mutableStateOf(CellMetrics(1f, 1f, 1f)) }
    var firstVisible by remember { mutableIntStateOf(0) }

    val terminal = state.terminal

    fun send(bytes: ByteArray) {
        if (bytes.isEmpty()) return
        onInput(bytes)
        // Typing returns the viewport to the bottom, because the thing the
        // person just did is happening there. A terminal that stayed scrolled
        // up while they typed would look frozen.
        scrollBack = 0
        armed = Mods.NONE
    }

    fun runSearch(text: String) {
        needle = text
        matches = if (text.isEmpty()) emptyList() else terminal.read { it.search(text) }
        matchAt = matches.lastIndex.coerceAtLeast(0)
        val hit = matches.getOrNull(matchAt) ?: return
        scrollBack = terminal.read { (it.totalLines - 1 - hit.line).coerceAtLeast(0) }
    }

    Column(
        modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.surface)
            .imePadding(),
    ) {
        TerminalBar(
            title = state.title,
            subtitle = state.status,
            searching = searching,
            needle = needle,
            matchCount = matches.size,
            onNeedle = { runSearch(it) },
            onSearchToggle = {
                searching = !searching
                if (!searching) {
                    needle = ""
                    matches = emptyList()
                    scrollBack = 0
                }
            },
            onNextMatch = {
                if (matches.isNotEmpty()) {
                    matchAt = (matchAt + 1) % matches.size
                    val hit = matches[matchAt]
                    scrollBack = terminal.read { (it.totalLines - 1 - hit.line).coerceAtLeast(0) }
                }
            },
            onCopy = {
                val sel = selection
                val text = if (sel != null) {
                    terminal.read { it.textBetween(sel.first, sel.second) }
                } else {
                    terminal.read { it.transcript() }
                }
                clipboard.setText(AnnotatedString(text))
                selection = null
            },
            onPaste = {
                val text = clipboard.getText()?.text ?: return@TerminalBar
                send(Keys.paste(text, terminal.bracketedPaste))
            },
            onBack = onBack,
        )

        BoxWithConstraints(
            Modifier
                .weight(1f)
                .fillMaxWidth(),
        ) {
            val grid = remember { mutableStateOf(80 to 24) }
            TerminalCanvas(
                terminal = terminal,
                revision = state.revision,
                fontSize = fontSizeSp.sp,
                scrollBack = scrollBack,
                palette = palette,
                selection = selection,
                matches = matches,
                onMetrics = { m, cols, rows ->
                    metrics = m
                    if (grid.value != cols to rows) grid.value = cols to rows
                },
                modifier = Modifier
                    .fillMaxSize()
                    .focusRequester(focus)
                    .focusable()
                    .scrollable(
                        orientation = Orientation.Vertical,
                        state = rememberScrollableState { delta ->
                            // A drag down scrolls back into the history, which
                            // is the direction a finger expects and the
                            // opposite of the line index.
                            val lines = (delta / metrics.height).toInt()
                            if (lines != 0) {
                                val limit = terminal.read { it.totalLines - it.rows }.coerceAtLeast(0)
                                scrollBack = (scrollBack + lines).coerceIn(0, limit)
                            }
                            delta
                        },
                    )
                    .pointerInput(Unit) {
                        detectTapGestures(
                            onTap = {
                                selection = null
                                focus.requestFocus()
                                keyboard?.show()
                            },
                        )
                    }
                    .pointerInput(metrics) {
                        // Long press then drag: the only selection gesture a
                        // terminal can have, because a plain drag is the
                        // scroll.
                        detectDragGesturesAfterLongPress(
                            onDragStart = { offset ->
                                val p = positionOf(offset.x, offset.y, metrics, firstVisible)
                                selection = p to p
                            },
                            onDrag = { change, _ ->
                                val sel = selection ?: return@detectDragGesturesAfterLongPress
                                selection = sel.first to
                                    positionOf(change.position.x, change.position.y, metrics, firstVisible)
                            },
                        )
                    },
            )

            // Recomputed whenever the grid changes shape, which on a phone is
            // an orientation change or the keyboard opening. Both must reach
            // the far end: a TUI repaints itself to the size it was told, and
            // one that was not told repaints to the old one.
            LaunchedEffect(grid.value) {
                val (cols, rows) = grid.value
                onResize(cols, rows)
            }
            LaunchedEffect(state.revision, scrollBack) {
                firstVisible = terminal.read {
                    (it.totalLines - 1 - scrollBack - grid.value.second + 1).coerceAtLeast(0)
                }
            }

            HiddenInput(
                focus = focus,
                onText = { text -> send(Keys.text(text, armed)) },
                onBackspace = { count -> repeat(count) { send(Keys.encode(Key.BACKSPACE, armed)) } },
                onKey = { key, mods -> send(Keys.encode(key, mods, terminal.applicationCursorKeys)) },
                armed = armed,
            )

            if (scrollBack > 0) {
                ScrollNotice(scrollBack) { scrollBack = 0 }
            }
        }

        AccessoryRow(
            keys = accessoryRow.ifEmpty { AccessoryKeys.DEFAULT },
            armed = armed,
            onModifier = { mods -> armed = mods },
            onAction = { action ->
                Keys.accessory(action, terminal.applicationCursorKeys, armed)?.let { send(it) }
            },
        )
    }

    LaunchedEffect(Unit) { focus.requestFocus() }
}

/** Everything the screen needs about the attachment, and nothing about how it works. */
data class TerminalUiState(
    val terminal: com.apexos.remote.core.term.Terminal,
    /** Bumped by the pump; what tells Compose the pixels are stale. */
    val revision: Long,
    val title: String,
    /** "attached", "reconnecting…", or whatever the machine said. */
    val status: String,
)

/**
 * The invisible field the soft keyboard talks to.
 *
 * The sentinel run is what makes backspace visible at all: a change that
 * shortens the text by one is one backspace, and the run is topped back up so
 * there is always something left to delete. [SENTINEL] is a zero-width space,
 * so a keyboard that decides to show the buffer shows nothing.
 */
@Composable
private fun HiddenInput(
    focus: FocusRequester,
    onText: (String) -> Unit,
    onBackspace: (Int) -> Unit,
    onKey: (Key, Mods) -> Unit,
    armed: Mods,
) {
    var value by remember { mutableStateOf(TextFieldValue(SENTINEL, TextRange(SENTINEL.length))) }
    BasicTextField(
        value = value,
        onValueChange = { next ->
            val before = value.text
            val after = next.text
            when {
                after.length > before.length -> {
                    val typed = after.substring(before.length).replace(SENTINEL_CHAR.toString(), "")
                    if (typed.isNotEmpty()) onText(typed)
                }
                after.length < before.length -> onBackspace(before.length - after.length)
                else -> Unit
            }
            value = TextFieldValue(SENTINEL, TextRange(SENTINEL.length))
        },
        modifier = Modifier
            .size(1.dp)
            .alpha(0f)
            .focusRequester(focus)
            .onPreviewKeyEvent { event ->
                val native = event.nativeKeyEvent
                if (event.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
                if (Keymap.isSystemKey(native.keyCode) || Keymap.isModifierKey(native.keyCode)) {
                    return@onPreviewKeyEvent false
                }
                val key = Keymap.keyOf(native.keyCode) ?: return@onPreviewKeyEvent false
                val mods = Keymap.modsOf(native.metaState)
                onKey(key, Mods(mods.ctrl || armed.ctrl, mods.alt || armed.alt, mods.shift))
                true
            },
        keyboardOptions = KeyboardOptions(
            // Autocorrect on a terminal turns `ls` into `Is`. Capitalisation
            // turns every command into a `command not found`.
            autoCorrectEnabled = false,
            capitalization = KeyboardCapitalization.None,
            keyboardType = KeyboardType.Ascii,
            imeAction = ImeAction.None,
        ),
    )
}

@Composable
private fun ScrollNotice(lines: Int, onBottom: () -> Unit) {
    Box(
        Modifier
            .fillMaxWidth()
            .padding(8.dp),
        contentAlignment = Alignment.TopEnd,
    ) {
        Text(
            text = "$lines lines up — tap to return",
            modifier = Modifier
                .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(4.dp))
                .padding(horizontal = 8.dp, vertical = 4.dp)
                .pointerInput(Unit) { detectTapGestures { onBottom() } },
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * The row above the keyboard.
 *
 * Scrollable, because a phone in portrait has room for about six buttons and
 * the shipped row has fourteen. A row that wrapped would take two lines of a
 * screen that has twenty-four.
 */
@Composable
private fun AccessoryRow(
    keys: List<AccessoryKey>,
    armed: Mods,
    onModifier: (Mods) -> Unit,
    onAction: (AccessoryAction) -> Unit,
) {
    LazyRow(
        Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 6.dp),
    ) {
        items(keys) { key ->
            val action = AccessoryKeys.parse(key.action)
            if (action != null) {
                val lit = when (action) {
                    is AccessoryAction.Modifier -> (action.ctrl && armed.ctrl) || (action.alt && armed.alt)
                    else -> false
                }
                AccessoryButton(key.label, lit) {
                    when (action) {
                        // A modifier arms the next press rather than sending
                        // anything, which is how a phone's shift key works and
                        // the only way one finger can press two keys.
                        is AccessoryAction.Modifier -> onModifier(
                            Mods(
                                ctrl = if (action.ctrl) !armed.ctrl else armed.ctrl,
                                alt = if (action.alt) !armed.alt else armed.alt,
                            ),
                        )
                        else -> onAction(action)
                    }
                }
            }
        }
    }
}

@Composable
private fun AccessoryButton(label: String, lit: Boolean, onClick: () -> Unit) {
    Box(
        Modifier
            .height(34.dp)
            .background(
                if (lit) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surface,
                RoundedCornerShape(5.dp),
            )
            .border(1.dp, MaterialTheme.colorScheme.outline.copy(alpha = 0.4f), RoundedCornerShape(5.dp))
            .pointerInput(onClick) { detectTapGestures { onClick() } }
            .padding(horizontal = 12.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            label,
            color = if (lit) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurface,
            fontFamily = FontFamily.Monospace,
            fontSize = 13.sp,
        )
    }
}

@Composable
@Suppress("LongParameterList")
private fun TerminalBar(
    title: String,
    subtitle: String,
    searching: Boolean,
    needle: String,
    matchCount: Int,
    onNeedle: (String) -> Unit,
    onSearchToggle: () -> Unit,
    onNextMatch: () -> Unit,
    onCopy: () -> Unit,
    onPaste: () -> Unit,
    onBack: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceVariant)) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onBack) {
                Icon(
                    androidx.compose.material.icons.Icons.Filled.ArrowBack,
                    contentDescription = "Back to the agent list",
                )
            }
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.titleSmall, maxLines = 1)
                Text(
                    subtitle,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
            TextButton("find", onSearchToggle)
            TextButton("copy", onCopy)
            TextButton("paste", onPaste)
        }
        if (searching) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextField(
                    value = needle,
                    onValueChange = onNeedle,
                    modifier = Modifier.weight(1f),
                    singleLine = true,
                    placeholder = { Text("find in scrollback") },
                )
                Text(
                    if (needle.isEmpty()) "" else "$matchCount",
                    Modifier.padding(horizontal = 8.dp),
                    style = MaterialTheme.typography.labelSmall,
                )
                TextButton("next", onNextMatch)
            }
        }
    }
}

@Composable
private fun TextButton(label: String, onClick: () -> Unit) {
    Text(
        label,
        Modifier
            .pointerInput(onClick) { detectTapGestures { onClick() } }
            .padding(horizontal = 8.dp, vertical = 6.dp),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.primary,
    )
}

/** A touch, in grid coordinates. */
internal fun positionOf(x: Float, y: Float, metrics: CellMetrics, firstVisibleLine: Int): Pos {
    val col = (x / metrics.width).toInt().coerceAtLeast(0)
    val row = (y / metrics.height).toInt().coerceAtLeast(0)
    return Pos(firstVisibleLine + row, col)
}

/**
 * A zero-width space.
 *
 * Eight of them, so a burst of backspaces is still a diff rather than an empty
 * field. Zero-width because a keyboard that chooses to display its buffer —
 * some do, above the keys — must display nothing.
 */
private const val SENTINEL_CHAR = '​'
private const val SENTINEL = "​​​​​​​​"

private val UnusedColour = Color.Transparent
