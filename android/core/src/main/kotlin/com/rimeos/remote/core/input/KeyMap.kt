package com.rimeos.remote.core.input

/**
 * Keys as the computer names them: Linux evdev codes
 * (`linux/input-event-codes.h`), from Android key codes (for a keyboard
 * plugged into or paired with the phone) and from characters (for a key
 * typed while a modifier on the key strip is held, where the computer needs
 * the key, not the text).
 *
 * Android's numbers are written out rather than read from
 * `android.view.KeyEvent` so this stays plain JVM code with tests. Keys the
 * phone keeps for itself (Back, Home, volume, media, power) map to nothing,
 * and so does every key [Input.keyAllowed] forbids.
 */
object KeyMap {
    const val ESC: Int = 1
    const val BACKSPACE: Int = 14
    const val TAB: Int = 15
    const val ENTER: Int = 28
    const val LEFT_CTRL: Int = 29
    const val LEFT_SHIFT: Int = 42
    const val LEFT_ALT: Int = 56
    const val SPACE: Int = 57
    const val CAPS_LOCK: Int = 58
    const val RIGHT_ALT: Int = 100
    const val HOME: Int = 102
    const val UP: Int = 103
    const val PAGE_UP: Int = 104
    const val LEFT: Int = 105
    const val RIGHT: Int = 106
    const val END: Int = 107
    const val DOWN: Int = 108
    const val PAGE_DOWN: Int = 109
    const val INSERT: Int = 110
    const val DELETE: Int = 111
    const val LEFT_META: Int = 125

    /** F1 to F12. */
    val F: List<Int> = listOf(59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88)

    /** The evdev key a modifier is pressed with. */
    fun of(mod: Mod): Int = when (mod) {
        Mod.CTRL -> LEFT_CTRL
        Mod.SHIFT -> LEFT_SHIFT
        Mod.ALT -> LEFT_ALT
        Mod.SUPER -> LEFT_META
        Mod.ALTGR -> RIGHT_ALT
    }

    private val ANDROID: Map<Int, Int> = buildMap {
        // Digits 0..9: KEYCODE_0 = 7.
        put(7, 11)
        for (d in 1..9) put(7 + d, 1 + d)
        // Letters: KEYCODE_A = 29.
        val letters = intArrayOf(
            30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50,
            49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
        )
        for ((i, code) in letters.withIndex()) put(29 + i, code)
        put(19, UP) // DPAD_UP
        put(20, DOWN)
        put(21, LEFT)
        put(22, RIGHT)
        put(55, 51) // COMMA
        put(56, 52) // PERIOD
        put(57, LEFT_ALT)
        put(58, RIGHT_ALT)
        put(59, LEFT_SHIFT)
        put(60, 54) // SHIFT_RIGHT
        put(61, TAB)
        put(62, SPACE)
        put(66, ENTER)
        put(67, BACKSPACE) // DEL
        put(68, 41) // GRAVE
        put(69, 12) // MINUS
        put(70, 13) // EQUALS
        put(71, 26) // LEFT_BRACKET
        put(72, 27) // RIGHT_BRACKET
        put(73, 43) // BACKSLASH
        put(74, 39) // SEMICOLON
        put(75, 40) // APOSTROPHE
        put(76, 53) // SLASH
        put(92, PAGE_UP)
        put(93, PAGE_DOWN)
        put(111, ESC)
        put(112, DELETE) // FORWARD_DEL
        put(113, LEFT_CTRL)
        put(114, 97) // CTRL_RIGHT
        put(115, CAPS_LOCK)
        put(116, 70) // SCROLL_LOCK
        put(117, LEFT_META)
        put(118, 126) // META_RIGHT
        put(120, 99) // SYSRQ
        put(121, 119) // BREAK -> PAUSE
        put(122, HOME) // MOVE_HOME
        put(123, END) // MOVE_END
        put(124, INSERT)
        for ((i, code) in F.withIndex()) put(131 + i, code) // F1..F12
        put(143, 69) // NUM_LOCK
        // Numpad 0..9: KEYCODE_NUMPAD_0 = 144.
        val pad = intArrayOf(82, 79, 80, 81, 75, 76, 77, 71, 72, 73)
        for ((i, code) in pad.withIndex()) put(144 + i, code)
        put(154, 98) // NUMPAD_DIVIDE
        put(155, 55) // NUMPAD_MULTIPLY
        put(156, 74) // NUMPAD_SUBTRACT
        put(157, 78) // NUMPAD_ADD
        put(158, 83) // NUMPAD_DOT
        put(159, 121) // NUMPAD_COMMA
        put(160, 96) // NUMPAD_ENTER
        put(161, 117) // NUMPAD_EQUALS
        put(162, 179) // NUMPAD_LEFT_PAREN
        put(163, 180) // NUMPAD_RIGHT_PAREN
    }

    /** The evdev code for an Android key code, or null for a key the phone keeps or the computer may not get. */
    fun android(keyCode: Int): Int? = ANDROID[keyCode]?.takeIf(Input::keyAllowed)

    /** A key and whether Shift is needed to type a character on a US layout. */
    data class Typed(val code: Int, val shift: Boolean)

    private val UNSHIFTED = "1234567890-=qwertyuiop[]asdfghjkl;'`\\zxcvbnm,./ "
    private val SHIFTED = "!@#$%^&*()_+QWERTYUIOP{}ASDFGHJKL:\"~|ZXCVBNM<>? "
    private val CODES = intArrayOf(
        2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13,
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
        30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41,
        43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53,
        SPACE,
    )

    /**
     * The key that types [c] on a US layout, for a shortcut such as Ctrl+C
     * made from the key strip's Ctrl and a typed "c". Null for a character
     * with no key there; that is sent as text instead.
     */
    fun char(c: Char): Typed? {
        val plain = UNSHIFTED.indexOf(c)
        if (plain >= 0) return Typed(CODES[plain], false)
        val shifted = SHIFTED.indexOf(c)
        if (shifted >= 0) return Typed(CODES[shifted], true)
        return null
    }
}
