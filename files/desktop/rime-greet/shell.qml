import QtQuick
import Quickshell
import Quickshell.Wayland

// ─────────────────────────────────────────────────────────────
// rime-greet — standalone greetd greeter for Rime OS.
//
// A visual + interaction port of Brain_Shell's Lockscreen, made
// fully self-contained: none of the shell singletons the Lockscreen
// leaned on (Theme, WallpaperService, LockState) exist at greeter
// stage, so the palette, wallpaper, clock and all state are inlined
// here instead of imported.
//
// Runtime model: this runs under `sway` (a wlroots kiosk compositor)
// launched by greetd — NOT a session lock. sway implements
// wlr-layer-shell natively (the same path gtkgreet uses), so the
// fullscreen surface is a PanelWindow anchored to all four edges with
// Exclusive keyboard focus, one per output via Variants over
// Quickshell.screens. (Host changed cage → sway in M1: cage 0.2.0 did
// not serve layer-shell to quickshell 0.3.0 — see M0 spike A / README.)
//
// Auth backend is Quickshell.Services.Greetd (see GreetContext.qml),
// which replaces the Lockscreen's PamContext.
// ─────────────────────────────────────────────────────────────

ShellRoot {
    id: shell

    // ── Inlined palette (ThemeGreet) ──────────────────────────────
    // Values lifted verbatim from Brain_Shell's colours.json fallbacks
    // (src/theme/ColorLoader.qml) — the same defaults the Lockscreen
    // paints with before any live theme is loaded. `active` and the
    // spark logo are edition-derived so gaming / daily / dev each get
    // their own accent.
    QtObject {
        id: greetTheme
        readonly property string fontFamily: "JetBrainsMono Nerd Font"
        readonly property color background: "#1a282a"
        readonly property color text:       "#cdd6f4"
        readonly property color subtext:    "#94e2d5"
        readonly property color border:     "#ffffff"
        readonly property color errorColor: "#ff5c5c"

        // Edition accent: rime (the one published image) and the legacy
        // daily → chartreuse, legacy gaming → gold, anything else (dev box
        // / VARIANT_ID unset) → the Brain_Shell blue so the greeter is
        // pixel-identical to the Lockscreen off-target.
        // The user's own accent when Rime Shell has published one, so the
        // login screen matches the desktop; the edition colour otherwise.
        readonly property color active:
            greetCtx.accent !== ""        ? greetCtx.accent
          : greetCtx.edition === "gaming" ? "#fde047"
          : greetCtx.edition === "rime"   ? "#d9f99d"
          : greetCtx.edition === "daily"  ? "#d9f99d"
          : "#a6d0f7"

        // Assets ship alongside this file, so Qt.resolvedUrl works both
        // in-tree (dev) and installed at /usr/share/rime-greet/.
        // With a published accent the WHITE spark is used and tinted to it in
        // GreetSurface — white keeps full luminance, so the tint is the accent
        // itself rather than a darkened version of it.
        readonly property bool tintLogo: greetCtx.accent !== ""
        readonly property url logoSource:
            greetCtx.accent !== ""        ? Qt.resolvedUrl("assets/spark-white.png")
          : greetCtx.edition === "gaming" ? Qt.resolvedUrl("assets/spark-gold.png")
          : greetCtx.edition === "rime"   ? Qt.resolvedUrl("assets/spark-chartreuse.png")
          : greetCtx.edition === "daily"  ? Qt.resolvedUrl("assets/spark-chartreuse.png")
          : Qt.resolvedUrl("assets/spark-white.png")
    }

    // ── Shared greeter state + auth backend ───────────────────────
    GreetContext { id: greetCtx }

    // ── One fullscreen layer-shell surface per output ─────────────
    Variants {
        model: Quickshell.screens
        PanelWindow {
            id: win
            required property var modelData
            screen: modelData

            // Opaque base so there is never a transparent flash before
            // the wallpaper / gradient paints.
            color: "black"

            WlrLayershell.layer:        WlrLayer.Overlay
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
            WlrLayershell.namespace:     "rime-greet"

            // All four edges anchored → fills the output.
            anchors {
                top:    true
                bottom: true
                left:   true
                right:  true
            }

            GreetSurface {
                anchors.fill: parent
                theme: greetTheme
                ctx:   greetCtx
            }
        }
    }
}
