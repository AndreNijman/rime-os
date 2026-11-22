package com.rimeos.remote.ui.touchpad

import android.os.SystemClock
import android.view.HapticFeedbackConstants
import android.view.KeyCharacterMap
import android.view.KeyEvent as AndroidKeyEvent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.sizeIn
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.systemGestureExclusion
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.changedToDownIgnoreConsumed
import androidx.compose.ui.input.pointer.changedToUpIgnoreConsumed
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.hideFromAccessibility
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.rimeos.remote.core.Settings
import com.rimeos.remote.core.input.KeyMap
import com.rimeos.remote.core.input.Mod
import com.rimeos.remote.core.input.MouseButton
import com.rimeos.remote.core.input.Refusal
import com.rimeos.remote.core.input.TouchpadPhase
import com.rimeos.remote.core.input.TypedText
import com.rimeos.remote.ui.theme.MachineText
import com.rimeos.remote.ui.theme.RimeTones
import com.rimeos.remote.ui.theme.labelColumnWidth
import kotlin.math.ceil
import kotlin.math.roundToInt
import kotlinx.coroutines.delay

/** Test tags, for the device suite. */
object TouchpadTags {
    const val PAD = "touchpad-pad"
    const val FIELD = "touchpad-field"
    const val COMMAND = "touchpad-grant-command"
    const val STATE = "touchpad-state"
}

