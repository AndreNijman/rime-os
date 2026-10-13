package com.apexos.remote.ui.theme

import android.app.Activity
import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import androidx.core.view.WindowCompat

/**
 * The APEX design language on a phone.
 *
 * ## Where these colours come from
 *
 * Not invented here. `files/desktop/apex-greet/shell.qml` is the first surface
 * of APEX anybody sees — the login screen — and it is already a deliberate
 * palette: `#1a282a` ground, `#cdd6f4` text, `#94e2d5` for the thing being
 * drawn attention to, `#ff5c5c` for a refusal. Those four are lifted verbatim,
 * so a phone and a laptop are recognisably the same product rather than two
 * things with the same name.
 *
 * ## Dynamic colour is offered and is not the default
 *
 * Material You is available from API 31 and it is genuinely nicer on a phone
 * whose owner has set a wallpaper they like. It is also, by construction, *not*
 * APEX's colours — and this is the app that shows which machine is about to run
 * a command as root. So: the APEX palette is what ships, dynamic colour is a
 * setting, and [ApexRemoteTheme] takes it as a parameter rather than reading a
 * build constant, because the choice belongs to the person holding the phone.
 */
object ApexPalette {
    /** The greeter's ground. */
    val Ink = Color(0xFF1A282A)
    /** The greeter's text. */
    val Bone = Color(0xFFCDD6F4)
    /** The greeter's accent: what is being drawn attention to. */
    val Teal = Color(0xFF94E2D5)
    /** The greeter's refusal colour. */
    val Alarm = Color(0xFFFF5C5C)

    /** Derived, and only where a Material scheme needs a slot the greeter has no opinion about. */
    val InkRaised = Color(0xFF223436)
    val InkSunken = Color(0xFF121D1F)
    val TealDeep = Color(0xFF0F5C52)
    val Paper = Color(0xFFF4F7F7)
    val PaperRaised = Color(0xFFFFFFFF)
    val InkOnPaper = Color(0xFF16292B)
}

private val ApexDark = darkColorScheme(
    primary = ApexPalette.Teal,
    onPrimary = ApexPalette.InkSunken,
    primaryContainer = ApexPalette.TealDeep,
    onPrimaryContainer = ApexPalette.Bone,
    secondary = ApexPalette.Bone,
    onSecondary = ApexPalette.InkSunken,
    background = ApexPalette.Ink,
    onBackground = ApexPalette.Bone,
    surface = ApexPalette.Ink,
    onSurface = ApexPalette.Bone,
    surfaceVariant = ApexPalette.InkRaised,
    onSurfaceVariant = ApexPalette.Bone,
    error = ApexPalette.Alarm,
    onError = ApexPalette.InkSunken,
    outline = ApexPalette.TealDeep,
)

private val ApexLight = lightColorScheme(
    primary = ApexPalette.TealDeep,
    onPrimary = ApexPalette.Paper,
    primaryContainer = ApexPalette.Teal,
    onPrimaryContainer = ApexPalette.InkOnPaper,
    secondary = ApexPalette.InkOnPaper,
    onSecondary = ApexPalette.Paper,
    background = ApexPalette.Paper,
    onBackground = ApexPalette.InkOnPaper,
    surface = ApexPalette.Paper,
    onSurface = ApexPalette.InkOnPaper,
    surfaceVariant = ApexPalette.PaperRaised,
    onSurfaceVariant = ApexPalette.InkOnPaper,
    error = Color(0xFFB3261E),
    onError = ApexPalette.Paper,
    outline = ApexPalette.TealDeep,
)

/**
 * Monospace where the content is machine text.
 *
 * A device key, a machine name and a terminal are all things a human compares
 * character by character, and a proportional font makes `l`, `1` and `I` the
 * same shape. `FontFamily.Monospace` rather than a bundled face: the app is
 * already 41 MB and the platform's monospace is legible everywhere.
 */
val ApexTypography = Typography().let { base ->
    base.copy(
        bodySmall = base.bodySmall.copy(fontFamily = FontFamily.Monospace),
        labelSmall = base.labelSmall.copy(
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.Medium,
            letterSpacing = 0.5.sp,
        ),
    )
}

/** Machine text: a key, an id, a path. Compared by eye, so never proportional. */
val MachineText: TextStyle = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 13.sp)

@Composable
fun ApexRemoteTheme(
    dark: Boolean = isSystemInDarkTheme(),
    useDynamicColour: Boolean = false,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val scheme = when {
        useDynamicColour && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        dark -> ApexDark
        else -> ApexLight
    }
    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as Activity).window
            // Edge to edge, and the status-bar icons flipped to match the
            // scheme rather than the system's idea of it — a light scheme under
            // a dark system theme otherwise gets white icons on white.
            WindowCompat.getInsetsController(window, view).isAppearanceLightStatusBars = !dark
        }
    }
    MaterialTheme(colorScheme = scheme, typography = ApexTypography, content = content)
}
