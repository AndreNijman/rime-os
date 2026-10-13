package com.apexos.remote.ui.terminal

import android.view.KeyEvent as AndroidKeyEvent
import com.apexos.remote.core.term.Key
import com.apexos.remote.core.term.Mods

/**
 * Android key codes to the keys a terminal knows.
 *
 * ## Why this is a table and not a `when` inside a composable
 *
 * Because it is testable this way and not that way. `:app` has six JVM unit
 * tests and no device; a mapping written inside `onPreviewKeyEvent` could only
 * be checked by pressing keys on a phone, which is exactly the verification
 * this project cannot do. `android.view.KeyEvent`'s constants are plain `int`s
 * available to a JVM test, so the table is.
 *
 * Everything not here falls through to the text path, which is right: a letter
 * key is text, and a terminal wants the character the layout produced rather
 * than the key that produced it. A German keyboard's `z` is `KEYCODE_Y`.
 */
object Keymap {
    private val KEYS: Map<Int, Key> = mapOf(
        AndroidKeyEvent.KEYCODE_ENTER to Key.ENTER,
        AndroidKeyEvent.KEYCODE_NUMPAD_ENTER to Key.ENTER,
        AndroidKeyEvent.KEYCODE_TAB to Key.TAB,
        AndroidKeyEvent.KEYCODE_DEL to Key.BACKSPACE,
        AndroidKeyEvent.KEYCODE_FORWARD_DEL to Key.DELETE,
        AndroidKeyEvent.KEYCODE_ESCAPE to Key.ESCAPE,
        AndroidKeyEvent.KEYCODE_INSERT to Key.INSERT,
        AndroidKeyEvent.KEYCODE_DPAD_UP to Key.UP,
        AndroidKeyEvent.KEYCODE_DPAD_DOWN to Key.DOWN,
        AndroidKeyEvent.KEYCODE_DPAD_LEFT to Key.LEFT,
        AndroidKeyEvent.KEYCODE_DPAD_RIGHT to Key.RIGHT,
        AndroidKeyEvent.KEYCODE_MOVE_HOME to Key.HOME,
        AndroidKeyEvent.KEYCODE_MOVE_END to Key.END,
        AndroidKeyEvent.KEYCODE_PAGE_UP to Key.PAGE_UP,
        AndroidKeyEvent.KEYCODE_PAGE_DOWN to Key.PAGE_DOWN,
        AndroidKeyEvent.KEYCODE_F1 to Key.F1,
        AndroidKeyEvent.KEYCODE_F2 to Key.F2,
        AndroidKeyEvent.KEYCODE_F3 to Key.F3,
        AndroidKeyEvent.KEYCODE_F4 to Key.F4,
        AndroidKeyEvent.KEYCODE_F5 to Key.F5,
        AndroidKeyEvent.KEYCODE_F6 to Key.F6,
        AndroidKeyEvent.KEYCODE_F7 to Key.F7,
        AndroidKeyEvent.KEYCODE_F8 to Key.F8,
        AndroidKeyEvent.KEYCODE_F9 to Key.F9,
        AndroidKeyEvent.KEYCODE_F10 to Key.F10,
        AndroidKeyEvent.KEYCODE_F11 to Key.F11,
        AndroidKeyEvent.KEYCODE_F12 to Key.F12,
    )

    /** The terminal key a code means, or `null` when it is text or nothing. */
    fun keyOf(code: Int): Key? = KEYS[code]

    /**
     * The modifiers a key event carried.
     *
     * `META_CTRL_ON` and friends and **not** the `isCtrlPressed` helpers,
     * because the mask is what a JVM test can build. The alt key is the meta
     * key here on purpose: a phone's `Alt` and a terminal's `Meta` are the same
     * thing to everything downstream, and a terminal has no encoding for a
     * third modifier anyway.
     */
    fun modsOf(metaState: Int): Mods = Mods(
        ctrl = metaState and AndroidKeyEvent.META_CTRL_ON != 0,
        alt = metaState and AndroidKeyEvent.META_ALT_ON != 0,
        shift = metaState and AndroidKeyEvent.META_SHIFT_ON != 0,
    )

    /**
     * Whether this key press should be swallowed rather than sent.
     *
     * The back key is the one that matters: a terminal that grabbed it would
     * be a screen the user cannot leave, and a terminal that sent it would
     * send nothing useful. Volume keys are the system's.
     */
    fun isSystemKey(code: Int): Boolean = when (code) {
        AndroidKeyEvent.KEYCODE_BACK,
        AndroidKeyEvent.KEYCODE_HOME,
        AndroidKeyEvent.KEYCODE_APP_SWITCH,
        AndroidKeyEvent.KEYCODE_VOLUME_UP,
        AndroidKeyEvent.KEYCODE_VOLUME_DOWN,
        AndroidKeyEvent.KEYCODE_VOLUME_MUTE,
        AndroidKeyEvent.KEYCODE_POWER,
        -> true
        else -> false
    }

    /**
     * A modifier key on its own, which must not be sent as anything.
     *
     * Without this, holding Ctrl on a hardware keyboard sends a stray byte
     * before the key it was meant to modify arrives.
     */
    fun isModifierKey(code: Int): Boolean = when (code) {
        AndroidKeyEvent.KEYCODE_CTRL_LEFT, AndroidKeyEvent.KEYCODE_CTRL_RIGHT,
        AndroidKeyEvent.KEYCODE_ALT_LEFT, AndroidKeyEvent.KEYCODE_ALT_RIGHT,
        AndroidKeyEvent.KEYCODE_SHIFT_LEFT, AndroidKeyEvent.KEYCODE_SHIFT_RIGHT,
        AndroidKeyEvent.KEYCODE_META_LEFT, AndroidKeyEvent.KEYCODE_META_RIGHT,
        AndroidKeyEvent.KEYCODE_CAPS_LOCK, AndroidKeyEvent.KEYCODE_FUNCTION,
        -> true
        else -> false
    }
}