/**
 * The phone as the computer's touchpad and keyboard.
 *
 * The pad takes the whole middle of the screen; the two buttons sit under
 * it where a laptop's are; the keys a phone keyboard lacks (Esc, the
 * modifiers, arrows, F-keys) run along the bottom. On a wide screen (a
 * tablet, or a phone on its side) the buttons and keys move to a panel
 * beside the pad so the pad stays as large as it can be.
 *
 * When the pad cannot be used the reason is drawn inside it, in words, with
 * the one thing that would change it: never a pad that silently does
 * nothing.
 *
 * Every rule is in `core.input` (tested on a computer); this is layout and
 * wiring.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Suppress("DEPRECATION") // LocalClipboardManager: see TerminalScreen for why.
@Composable
fun TouchpadScreen(
    model: TouchpadViewModel,
    state: TouchpadUiState,
    settings: Settings,
    onNatural: (Boolean) -> Unit,
    onSpeed: (Float) -> Unit,
    onBack: () -> Unit,
) {
    val view = LocalView.current
    val keyboard = LocalSoftwareKeyboardController.current
    val fieldFocus = remember { FocusRequester() }
    val padFocus = remember { FocusRequester() }
    var options by remember { mutableStateOf(false) }
    var typing by remember { mutableStateOf(false) }
    var showFKeys by remember { mutableStateOf(false) }
    var field by remember { mutableStateOf(fieldValue(TypedText.EMPTY)) }
    val machine = state.machine ?: "the computer"
    val ready = state.phase == TouchpadPhase.Ready

    // A touchpad that dims and locks the phone mid-drag is not one.
    DisposableEffect(view) {
        view.keepScreenOn = true
        onDispose { view.keepScreenOn = false }
    }
    DisposableEffect(model, view) {
        model.onClick = { view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP) }
        onDispose { model.onClick = null }
    }
    LaunchedEffect(settings.touchpadNatural, settings.touchpadSpeed) {
        model.configure(settings.touchpadNatural, settings.touchpadSpeed)
    }
    // Focus on the screen itself, so a hardware keyboard types from the start.
    LaunchedEffect(Unit) { runCatching { padFocus.requestFocus() } }
    // A keyboard left up over a pad that cannot type is in the way.
    LaunchedEffect(ready) {
        if (!ready && typing) {
            keyboard?.hide()
            typing = false
        }
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { TitleWithStatus(machine, state.phase) },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                actions = {
                    TextButton(
                        onClick = { options = !options },
                        modifier = Modifier.semantics { stateDescription = if (options) "Shown" else "Hidden" },
                    ) { Text(if (options) "Done" else "Options") }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        Box(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .imePadding()
                .focusRequester(padFocus)
                .focusable()
                .onPreviewKeyEvent { event ->
                    val native = event.nativeKeyEvent
                    val down = event.type == KeyEventType.KeyDown
                    val soft = native.deviceId == KeyCharacterMap.VIRTUAL_KEYBOARD || native.device?.isVirtual == true
                    if (soft) {
                        // The soft keyboard's own keys: the few it sends as keys
                        // rather than text are sent as taps; the rest is the field's.
                        val code = when (native.keyCode) {
                            AndroidKeyEvent.KEYCODE_DEL -> KeyMap.BACKSPACE
                            AndroidKeyEvent.KEYCODE_ENTER, AndroidKeyEvent.KEYCODE_NUMPAD_ENTER -> KeyMap.ENTER
                            AndroidKeyEvent.KEYCODE_TAB -> KeyMap.TAB
                            AndroidKeyEvent.KEYCODE_FORWARD_DEL -> KeyMap.DELETE
                            else -> return@onPreviewKeyEvent false
                        }
                        if (down && native.repeatCount == 0 || down && code == KeyMap.BACKSPACE) model.tap(code)
                        true
                    } else {
                        model.hardware(native.keyCode, down, native.repeatCount > 0)
                    }
                },
        ) {
            // The field the soft keyboard types into. One dp, never seen, and
            // holding nothing but a marker (and whatever is still being
            // composed): what is committed goes to the computer and is gone.
            BasicTextField(
                value = field,
                onValueChange = { v ->
                    val composing = v.composition?.let { it.start until it.end }
                    field = fieldValue(model.typed(v.text, composing))
                },
                modifier = Modifier
                    .size(1.dp)
                    .semantics { hideFromAccessibility() }
                    .testTag(TouchpadTags.FIELD)
                    .focusRequester(fieldFocus),
                // A password field to the keyboard: no suggestions, no
                // autocorrect, and nothing typed here is learned by it.
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Password,
                    imeAction = ImeAction.None,
                ),
                textStyle = TextStyle(color = Color.Transparent),
                // Unspecified, not Transparent: see TerminalScreen.
                cursorBrush = SolidColor(Color.Unspecified),
            )

            BoxWithConstraints(Modifier.fillMaxSize()) {
                val wide = maxWidth > maxHeight && maxWidth >= 560.dp
                val toggleKeyboard = {
                    if (typing) {
                        keyboard?.hide()
                        runCatching { padFocus.requestFocus() }
                        typing = false
                    } else {
                        runCatching { fieldFocus.requestFocus() }
                        keyboard?.show()
                        typing = true
                    }
                }
                val controls: @Composable (Modifier) -> Unit = { m ->
                    Controls(
                        state = state,
                        enabled = ready,
                        typing = typing,
                        showFKeys = showFKeys,
                        wide = wide,
                        onFKeys = { showFKeys = !showFKeys },
                        onKeyboard = toggleKeyboard,
                        model = model,
                        modifier = m,
                    )
                }
                if (wide) {
                    Row(Modifier.fillMaxSize().padding(16.dp), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                        Column(Modifier.weight(1f).fillMaxHeight()) {
                            if (options) Options(settings, onNatural, onSpeed)
                            Pad(model, state, machine, Modifier.weight(1f).fillMaxWidth())
                        }
                        controls(Modifier.width(labelColumnWidth(320.dp)).fillMaxHeight())
                    }
                } else {
                    Column(Modifier.fillMaxSize().padding(horizontal = 12.dp, vertical = 8.dp)) {
                        if (options) Options(settings, onNatural, onSpeed)
                        Pad(model, state, machine, Modifier.weight(1f).fillMaxWidth())
                        Spacer(Modifier.height(10.dp))
                        controls(Modifier.fillMaxWidth())
                    }
                }
            }
        }
    }
}

private fun fieldValue(f: TypedText.Field) = TextFieldValue(
    text = f.text,
    selection = TextRange(f.cursor),
    composition = f.composing?.let { TextRange(it.first, it.last + 1) },
)

// ── the title ───────────────────────────────────────────────────────────

@Composable
private fun TitleWithStatus(machine: String, phase: TouchpadPhase) {
    val word = statusWord(phase)
    val colour = statusColour(phase)
    val dot by animateColorAsState(colour, tween(220), label = "status-dot")
    Column {
        Text(machine, style = MaterialTheme.typography.titleMedium, maxLines = 1)
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
        ) {
            Box(Modifier.size(7.dp).clip(CircleShape).background(dot))
            Spacer(Modifier.width(6.dp))
            Text(
                "Touchpad · $word",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.7f),
                maxLines = 1,
            )
        }
    }
}

/** The desktop's own warning and danger tones, resolved for the surface in use. */
private val Amber: Color
    @Composable @ReadOnlyComposable get() = RimeTones.forToken("warning")
