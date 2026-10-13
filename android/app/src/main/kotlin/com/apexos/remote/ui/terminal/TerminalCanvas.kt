package com.apexos.remote.ui.terminal

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.TextUnit
import com.apexos.remote.core.term.Attrs
import com.apexos.remote.core.term.Colour
import com.apexos.remote.core.term.Line
import com.apexos.remote.core.term.Match
import com.apexos.remote.core.term.Pos
import com.apexos.remote.core.term.Terminal

/**
 * The sixteen colours a palette index 0..15 means.
 *
 * These are the renderer's, not the protocol's: `SGR 31` says "your red", and
 * which red that is has always been the terminal's business. They are chosen
 * to sit with the APEX palette — the greeter's `#cdd6f4` text and `#1a282a`
 * ground — rather than being the VGA sixteen, which look wrong beside it.
 *
 * Indices 16..255 are **not** here, deliberately: those are arithmetic that
 * every terminal agrees on, and `Colour.resolveCube` does it.
 */
private val AnsiDark = intArrayOf(
    0xFF45475A.toInt(), // 0 black, lifted off the ground so it is visible on it
    0xFFF38BA8.toInt(), // 1 red
    0xFFA6E3A1.toInt(), // 2 green
    0xFFF9E2AF.toInt(), // 3 yellow
    0xFF89B4FA.toInt(), // 4 blue
    0xFFF5C2E7.toInt(), // 5 magenta
    0xFF94E2D5.toInt(), // 6 cyan, the greeter's accent
    0xFFBAC2DE.toInt(), // 7 white
    0xFF585B70.toInt(), // 8 bright black
    0xFFFF8098.toInt(),
    0xFFB9F0B4.toInt(),
    0xFFFFECC0.toInt(),
    0xFF9EC5FF.toInt(),
    0xFFFFD0F4.toInt(),
    0xFFA9F0E3.toInt(),
    0xFFE6EAF5.toInt(),
)

private val AnsiLight = intArrayOf(
    0xFF3C3C43.toInt(),
    0xFFB3261E.toInt(),
    0xFF17752F.toInt(),
    0xFF7A4A00.toInt(),
    0xFF0B57D0.toInt(),
    0xFF8B2FA0.toInt(),
    0xFF0F5C52.toInt(),
    0xFF5A5D63.toInt(),
    0xFF6B6B73.toInt(),
    0xFFD03A30.toInt(),
    0xFF1E8F3A.toInt(),
    0xFF9A6000.toInt(),
    0xFF1A6BE0.toInt(),
    0xFFA83BBE.toInt(),
    0xFF157A6E.toInt(),
    0xFF2B2D31.toInt(),
)

/**
 * What the terminal draws with, resolved once per frame rather than per cell.
 */
class TerminalPalette(
    val foreground: Int,
    val background: Int,
    val cursor: Int,
    val selection: Int,
    val highlight: Int,
    private val ansi: IntArray,
) {
    /** A packed [Colour] to an ARGB int. */
    fun resolve(colour: Int, isForeground: Boolean): Int = when (Colour.kind(colour)) {
        Colour.KIND_DEFAULT -> if (isForeground) foreground else background
        Colour.KIND_INDEXED -> {
            val index = Colour.index(colour)
            if (index < 16) ansi[index] else Colour.resolveCube(index)?.let { toArgb(it) } ?: foreground
        }
        else -> toArgb(colour)
    }

    private fun toArgb(rgb: Int): Int =
        (0xFF shl 24) or (Colour.red(rgb) shl 16) or (Colour.green(rgb) shl 8) or Colour.blue(rgb)
}

@Composable
fun rememberTerminalPalette(dark: Boolean = isSystemInDarkTheme()): TerminalPalette {
    val scheme = MaterialTheme.colorScheme
    val fg = scheme.onSurface.toArgb()
    val bg = scheme.surface.toArgb()
    val cursor = scheme.primary.toArgb()
    val selection = scheme.primary.copy(alpha = 0.35f).toArgb()
    val highlight = scheme.tertiary.copy(alpha = 0.45f).toArgb()
    return remember(fg, bg, cursor, dark) {
        TerminalPalette(fg, bg, cursor, selection, highlight, if (dark) AnsiDark else AnsiLight)
    }
}

