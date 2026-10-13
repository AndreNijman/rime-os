package com.apexos.remote.ui.theme

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import com.apexos.remote.core.agent.AgentStates
import com.apexos.remote.core.agent.Tone

/**
 * The five status tokens the desktop shell has, and the phone did not.
 *
 * ## Why these are literal hexes and the rest of the theme is not
 *
 * `apex-shell/src/theme/Colors.qml` holds them as literals too, in dark/light
 * pairs, and it does that deliberately: the rest of its palette is derived
 * from the user's wallpaper by matugen, and a status colour that followed a
 * wallpaper would mean "this agent needs permission" in whatever colour the
 * wallpaper happened to suggest. The phone has the same tension — Material You
 * is offered here — and resolves it the same way.
 *
 * So: **the four status tokens do not follow dynamic colour.** `subtext` does,
 * because it is not a status; it is "text that matters less", which is exactly
 * what `onSurfaceVariant` is on both sides.
 *
 * The values are copied from `Colors.qml` lines 99-114 and are checked against
 * it by `tools/check-agent-state.sh`'s sibling concern — the state table —
 * while the hexes themselves are asserted in `StatusColoursTest`.
 */
object StatusColours {
    // Dark, which is what APEX ships.
    val InfoDark = Color(0xFF89B4FA)
    val WarningDark = Color(0xFFF5C47A)
    val AttentionDark = Color(0xFFD0BCFF)
    val DangerDark = Color(0xFFF87171)
    val SuccessDark = Color(0xFFA6E3A1)

    // Light.
    val InfoLight = Color(0xFF0B57D0)
    val WarningLight = Color(0xFF7A4A00)
    val AttentionLight = Color(0xFF6B3FA0)
    val DangerLight = Color(0xFF8C1D18)
    val SuccessLight = Color(0xFF17752F)

    /**
     * The two inks a solid badge picks between.
     *
     * `Colors.qml`'s `fixedLight` and `fixedDark`. Fixed, and named so: they
     * do not move with the scheme, because the thing they have to contrast
     * with is the status colour and not the background.
     */
    val FixedLight = Color(0xFFFFFFFF)
    val FixedDark = Color(0xFF1E1E2E)

    /**
     * Resolve a token name to a colour.
     *
     * A name and not an enum, because the token names come out of
     * [AgentStates] which took them from the desktop's table, and a second
     * enumeration of the same six strings is a second thing to keep in step.
     */
    @Composable
    @ReadOnlyComposable
    fun of(token: String, dark: Boolean): Color = when (token) {
        "info" -> if (dark) InfoDark else InfoLight
        "warning" -> if (dark) WarningDark else WarningLight
        "attention" -> if (dark) AttentionDark else AttentionLight
        "danger" -> if (dark) DangerDark else DangerLight
        "success" -> if (dark) SuccessDark else SuccessLight
        // Not a literal: "text that matters less" is a property of the scheme,
        // and under Material You it should follow the wallpaper like every
        // other piece of secondary text.
        "subtext" -> MaterialTheme.colorScheme.onSurfaceVariant
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }

    @Composable
    @ReadOnlyComposable
    fun of(tone: Tone, dark: Boolean): Color = of(tone.token, dark)

    /**
     * Which ink to draw on a solid badge of [fill].
     *
     * `Colors.qml`'s `onStatus`, ported exactly — and the port matters,
     * because it is **a comparison of two candidates, not a lightness
     * threshold**. The two are not the same function: a threshold at 0.5 puts
     * white on `#f87171`, and the comparison puts `#1e1e2e` on it, which is
     * what the desktop actually draws.
     */
    fun onStatus(fill: Color): Color =
        if (contrastRatio(FixedDark, fill) >= contrastRatio(FixedLight, fill)) FixedDark else FixedLight

    /** WCAG 2.1 relative-luminance contrast: `(L + 0.05) / (l + 0.05)`. */
    fun contrastRatio(a: Color, b: Color): Float {
        val la = a.luminance()
        val lb = b.luminance()
        val hi = maxOf(la, lb)
        val lo = minOf(la, lb)
        return (hi + 0.05f) / (lo + 0.05f)
    }
}