private val Danger: Color
    @Composable @ReadOnlyComposable get() = RimeTones.forToken("danger")
private val Quiet: Color
    @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.55f)

private fun statusWord(phase: TouchpadPhase): String = when (phase) {
    TouchpadPhase.Ready -> "ready"
    TouchpadPhase.Connecting -> "connecting"
    TouchpadPhase.Paused -> "paused"
    TouchpadPhase.LinkLost -> "reconnecting"
    is TouchpadPhase.Waiting -> when (phase.why) {
        Refusal.LOCKED -> "locked"
        Refusal.BUSY -> "in use"
        else -> "waiting"
    }
    is TouchpadPhase.Stopped -> "stopped"
    is TouchpadPhase.GrantOff -> "off"
    TouchpadPhase.Unsupported -> "not available"
    is TouchpadPhase.Ended -> "closed"
    TouchpadPhase.Away -> "closed"
}

@Composable
@ReadOnlyComposable
private fun statusColour(phase: TouchpadPhase): Color = when (phase) {
    TouchpadPhase.Ready -> MaterialTheme.colorScheme.primary
    TouchpadPhase.Paused, TouchpadPhase.LinkLost, is TouchpadPhase.Waiting -> Amber
    is TouchpadPhase.Stopped, is TouchpadPhase.Ended -> Danger
    else -> Quiet
}

// ── options ─────────────────────────────────────────────────────────────

@Composable
private fun Options(settings: Settings, onNatural: (Boolean) -> Unit, onSpeed: (Float) -> Unit) {
    Surface(
        shape = RoundedCornerShape(14.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = Modifier.fillMaxWidth().padding(bottom = 10.dp),
    ) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Natural scrolling", style = MaterialTheme.typography.bodyLarge)
                    Text(
                        "Content follows your fingers, as on a phone",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.7f),
                    )
                }
                Switch(checked = settings.touchpadNatural, onCheckedChange = onNatural)
            }
            Spacer(Modifier.height(6.dp))
            Text("Pointer speed", style = MaterialTheme.typography.bodyLarge)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Slow", style = MaterialTheme.typography.labelMedium)
                Slider(
                    value = settings.touchpadSpeed,
                    onValueChange = onSpeed,
                    valueRange = 0.5f..2f,
                    // The Rime scheme leaves the container slots at Material's
                    // defaults (lavender); the track takes the accent instead.
                    colors = SliderDefaults.colors(
                        inactiveTrackColor = MaterialTheme.colorScheme.primary.copy(alpha = 0.18f),
                        inactiveTickColor = MaterialTheme.colorScheme.primary.copy(alpha = 0.5f),
                    ),
                    modifier = Modifier
                        .weight(1f)
                        .padding(horizontal = 10.dp)
                        .semantics { contentDescription = "Pointer speed" },
                )
                Text("Fast", style = MaterialTheme.typography.labelMedium)
            }
        }
    }
}

// ── the pad ─────────────────────────────────────────────────────────────

private val PadShape = RoundedCornerShape(topStart = 22.dp, topEnd = 22.dp, bottomStart = 8.dp, bottomEnd = 8.dp)