/** How big one cell is, measured from the font rather than guessed. */
data class CellMetrics(val width: Float, val height: Float, val baseline: Float)

/**
 * The grid.
 *
 * ## Why this is a `Canvas` and not a column of `Text`s
 *
 * Eighty by twenty-four is 1920 composables, recomposed on every byte that
 * arrives. A canvas draws runs: consecutive cells sharing a foreground,
 * background and attribute set become one `drawText`, which on ordinary
 * terminal output is a handful of calls per line rather than eighty.
 *
 * ## Why [revision] is a parameter that is never read
 *
 * It is read by Compose. The terminal is a mutable object that Compose cannot
 * observe, so something has to tell the runtime that the pixels are stale, and
 * that something is a `State<Long>` the pump bumps. Removing the parameter
 * would compile, draw once, and then never update again — which is why it is
 * named and documented rather than being an `@Suppress`.
 *
 * ## The lock
 *
 * The whole pass runs inside [Terminal.read]. A frame that is internally
 * inconsistent — half of it from before a scroll and half from after — is
 * worse than a frame that is one read old, and the pump moves whole `Line`
 * objects between the grid and the scrollback while this runs.
 */
@Composable
fun TerminalCanvas(
    terminal: Terminal,
    @Suppress("UNUSED_PARAMETER") revision: Long,
    fontSize: TextUnit,
    /** How many lines above the bottom the viewport is scrolled. */
    scrollBack: Int,
    palette: TerminalPalette,
    selection: Pair<Pos, Pos>?,
    matches: List<Match>,
    onMetrics: (CellMetrics, Int, Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val density = LocalDensity.current
    val paint = remember {
        android.graphics.Paint().apply {
            isAntiAlias = true
            typeface = android.graphics.Typeface.MONOSPACE
        }
    }
    val textSizePx = with(density) { fontSize.toPx() }

    Canvas(modifier) {
        paint.textSize = textSizePx
        paint.isFakeBoldText = false
        paint.textSkewX = 0f
        val metrics = paint.fontMetrics
        val cellWidth = paint.measureText("M")
        val cellHeight = metrics.descent - metrics.ascent + metrics.leading
        if (cellWidth <= 0f || cellHeight <= 0f) return@Canvas
        val cols = (size.width / cellWidth).toInt().coerceAtLeast(1)
        val rows = (size.height / cellHeight).toInt().coerceAtLeast(1)
        onMetrics(CellMetrics(cellWidth, cellHeight, -metrics.ascent), cols, rows)

        drawIntoCanvas { canvas ->
            val native = canvas.nativeCanvas
            terminal.read { screen ->
                // The bottom of the viewport is the bottom of the grid unless
                // the reader has scrolled up into the history.
                val last = screen.totalLines - 1 - scrollBack
                val first = (last - rows + 1).coerceAtLeast(0)
                val cursorLine = screen.totalLines - screen.rows + terminal.cursorRow
                for (row in 0 until rows) {
                    val index = first + row
                    val line = screen.lineAt(index) ?: continue
                    val top = row * cellHeight
                    drawLine(
                        native, paint, line, index, top, cellHeight, cellWidth, -metrics.ascent,
                        palette, selection, matches, screen.cols,
                    )
                    if (index == cursorLine && terminal.cursorVisible) {
                        val x = terminal.cursorCol * cellWidth
                        paint.color = palette.cursor
                        paint.alpha = 160
                        native.drawRect(x, top, x + cellWidth, top + cellHeight, paint)
                        paint.alpha = 255
                    }
                }
            }
        }
    }
}

@Suppress("LongParameterList")
private fun drawLine(
    native: android.graphics.Canvas,
    paint: android.graphics.Paint,
    line: Line,
    index: Int,
    top: Float,
    cellHeight: Float,
    cellWidth: Float,
    baseline: Float,
    palette: TerminalPalette,
    selection: Pair<Pos, Pos>?,
    matches: List<Match>,
    cols: Int,
) {
    var col = 0
    val text = StringBuilder(cols)
    while (col < cols) {
        val fg = line.fg[col]
        val bg = line.bg[col]
        val attrs = line.attrs[col]
        // One run: consecutive cells that share everything a paint carries.
        var end = col
        while (end < cols && line.fg[end] == fg && line.bg[end] == bg && line.attrs[end] == attrs) end++
        text.setLength(0)
        for (i in col until end) {
            val code = line.code[i]
            if (code == com.apexos.remote.core.term.Cell.WIDE_TAIL) continue
            text.appendCodePoint(code)
        }

        val inverse = Attrs.has(attrs, Attrs.INVERSE)
        var ink = palette.resolve(if (inverse) bg else fg, isForeground = !inverse)
        var ground = palette.resolve(if (inverse) fg else bg, isForeground = inverse)
        if (inverse && Colour.isDefault(fg) && Colour.isDefault(bg)) {
            ink = palette.background
            ground = palette.foreground
        }

        val x = col * cellWidth
        val w = (end - col) * cellWidth
        if (ground != palette.background) {
            paint.color = ground
            paint.alpha = 255
            native.drawRect(x, top, x + w, top + cellHeight, paint)
        }
        // Selection and search highlights go over the background and under the
        // glyphs, so text stays readable inside them.
        drawOverlay(native, paint, index, col, end, top, cellHeight, cellWidth, selection, matches, palette)

        if (!Attrs.has(attrs, Attrs.HIDDEN) && text.isNotEmpty()) {
            paint.color = ink
            paint.alpha = if (Attrs.has(attrs, Attrs.DIM)) 150 else 255
            paint.isFakeBoldText = Attrs.has(attrs, Attrs.BOLD)
            paint.textSkewX = if (Attrs.has(attrs, Attrs.ITALIC)) -0.25f else 0f
            paint.isUnderlineText = Attrs.has(attrs, Attrs.UNDERLINE)
            paint.isStrikeThruText = Attrs.has(attrs, Attrs.STRIKE)
            native.drawText(text, 0, text.length, x, top + baseline, paint)
            paint.isFakeBoldText = false
            paint.textSkewX = 0f
            paint.isUnderlineText = false
            paint.isStrikeThruText = false
            paint.alpha = 255
        }
        col = end
    }
}

@Suppress("LongParameterList")
private fun drawOverlay(
    native: android.graphics.Canvas,
    paint: android.graphics.Paint,
    index: Int,
    from: Int,
    to: Int,
    top: Float,
    cellHeight: Float,
    cellWidth: Float,
    selection: Pair<Pos, Pos>?,
    matches: List<Match>,
    palette: TerminalPalette,
) {
    if (selection != null) {
        val (a, b) = if (selection.first <= selection.second) selection else selection.second to selection.first
        if (index in a.line..b.line) {
            val lo = if (index == a.line) a.col else 0
            val hi = if (index == b.line) b.col else Int.MAX_VALUE
            val s = maxOf(from, lo)
            val e = minOf(to - 1, hi)
            if (s <= e) {
                paint.color = palette.selection
                native.drawRect(s * cellWidth, top, (e + 1) * cellWidth, top + cellHeight, paint)
            }
        }
    }
    for (m in matches) {
        if (m.line != index) continue
        val s = maxOf(from, m.col)
        val e = minOf(to - 1, m.col + m.length - 1)
        if (s <= e) {
            paint.color = palette.highlight
            native.drawRect(s * cellWidth, top, (e + 1) * cellWidth, top + cellHeight, paint)
        }
    }
    // Nothing is painted when there is neither a selection nor a match on
    // this line, which is the ordinary case: an overlay drawn transparently
    // over every cell would double the draw calls for no pixels.
}
