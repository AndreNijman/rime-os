package com.apexos.remote.ui.agent

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.apexos.remote.core.agent.AgentStates
import com.apexos.remote.core.agent.Weight
import com.apexos.remote.ui.theme.StatusColours

/**
 * A session's state, as one chip.
 *
 * ## It takes a state and nothing else
 *
 * Not the agent, not the session, not a colour. `StateBadge.qml` is built the
 * same way, and a test landed on the desktop today asserting that all five
 * session kinds render identically — which is only true because the badge is
 * given no way to know which kind it is looking at. A badge that took a
 * session could grow a special case for one agent, and the two would drift.
 *
 * The geometry is `StateBadge.qml`'s: the chip is square, the corner radius is
 * 0.3 of the size, the glyph is 0.62 of it, a tint is the tone at 0.18 and an
 * outline is a one-pixel ring.
 */
@Composable
fun StateBadge(
    state: String?,
    modifier: Modifier = Modifier,
    size: Dp = 22.dp,
    dark: Boolean = isSystemInDarkTheme(),
) {
    val tone = AgentStates.tone(state)
    val weight = tone.weight
    val colour = StatusColours.of(tone, dark)
    val fill = when (weight) {
        Weight.SOLID -> colour
        Weight.TINT -> colour.copy(alpha = Weight.TINT_ALPHA)
        else -> Color.Transparent
    }
    val ink = if (weight == Weight.SOLID) StatusColours.onStatus(colour) else colour

    Box(
        modifier
            .size(size)
            .clip(RoundedCornerShape(size * CORNER))
            .background(fill)
            .then(
                if (weight == Weight.OUTLINE) {
                    Modifier.border(1.dp, colour, RoundedCornerShape(size * CORNER))
                } else {
                    Modifier
                },
            )
            // The colour is the whole of the message, so a screen reader that
            // cannot see it must be told the same thing in words. "Working"
            // and "needs permission" are what the badge means; a blank chip is
            // what it would otherwise announce.
            .semantics { contentDescription = AgentStates.label(state) },
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = glyph(state),
            color = ink,
            fontSize = (size.value * GLYPH).sp,
            fontWeight = FontWeight.Bold,
        )
    }
}

/**
 * The character in the chip.
 *
 * ASCII rather than the desktop's Nerd Font glyphs, and that is a decision
 * rather than an omission: the phone does not ship a patched font, and a
 * missing glyph renders as a hollow box — which on the badge that means "this
 * agent needs your permission" would be worse than a letter.
 */
private fun glyph(state: String?): String = when (state) {
    AgentStates.STARTING -> "·"
    AgentStates.WORKING -> "▶"
    AgentStates.WAITING_FOR_USER -> "?"
    AgentStates.PERMISSION_REQUEST -> "!"
    AgentStates.COMPLETE -> "✓"
    AgentStates.FAILED -> "✕"
    AgentStates.EXITED -> "—"
    else -> "·"
}

private const val CORNER = 0.3f
private const val GLYPH = 0.62f