@Composable
private fun Pad(model: TouchpadViewModel, state: TouchpadUiState, machine: String, modifier: Modifier) {
    val fingers = remember { mutableStateMapOf<Long, Offset>() }
    var touched by remember { mutableStateOf(false) }
    val ready = state.phase == TouchpadPhase.Ready
    val hintAlpha by animateFloatAsState(if (touched || !ready) 0f else 1f, tween(400), label = "hint")
    val edge by animateColorAsState(
        if (ready) MaterialTheme.colorScheme.primary.copy(alpha = 0.35f) else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.10f),
        tween(250),
        label = "pad-edge",
    )
    val teal = MaterialTheme.colorScheme.primary

    // The note that the computer let go by itself: shown for a few seconds.
    var releasedShown by remember { mutableStateOf(false) }
    LaunchedEffect(state.releasedAt) {
        if (state.releasedAt != null) {
            releasedShown = true
            delay(3_000)
            releasedShown = false
        }
    }

    Box(
        modifier
            .clip(PadShape)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .border(BorderStroke(1.dp, edge), PadShape)
            // The system's back gesture lives at the screen's edges; on a pad
            // that reaches them, a drag from the edge is a pointer move.
            .systemGestureExclusion()
            .testTag(TouchpadTags.PAD)
            .semantics {
                contentDescription = "Touchpad for $machine"
                stateDescription = statusWord(state.phase)
                if (ready) {
                    customActions = listOf(
                        CustomAccessibilityAction("Left click") { model.click(MouseButton.LEFT); true },
                        CustomAccessibilityAction("Right click") { model.click(MouseButton.RIGHT); true },
                        CustomAccessibilityAction("Middle click") { model.click(MouseButton.MIDDLE); true },
                    )
                }
            }
            .pointerInput(model) {
                awaitPointerEventScope {
                    while (true) {
                        val event = awaitPointerEvent()
                        for (c in event.changes) {
                            val id = c.id.value
                            when {
                                c.changedToDownIgnoreConsumed() -> {
                                    touched = true
                                    fingers[id] = c.position
                                    model.down(id, c.position.x.toDp().value.toDouble(), c.position.y.toDp().value.toDouble(), c.uptimeMillis)
                                }
                                c.changedToUpIgnoreConsumed() -> {
                                    fingers.remove(id)
                                    model.up(id, c.uptimeMillis)
                                }
                                c.pressed && c.positionChanged() -> {
                                    // Every sample the system batched, not just the
                                    // last: the curve reads speed from them.
                                    for (h in c.historical) {
                                        model.move(id, h.position.x.toDp().value.toDouble(), h.position.y.toDp().value.toDouble(), h.uptimeMillis)
                                    }
                                    fingers[id] = c.position
                                    model.move(id, c.position.x.toDp().value.toDouble(), c.position.y.toDp().value.toDouble(), c.uptimeMillis)
                                }
                            }
                            c.consume()
                        }
                    }
                }
            },
    ) {
        // Where the fingers are, softly: the pad answering the touch.
        Canvas(Modifier.fillMaxSize()) {
            if (ready) {
                for (p in fingers.values) {
                    drawCircle(teal.copy(alpha = 0.16f), radius = 34.dp.toPx(), center = p)
                    drawCircle(teal.copy(alpha = 0.55f), radius = 34.dp.toPx(), center = p, style = Stroke(1.5.dp.toPx()))
                }
            }
        }

        if (ready) {
            Column(
                Modifier.align(Alignment.Center).padding(24.dp).semantics { hideFromAccessibility() },
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Hint("One finger moves the pointer", hintAlpha)
                Hint("Tap to click · two fingers to scroll or right-click", hintAlpha)
                Hint("Tap, then drag, to hold the button", hintAlpha)
            }
        }

        AnimatedVisibility(
            visible = releasedShown && ready,
            enter = fadeIn(tween(150)),
            exit = fadeOut(tween(300)),
            modifier = Modifier.align(Alignment.TopCenter).padding(top = 12.dp),
        ) {
            Surface(shape = RoundedCornerShape(50), color = MaterialTheme.colorScheme.background.copy(alpha = 0.85f)) {
                Text(
                    "The computer let go after the phone went quiet",
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(horizontal = 14.dp, vertical = 6.dp),
                )
            }
        }

        if (!ready) {
            PhasePanel(model, state.phase, machine, Modifier.align(Alignment.Center))
        }
    }
}

@Composable
private fun Hint(text: String, alpha: Float) {
    Text(
        text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.55f * alpha),
        textAlign = TextAlign.Center,
        modifier = Modifier.padding(vertical = 3.dp),
    )
}

// ── what the pad says when it cannot be used ───────────────────────────

@Suppress("DEPRECATION")
@Composable
private fun PhasePanel(model: TouchpadViewModel, phase: TouchpadPhase, machine: String, modifier: Modifier) {
    var now by remember { mutableLongStateOf(SystemClock.uptimeMillis()) }
    LaunchedEffect(phase) {
        while (phase is TouchpadPhase.Stopped || phase is TouchpadPhase.Waiting) {
            now = SystemClock.uptimeMillis()
            delay(250)
        }
    }
    Column(
        modifier
            .widthIn(max = 460.dp)
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp, vertical = 20.dp)
            .testTag(TouchpadTags.STATE)
            .semantics(mergeDescendants = false) { liveRegion = LiveRegionMode.Polite },
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        when (phase) {
            TouchpadPhase.Away, TouchpadPhase.Connecting -> {
                CircularProgressIndicator(strokeWidth = 2.5.dp)
                Spacer(Modifier.height(16.dp))
                PanelTitle("Connecting to $machine")
                PanelBody("The touchpad is opening.")
            }
            TouchpadPhase.LinkLost -> {
                CircularProgressIndicator(strokeWidth = 2.5.dp, color = Amber)
                Spacer(Modifier.height(16.dp))
                PanelTitle("Reconnecting")
                PanelBody("The connection to $machine dropped. The touchpad comes back by itself once it is up again.")
            }
            TouchpadPhase.Paused -> {
                Glyph("◐", Amber)
                PanelTitle("$machine is locked")
                PanelBody("Nothing you do here reaches it while it is locked. Unlock it at the computer and the touchpad carries on by itself.")
            }
            is TouchpadPhase.Waiting -> {
                val (title, body) = when (phase.why) {
                    Refusal.LOCKED -> "$machine is locked" to
                        "The touchpad opens once it is unlocked at the computer. Trying again by itself."
                    Refusal.BUSY -> "Another phone has the touchpad" to
                        "One phone at a time. This one takes over when the other lets go."
                    Refusal.UNAVAILABLE -> "Not available right now" to
                        phase.message.ifBlank { "$machine has no desktop to send input to (Gaming Mode, or nobody is logged in)." }
                    Refusal.STOPPING -> "Rime Remote is restarting" to "Trying again in a moment."
                    else -> "The touchpad closed" to "Opening it again."
                }
                Glyph(if (phase.why == Refusal.LOCKED) "◐" else "…", Amber)
                PanelTitle(title)
                PanelBody(body)
                val left = ((phase.retryAt - now) / 1000.0).let { ceil(it).roundToInt() }.coerceAtLeast(0)
                Spacer(Modifier.height(12.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (left > 0) "Next try in $left s" else "Trying…",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f),
                    )
                    Spacer(Modifier.width(12.dp))
                    TextButton(onClick = { model.retry() }) { Text("Try now") }
                }
            }
            is TouchpadPhase.Stopped -> {
                val left = ceil((phase.until - now) / 1000.0).roundToInt().coerceAtLeast(0)
                Glyph("■", Danger)
                PanelTitle("Stopped at the computer")
                PanelBody(
                    if (left > 0) {
                        "Someone at $machine stopped the touchpad. It can be opened again in $left s, or sooner if they allow this phone again."
                    } else {
                        "Someone at $machine stopped the touchpad. You can open it again now."
                    },
                )
                Spacer(Modifier.height(16.dp))
                Button(
                    onClick = { model.retry() },
                    enabled = left == 0,
                    shape = RoundedCornerShape(10.dp),
                ) { Text(if (left > 0) "Open again in $left s" else "Open the touchpad") }
            }
            is TouchpadPhase.GrantOff -> {
                val clipboard = LocalClipboardManager.current
                var copied by remember { mutableStateOf(false) }
                Glyph("⌨", MaterialTheme.colorScheme.primary)
                PanelTitle("Turn the touchpad on at the computer")
                PanelBody(
                    "This phone is paired with $machine but is not allowed to move its pointer or type. " +
                        "That is only ever allowed at the computer. In a terminal there, run:",
                )
                Spacer(Modifier.height(14.dp))
                Surface(
                    shape = RoundedCornerShape(10.dp),
                    color = MaterialTheme.colorScheme.background,
                    border = BorderStroke(1.dp, MaterialTheme.colorScheme.outline.copy(alpha = 0.6f)),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    SelectionContainer {
                        Text(
                            phase.command,
                            style = MachineText.copy(color = MaterialTheme.colorScheme.onBackground),
                            modifier = Modifier.padding(horizontal = 14.dp, vertical = 12.dp).testTag(TouchpadTags.COMMAND),
                        )
                    }
                }
                Spacer(Modifier.height(12.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    OutlinedButton(
                        onClick = {
                            clipboard.setText(AnnotatedString(phase.command))
                            copied = true
                        },
                        shape = RoundedCornerShape(10.dp),
                    ) { Text(if (copied) "Copied" else "Copy") }
                    Button(onClick = { model.retry() }, shape = RoundedCornerShape(10.dp)) { Text("Check again") }
                }
                Spacer(Modifier.height(10.dp))
                Text(
                    "It turns off the same way, with off instead of on.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f),
                    textAlign = TextAlign.Center,
                )
            }
            TouchpadPhase.Unsupported -> {
                Glyph("↑", Quiet)
                PanelTitle("Update Rime on $machine")
                PanelBody("Its Rime Remote is too old for the touchpad. Once it is updated, this screen works.")
            }
            is TouchpadPhase.Ended -> {
                Glyph("×", Danger)
                PanelTitle("$machine closed the touchpad")
                if (phase.message.isNotBlank()) PanelBody(phase.message)
                Spacer(Modifier.height(16.dp))
                Button(onClick = { model.retry() }, shape = RoundedCornerShape(10.dp)) { Text("Try again") }
            }
            TouchpadPhase.Ready -> {}
        }
    }
}

@Composable
private fun Glyph(text: String, colour: Color) {
    Box(
        Modifier.sizeIn(minWidth = 48.dp, minHeight = 48.dp).clip(CircleShape).background(colour.copy(alpha = 0.14f)).semantics { hideFromAccessibility() },
        contentAlignment = Alignment.Center,
    ) {
        Text(text, color = colour, style = MaterialTheme.typography.headlineSmall, modifier = Modifier.padding(8.dp))
    }
    Spacer(Modifier.height(14.dp))
}

@Composable
private fun PanelTitle(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleLarge.copy(fontWeight = FontWeight.Medium),
        textAlign = TextAlign.Center,
    )
    Spacer(Modifier.height(8.dp))
}

@Composable
private fun PanelBody(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.78f),
        textAlign = TextAlign.Center,
    )
}

// ── buttons and keys ───────────────────────────────────────────────────

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Controls(
    state: TouchpadUiState,
    enabled: Boolean,
    typing: Boolean,
    showFKeys: Boolean,
    wide: Boolean,
    onFKeys: () -> Unit,
    onKeyboard: () -> Unit,
    model: TouchpadViewModel,
    modifier: Modifier,
) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            PadButton(
                "Left",
                enabled,
                RoundedCornerShape(topStart = 8.dp, topEnd = 4.dp, bottomStart = 18.dp, bottomEnd = 4.dp),
                Modifier.weight(1f),
            ) { model.button(MouseButton.LEFT, it) }
            PadButton(
                "Right",
                enabled,
                RoundedCornerShape(topStart = 4.dp, topEnd = 8.dp, bottomStart = 4.dp, bottomEnd = 18.dp),
                Modifier.weight(1f),
            ) { model.button(MouseButton.RIGHT, it) }
        }

        val mods = listOf(Mod.CTRL to "Ctrl", Mod.ALT to "Alt", Mod.SUPER to "Super", Mod.SHIFT to "Shift")
        val arrows = listOf(KeyMap.LEFT to "←", KeyMap.UP to "↑", KeyMap.DOWN to "↓", KeyMap.RIGHT to "→")
        val arrowNames = mapOf(KeyMap.LEFT to "Left arrow", KeyMap.UP to "Up arrow", KeyMap.DOWN to "Down arrow", KeyMap.RIGHT to "Right arrow")

        val keyboardKey: @Composable () -> Unit = {
            Key(
                if (typing) "Hide keys" else "Keyboard",
                enabled = enabled || typing,
                accent = true,
                selected = typing,
                description = if (typing) "Hide the keyboard" else "Show the keyboard",
                onClick = onKeyboard,
            )
        }
        val first: @Composable () -> Unit = {
            keyboardKey()
            Key("Esc", enabled, description = "Escape") { model.tap(KeyMap.ESC) }
            Key("Tab", enabled) { model.tap(KeyMap.TAB) }
            for ((mod, label) in mods) {
                val locked = mod in state.locked
                val armed = mod in state.armed
                Key(
                    label,
                    enabled,
                    selected = armed,
                    locked = locked,
                    state = when {
                        locked -> "locked on"
                        armed -> "on for the next key"
                        else -> "off"
                    },
                ) { model.toggle(mod) }
            }
            for ((code, label) in arrows) Key(label, enabled, description = arrowNames[code]) { model.tap(code) }
            Key(if (showFKeys) "Fn ▴" else "Fn ▾", enabled, description = if (showFKeys) "Hide more keys" else "More keys", onClick = onFKeys)
        }
        val more: @Composable () -> Unit = {
            KeyMap.F.forEachIndexed { i, code -> Key("F${i + 1}", enabled) { model.tap(code) } }
            Key("Home", enabled) { model.tap(KeyMap.HOME) }
            Key("End", enabled) { model.tap(KeyMap.END) }
            Key("PgUp", enabled, description = "Page up") { model.tap(KeyMap.PAGE_UP) }
            Key("PgDn", enabled, description = "Page down") { model.tap(KeyMap.PAGE_DOWN) }
            Key("Del", enabled, description = "Delete") { model.tap(KeyMap.DELETE) }
            Key("Ins", enabled, description = "Insert") { model.tap(KeyMap.INSERT) }
        }

        if (wide) {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) { first() }
                if (showFKeys) {
                    Spacer(Modifier.height(6.dp))
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) { more() }
                }
            }
        } else {
            Row(
                Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) { first() }
            AnimatedVisibility(visible = showFKeys) {
                Row(
                    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) { more() }
            }
        }
    }
}

/** A button held as long as the finger is on it, as a touchpad's is. */
@Composable
private fun PadButton(label: String, enabled: Boolean, shape: Shape, modifier: Modifier, onPress: (Boolean) -> Unit) {
    var down by remember { mutableStateOf(false) }
    val fill by animateColorAsState(
        when {
            down -> MaterialTheme.colorScheme.primaryContainer
            else -> MaterialTheme.colorScheme.surfaceVariant
        },
        tween(90),
        label = "button-fill",
    )
    Box(
        modifier
            .height(60.dp)
            .clip(shape)
            .background(fill)
            .border(BorderStroke(1.dp, MaterialTheme.colorScheme.onSurface.copy(alpha = 0.08f)), shape)
            .pointerInput(enabled) {
                if (!enabled) return@pointerInput
                awaitEachGesture {
                    awaitFirstDown().consume()
                    down = true
                    onPress(true)
                    try {
                        waitForUpOrCancellation()
                    } finally {
                        down = false
                        onPress(false)
                    }
                }
            }
            .semantics(mergeDescendants = true) {
                role = Role.Button
                contentDescription = "$label click"
                if (enabled) {
                    onClick {
                        onPress(true)
                        onPress(false)
                        true
                    }
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = if (enabled) 0.85f else 0.35f),
        )
    }
}

@Composable
private fun Key(
    label: String,
    enabled: Boolean,
    selected: Boolean = false,
    locked: Boolean = false,
    accent: Boolean = false,
    description: String? = null,
    state: String? = null,
    minWidth: Dp = 48.dp,
    onClick: () -> Unit,
) {
    // Locked is the strongest fill the scheme has; armed (once) a quieter
    // one with an edge, so the two read apart at a glance in either theme.
    val scheme = MaterialTheme.colorScheme
    val container = when {
        locked -> scheme.primary
        selected -> scheme.primaryContainer
        else -> scheme.surfaceVariant
    }.let { if (enabled || !(locked || selected)) it else it.copy(alpha = 0.45f) }
    val content = when {
        locked -> scheme.onPrimary
        selected -> scheme.onPrimaryContainer
        accent -> scheme.primary
        else -> scheme.onSurfaceVariant
    }
    val edge = when {
        locked -> null
        selected -> BorderStroke(1.dp, scheme.primary)
        accent -> BorderStroke(1.dp, scheme.primary.copy(alpha = if (enabled) 0.6f else 0.25f))
        else -> BorderStroke(1.dp, scheme.onSurface.copy(alpha = 0.08f))
    }
    Surface(
        onClick = onClick,
        enabled = enabled,
        shape = RoundedCornerShape(10.dp),
        color = container,
        contentColor = content,
        border = edge,
        modifier = Modifier
            .defaultMinSize(minWidth = minWidth, minHeight = 48.dp)
            .semantics {
                description?.let { contentDescription = it }
                state?.let { stateDescription = it }
                if (selected || locked) this.selected = true
            },
    ) {
        Box(Modifier.padding(horizontal = 12.dp), contentAlignment = Alignment.Center) {
            Text(
                label,
                style = MaterialTheme.typography.labelLarge,
                color = content.copy(alpha = if (enabled) 1f else 0.35f),
                maxLines = 1,
            )
        }
    }
}
