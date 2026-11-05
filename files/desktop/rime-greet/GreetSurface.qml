import QtQuick
import QtQuick.Effects

// ─────────────────────────────────────────────────────────────
// GreetSurface — the per-output visual.
//
// A pixel-faithful port of the Brain_Shell Lockscreen surface:
// gradient fallback + blurred/dimmed wallpaper, the big clock block,
// and the pill auth card (username + password). Binds to a shared
// GreetContext (`ctx`) and the inlined palette (`theme`) supplied by
// shell.qml.
// ─────────────────────────────────────────────────────────────

Item {
    id: root
    required property var theme
    required property var ctx

    // Wallpaper is a system path (not shipped with the greeter); a
    // missing file falls back to the gradient below. Resolved by the
    // shared context: the last user's own published wallpaper when
    // there is one, the shipped default otherwise. It used to be this
    // literal default, which is why the login screen never followed the
    // wallpaper you picked in Rime Shell — see GreetContext.
    readonly property url wallpaper: root.ctx.wallpaper

    property real shakeOffset: 0
    property bool capsOn:      false

    // ── Initial-focus state ──────────────────────────────────────
    // True once a human has actually typed in the username field. The
    // last-user name arrives asynchronously, and it must never yank
    // focus away from someone who is already mid-type.
    property bool usernameEdited:     false
    // The returning-user jump to the password field happens once, not on
    // every subsequent change to the username.
    property bool initialFocusApplied: false

    focus: true
    // Any stray keystroke lands in the password field.
    Keys.forwardTo: [passwordInput]

    // Re-shake + clear the field whenever an attempt fails.
    Connections {
        target: root.ctx
        function onFailed() {
            passwordInput.text = ""
            shakeAnim.restart()
        }
    }

    // Keep the username field in sync when the last-user file resolves
    // asynchronously after this surface has already loaded — and move focus
    // to the password field when it does.
    //
    // THE BUG THIS FIXES: booting with a remembered username left the cursor
    // in the USERNAME field, so the first thing a returning user typed went
    // into the wrong box. focusInitial() has always had the right rule, but it
    // runs at Component.onCompleted, and at that instant ctx.username is still
    // "" — the last-user file is read by a Process that resolves a few
    // milliseconds later. So the empty-username branch always won on a fresh
    // boot, and nothing re-evaluated once the name actually arrived.
    Connections {
        target: root.ctx
        function onUsernameChanged() {
            if (!usernameInput.activeFocus) usernameInput.text = root.ctx.username

            if (root.usernameEdited || root.initialFocusApplied) return
            if (root.ctx.username.length === 0) return

            root.initialFocusApplied = true
            // Assign explicitly rather than relying on the guarded sync above:
            // focusInitial() may already have focused this field, in which case
            // that line is skipped and the pill would sit visibly empty while
            // the context knows the name.
            usernameInput.text = root.ctx.username
            passwordInput.forceActiveFocus()
        }
    }

    // ── Background: gradient fallback ──────────────────────────────
    // Always present so an empty / broken wallpaper path can never
    // leave a blank surface.
    Rectangle {
        anchors.fill: parent
        gradient: Gradient {
            orientation: Gradient.Vertical
            GradientStop { position: 0.0; color: Qt.darker(root.theme.background, 1.15) }
            GradientStop {
                position: 1.0
                color: Qt.rgba(root.theme.active.r, root.theme.active.g, root.theme.active.b, 1.0)
            }
        }
    }

    // Wallpaper texture (hidden; fed into the blur effect).
    Image {
        id: wallImg
        anchors.fill: parent
        source:       root.wallpaper
        fillMode:     Image.PreserveAspectCrop
        asynchronous: true
        cache:        true
        visible:      false
    }

    // Blurred + dimmed wallpaper; hidden if the image fails to load,
    // revealing the gradient underneath.
    MultiEffect {
        anchors.fill: parent
        source:      wallImg
        visible:     wallImg.status === Image.Ready
        blurEnabled: true
        blur:        1.0
        blurMax:     48
        brightness: -0.30
        saturation: -0.10
    }

    // Extra scrim for legibility.
    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.35)
    }

    // Clicking anywhere re-focuses the password field.
    MouseArea {
        anchors.fill: parent
        onClicked: passwordInput.forceActiveFocus()
    }

    // ── Clock + date ───────────────────────────────────────────────
    Column {
        id: clockBlock
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom:           card.top
        anchors.bottomMargin:     56
        spacing: 4

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text:           root.ctx.timeText
            color:          root.theme.text
            font.family:    root.theme.fontFamily
            font.pixelSize: 120
            font.bold:      true
        }
        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text:           root.ctx.dateText
            color:          root.theme.subtext
            font.family:    root.theme.fontFamily
            font.pixelSize: 22
        }
    }

    // ── Auth card ──────────────────────────────────────────────────
    Column {
        id: card
        anchors.centerIn: parent
        anchors.verticalCenterOffset: 90
        spacing: 14
        transform: Translate { x: root.shakeOffset }

        // Spark logo, centred above the username. Tinted to the user's accent
        // when one is published; drawn as-is (the edition spark) otherwise.
        // Wrapped in an Item because the Column positions its children and
        // MultiEffect must overlay the Image rather than sit below it.
        Item {
            anchors.horizontalCenter: parent.horizontalCenter
            width:  logo.implicitWidth
            height: logo.implicitHeight
            Image {
                id: logo
                source:            root.theme.logoSource
                sourceSize.height: 84
                fillMode:          Image.PreserveAspectFit
                smooth:            true
                asynchronous:      true
                visible:           !root.theme.tintLogo
            }
            MultiEffect {
                anchors.fill:      logo
                source:            logo
                visible:           root.theme.tintLogo && logo.status === Image.Ready
                colorization:      1.0
                colorizationColor: root.theme.active
            }
        }

        // Username pill (smaller variant of the password pill).
        Rectangle {
            id: userField
            anchors.horizontalCenter: parent.horizontalCenter
            width:  340
            height: 44
            radius: height / 2
            color:  Qt.rgba(root.theme.background.r, root.theme.background.g, root.theme.background.b, 0.55)
            border.width: 2
            border.color: usernameInput.activeFocus ? root.theme.active : root.theme.border
            Behavior on border.color { ColorAnimation { duration: 140 } }

            // Person glyph
            Text {
                anchors.verticalCenter: parent.verticalCenter
                anchors.left:           parent.left
                anchors.leftMargin:     18
                text:  "󰀄"
                color: root.theme.subtext
                font.family:    root.theme.fontFamily
                font.pixelSize: 16
            }

            TextInput {
                id: usernameInput
                objectName:          "greetUsernameField"
                anchors.fill:        parent
                anchors.leftMargin:  44
                anchors.rightMargin: 18
                verticalAlignment:   TextInput.AlignVCenter
                clip:                true
                enabled:             !root.ctx.checking
                color:               root.theme.text
                selectionColor:      root.theme.active
                font.family:         root.theme.fontFamily
                font.pixelSize:      16

                // What a screen reader announces. The placeholder below is a
                // sibling Text that disappears the moment anything is typed, so
                // it is not a label — without this the reader reaches an
                // unnamed edit box on the screen where a machine is unlocked.
                Accessible.role:        Accessible.EditableText
                Accessible.name:        "Username"
                Accessible.description: "The account to log in as"

                // Tab / Shift+Tab walk the whole card so a keyboard-only login
                // works when no username is prefilled. The ring runs
                // username → keyboard layout → password → session picker, and
                // the layout indicator sits BEFORE the password deliberately:
                // it is the thing that has to be checked before a password is
                // typed, not after it has already failed.
                activeFocusOnTab:    true
                KeyNavigation.tab:     layoutPill
                KeyNavigation.backtab: nextArrow
                onTextChanged: root.ctx.username = text
                // textEdited fires ONLY for human edits, not for programmatic
                // assignment — which is exactly the distinction needed here, so
                // the async last-user arrival cannot steal focus mid-type.
                onTextEdited:  root.usernameEdited = true
                onAccepted:    passwordInput.forceActiveFocus()
                Component.onCompleted: text = root.ctx.username

                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    anchors.left:           parent.left
                    visible: usernameInput.text.length === 0
                    text:  "Username"
                    color: root.theme.subtext
                    font.family:    usernameInput.font.family
                    font.pixelSize: usernameInput.font.pixelSize
                }
            }
        }

        // ── Keyboard layout, shown BEFORE the password is typed ──────
        //
        // The defect this exists for is recorded in three places already —
        // sway-greet.conf, labwc-greet/environment and the keymap generator all
        // carry the same note: on a QWERTZ or AZERTY machine a password typed
        // on a US map does not match, and the user cannot log in to the system
        // they just installed. Those three fixed the layout the greeter COMES
        // UP on. None of them tells the user what that layout is, and none lets
        // them change it, so a wrong one is still discovered as an
        // indistinguishable "wrong password".
        //
        // So: always visible, always in the Tab ring (a screen-reader user has
        // no other way to learn which map is live), and switchable in place
        // when the system has more than one configured. With a single layout it
        // is an indicator and nothing else — a control that answers a keypress
        // by doing nothing is worse than one that is plainly not a control.
        Rectangle {
            id: layoutPill
            objectName: "greetLayoutButton"
            anchors.horizontalCenter: parent.horizontalCenter
            width:  340
            height: 30
            radius: height / 2
            color:  Qt.rgba(root.theme.background.r, root.theme.background.g, root.theme.background.b,
                            layoutPill.activeFocus ? 0.75 : 0.40)
            border.width: layoutPill.activeFocus ? 2 : 1
            border.color: layoutPill.activeFocus ? root.theme.active : root.theme.border
            Behavior on border.color { ColorAnimation { duration: 140 } }

            readonly property bool switchable: root.ctx.layouts !== undefined
                                               && root.ctx.layouts.length > 1

            activeFocusOnTab: true
            KeyNavigation.tab:     passwordInput
            KeyNavigation.backtab: usernameInput

            Accessible.role: Accessible.Button
            // The layout name is IN the accessible name rather than only in the
            // description: a reader that announces role-then-name gives the
            // user the live map in the first phrase, which is the whole point.
            Accessible.name: "Keyboard layout: " + root.ctx.layoutName
                             + (layoutPill.switchable ? " (press Space to change)" : "")
            Accessible.description: layoutPill.switchable
                ? "Cycles through the layouts configured on this system"
                : "This system has one keyboard layout configured"
            Accessible.onPressAction: layoutPill.activate()

            function activate() {
                if (!layoutPill.switchable) return
                root.ctx.cycleLayout(1)
            }

            Keys.onPressed: function(event) {
                if (event.key === Qt.Key_Space
                    || event.key === Qt.Key_Return
                    || event.key === Qt.Key_Enter) {
                    layoutPill.activate()
                    event.accepted = true
                }
            }

            Row {
                anchors.centerIn: parent
                spacing: 8

                // Keyboard glyph.
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text:  "󰌌"
                    color: root.theme.subtext
                    font.family:    root.theme.fontFamily
                    font.pixelSize: 14
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text:  root.ctx.layoutName
                    color: layoutPill.activeFocus ? root.theme.active : root.theme.subtext
                    font.family:    root.theme.fontFamily
                    font.pixelSize: 13
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    visible: layoutPill.switchable
                    text:  "⇄"
                    color: root.theme.subtext
                    font.family:    root.theme.fontFamily
                    font.pixelSize: 13
                }
            }

            MouseArea {
                anchors.fill: parent
                enabled:      layoutPill.switchable
                cursorShape:  Qt.PointingHandCursor
                onClicked: {
                    layoutPill.forceActiveFocus()
                    layoutPill.activate()
                }
            }
        }

        // Password pill (geometry verbatim from the Lockscreen).
        Rectangle {
            id: field
            anchors.horizontalCenter: parent.horizontalCenter
            width:  340
            height: 52
            radius: height / 2
            color:  Qt.rgba(root.theme.background.r, root.theme.background.g, root.theme.background.b, 0.55)
            border.width: 2
            border.color: root.ctx.hasError
                              ? root.theme.errorColor
                              : (passwordInput.activeFocus ? root.theme.active : root.theme.border)
            Behavior on border.color { ColorAnimation { duration: 140 } }

            // Lock glyph
            Text {
                anchors.verticalCenter: parent.verticalCenter
                anchors.left:           parent.left
                anchors.leftMargin:     18
                text:  "󰌾"
                color: root.theme.subtext
                font.family:    root.theme.fontFamily
                font.pixelSize: 18
            }

            TextInput {
                id: passwordInput
                objectName:          "greetPasswordField"
                anchors.fill:        parent
                anchors.leftMargin:  46
                anchors.rightMargin: 52
                verticalAlignment:   TextInput.AlignVCenter
                clip:                true
                enabled:             !root.ctx.checking
                focus:               true
                // With the shapes loaded the field paints nothing itself —
                // mask characters, cursor and selection all transparent — and
                // the shapes show its LENGTH. Without them (the component
                // could not be loaded) it is the plain masked field it always
                // was. Either way it stays echoMode Password, mask delay 0.
                color:               shapes.active ? "transparent" : root.theme.text
                selectionColor:      shapes.active ? "transparent" : root.theme.active
                selectedTextColor:   shapes.active ? "transparent" : root.theme.background
                // The caret is not painted in `color`: with the shapes on it
                // would float where the invisible mask text ends. No caret
                // then; the stock one when the field is plain dots.
                cursorDelegate:      shapes.active ? noCaret : null
                font.family:         root.theme.fontFamily
                font.pixelSize:      18
                echoMode:            TextInput.Password
                passwordCharacter:   "●"
                passwordMaskDelay:   0
                activeFocusOnPress:  true

                // passwordEdit is not decoration: it is what tells an assistive
                // technology this field's content must not be echoed.
                //
                // It comes at a price Qt imposes and does not document well.
                // qquickaccessibleattached_p.h:91-93 —
                //
                //     QString name() const {
                //         if (m_state.passwordEdit)
                //             return QString();
                //
                // — so setting it makes `Accessible.name` report empty no matter
                // what is assigned here. `description()` (:111) has no such
                // guard, so the LABEL lives there; the name is left in place
                // against the day Qt stops suppressing it. The choice is
                // between a field a reader will not echo and a field a reader
                // can name, and echoing a typed password aloud is the worse of
                // the two. tests/greet-a11y-test.qml pins this behaviour so the
                // trade is re-examined if Qt ever changes it.
                Accessible.role:         Accessible.EditableText
                Accessible.name:         "Password"
                Accessible.passwordEdit: true
                Accessible.description:  "Password. Press Enter to log in, Escape to clear the field."

                // Tab steps on to the session picker; Shift+Tab steps back to
                // the keyboard-layout indicator.
                activeFocusOnTab:    true
                KeyNavigation.tab:     prevArrow
                KeyNavigation.backtab: layoutPill

                // Mirror the buffer into shared state (used by Greetd).
                //
                // The error clears when the user TYPES, not when the field is
                // emptied: onFailed below clears the field after fail() has set
                // hasError, and a reset on every text change meant the refusal
                // was undone the moment it was shown — the red outline and the
                // shapes' refusal tint never survived their own frame.
                onTextChanged: {
                    root.ctx.password = text
                    if (root.ctx.hasError && text.length > 0) root.ctx.hasError = false
                }

                // Enter submits.
                onAccepted: root.ctx.tryAuth()

                Keys.onPressed: function(event) {
                    if (event.key === Qt.Key_Escape) {
                        text = ""
                        event.accepted = true
                        return
                    }
                    if (event.key === Qt.Key_CapsLock) {
                        // Best-effort toggle; the case heuristic below
                        // corrects it as soon as a letter is typed.
                        root.capsOn = !root.capsOn
                        return
                    }
                    // Alt+Left / Alt+Right cycle the session (keyboard
                    // parity with the on-screen ‹ › picker).
                    if ((event.modifiers & Qt.AltModifier) &&
                        (event.key === Qt.Key_Left || event.key === Qt.Key_Right)) {
                        root.ctx.cycleSession(event.key === Qt.Key_Left ? -1 : 1)
                        event.accepted = true
                        return
                    }
                    // Caps-Lock detection via typed-character case.
                    if (event.text.length === 1) {
                        var c = event.text
                        var isLower = (c >= "a" && c <= "z")
                        var isUpper = (c >= "A" && c <= "Z")
                        var shift   = (event.modifiers & Qt.ShiftModifier) !== 0
                        if (isLower || isUpper)
                            root.capsOn = shift ? isLower : isUpper
                    }
                }

                // Placeholder. With the shapes loaded it waits for them to
                // have actually left, so it never draws over a leaving row.
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    anchors.left:           parent.left
                    visible: (shapes.active ? shapes.item.empty
                                            : passwordInput.text.length === 0)
                             && !root.ctx.checking
                    text:  "Enter password"
                    color: root.theme.subtext
                    font.family:    passwordInput.font.family
                    font.pixelSize: passwordInput.font.pixelSize
                }
            }

            Component { id: noCaret; Item {} }

            // What the field holds, as shapes: Rime Shell's PasswordShapes,
            // handed the field's LENGTH and never its text (see GreetContext's
            // shapesSource). Its motion is the last user's own — speed, scale
            // and Reduce Motion as their session published them.
            Loader {
                id: shapes
                readonly property bool active: status === Loader.Ready && item !== null
                anchors.fill: passwordInput
                source: root.ctx.shapesSource
                onLoaded: {
                    item.length      = Qt.binding(function () { return passwordInput.length })
                    item.accent      = Qt.binding(function () { return root.theme.active })
                    item.text        = Qt.binding(function () { return root.theme.text })
                    item.background  = Qt.binding(function () { return root.theme.background })
                    item.danger      = Qt.binding(function () { return root.theme.errorColor })
                    item.error       = Qt.binding(function () { return root.ctx.hasError })
                    item.busy        = Qt.binding(function () { return root.ctx.checking })
                    item.speed       = Qt.binding(function () { return root.ctx.motionSpeed })
                    item.motionScale = Qt.binding(function () { return root.ctx.motionScale })
                    item.reduced     = Qt.binding(function () { return root.ctx.motionReduced })
                    item.size        = 15
                }
            }

            // Spinner (shown while the auth conversation is in flight).
            Item {
                id: spinner
                anchors.verticalCenter: parent.verticalCenter
                anchors.right:          parent.right
                anchors.rightMargin:    16
                width:  22
                height: 22
                visible: root.ctx.checking

                Rectangle {
                    anchors.fill: parent
                    radius: width / 2
                    color: "transparent"
                    border.width: 3
                    border.color: Qt.rgba(root.theme.active.r, root.theme.active.g, root.theme.active.b, 0.25)
                }
                Rectangle {
                    width: 6; height: 6; radius: 3
                    color: root.theme.active
                    anchors.horizontalCenter: parent.horizontalCenter
                    y: -1
                }
                RotationAnimator on rotation {
                    running: spinner.visible
                    loops:   Animation.Infinite
                    from: 0; to: 360
                    duration: 850
                }
            }
        }

        // Status line — error message, recovery notice, or caps-lock warning.
        //
        // Everything this line says, the surface ALSO says in a way no reader
        // can hear: a shake, a red border, a Nerd Font glyph. AlertMessage is
        // what makes a reader interrupt for it rather than wait to be asked,
        // and the accessible name drops the glyph — a reader given "󰪛" spells
        // out a private-use codepoint or says nothing at all. The recovery
        // notice carries "󰀦" (nf-md-alert) on the same terms, out of the same
        // Material Design range as the three glyphs already on this surface.
        //
        // Three tiers, in this order. The middle one tests the notice for TRUTH
        // and not against "": a ctx that does not carry the property at all
        // yields undefined, `undefined !== ""` is TRUE in JavaScript, and the
        // line would then render undefined and swallow BOTH the caps-lock
        // warning and — through the accessible name — anything a reader was
        // waiting on. tests/greet-a11y-test.qml caught exactly that.
        // Three tiers, in this order. The recovery notice (roadmap P2-018) sits
        // above Caps Lock because it is the only thing on this screen that
        // explains why the session picker moved by itself — a greeter that
        // silently changed the selection would read as the machine having lost
        // the user's preference, which is the failure the notice exists to
        // prevent. It sits below the auth error because an error is about the
        // keystroke the user just made. It goes with the preselection: cycling
        // the picker clears both (GreetContext cycleSession).
        Text {
            id: statusLine
            objectName: "greetStatusLine"
            anchors.horizontalCenter: parent.horizontalCenter
            height:  18
            text: root.ctx.hasError ? root.ctx.errorText
                : (root.ctx.recoveryNotice ? "󰀦  " + root.ctx.recoveryNotice
                : (root.capsOn ? "󰪛  Caps Lock is on" : ""))
            color: root.ctx.hasError ? root.theme.errorColor : root.theme.subtext
            font.family:    root.theme.fontFamily
            font.pixelSize: 14

            Accessible.role: Accessible.AlertMessage
            Accessible.name: root.ctx.hasError ? root.ctx.errorText
                           : (root.ctx.recoveryNotice ? root.ctx.recoveryNotice
                           : (root.capsOn ? "Caps Lock is on" : ""))
        }
    }

    // ── Session picker (subtle, bottom-centre) ────────────────────
    // Matches the Lockscreen typography: JetBrainsMono, subtext colour.
    // Rendered as "‹ Session ›"; arrows cycle, hover picks up the accent.
    Row {
        id: sessionPicker
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom:           parent.bottom
        anchors.bottomMargin:     40
        spacing: 14
        visible: root.ctx.sessions.length > 0

        // The two arrows were Text + MouseArea, which is a control to a mouse
        // and scenery to everything else: not in the Tab ring, no role, no
        // name. The only keyboard route to them was Alt+Left / Alt+Right buried
        // in passwordInput's Keys.onPressed — undiscoverable, and unreachable
        // for anyone who cannot get to the password field in the first place.
        Text {
            id: prevArrow
            objectName: "greetSessionPrev"
            anchors.verticalCenter: parent.verticalCenter
            text:  "‹"
            color: (prevMouse.containsMouse || prevArrow.activeFocus)
                       ? root.theme.active : root.theme.subtext
            font.family:    root.theme.fontFamily
            font.pixelSize: 20

            activeFocusOnTab: true
            KeyNavigation.tab:     nextArrow
            KeyNavigation.backtab: passwordInput

            Accessible.role: Accessible.Button
            Accessible.name: "Previous session"
            Accessible.description: "Chooses which desktop this login starts"
            Accessible.onPressAction: root.ctx.cycleSession(-1)

            Keys.onPressed: function(event) {
                if (event.key === Qt.Key_Space
                    || event.key === Qt.Key_Return
                    || event.key === Qt.Key_Enter) {
                    root.ctx.cycleSession(-1)
                    event.accepted = true
                }
            }

            // A focus ring, because the accent-colour swap alone is the same
            // signal hover uses and a keyboard user cannot tell them apart.
            Rectangle {
                anchors.fill: parent
                anchors.margins: -4
                radius: 4
                color: "transparent"
                border.width: 1
                border.color: root.theme.active
                visible: prevArrow.activeFocus
            }

            MouseArea {
                id: prevMouse
                anchors.fill: parent
                hoverEnabled: true
                cursorShape:  Qt.PointingHandCursor
                onClicked:    root.ctx.cycleSession(-1)
            }
        }
        Text {
            id: sessionLabel
            objectName: "greetSessionName"
            anchors.verticalCenter:  parent.verticalCenter
            horizontalAlignment:     Text.AlignHCenter
            width: Math.max(160, implicitWidth)
            text:  root.ctx.sessionName
            color: root.theme.subtext
            font.family:    root.theme.fontFamily
            font.pixelSize: 15

            // Named rather than left to the default text reading, so the
            // announcement says WHAT the value is. Two buttons that change an
            // unnamed word are two buttons that change nothing a reader can
            // report.
            Accessible.role: Accessible.StaticText
            Accessible.name: "Session: " + root.ctx.sessionName
        }
        Text {
            id: nextArrow
            objectName: "greetSessionNext"
            anchors.verticalCenter: parent.verticalCenter
            text:  "›"
            color: (nextMouse.containsMouse || nextArrow.activeFocus)
                       ? root.theme.active : root.theme.subtext
            font.family:    root.theme.fontFamily
            font.pixelSize: 20

            activeFocusOnTab: true
            KeyNavigation.tab:     usernameInput
            KeyNavigation.backtab: prevArrow

            Accessible.role: Accessible.Button
            Accessible.name: "Next session"
            Accessible.description: "Chooses which desktop this login starts"
            Accessible.onPressAction: root.ctx.cycleSession(1)

            Keys.onPressed: function(event) {
                if (event.key === Qt.Key_Space
                    || event.key === Qt.Key_Return
                    || event.key === Qt.Key_Enter) {
                    root.ctx.cycleSession(1)
                    event.accepted = true
                }
            }

            Rectangle {
                anchors.fill: parent
                anchors.margins: -4
                radius: 4
                color: "transparent"
                border.width: 1
                border.color: root.theme.active
                visible: nextArrow.activeFocus
            }

            MouseArea {
                id: nextMouse
                anchors.fill: parent
                hoverEnabled: true
                cursorShape:  Qt.PointingHandCursor
                onClicked:    root.ctx.cycleSession(1)
            }
        }
    }

    // ── Error shake (values verbatim from the Lockscreen) ──────────
    SequentialAnimation {
        id: shakeAnim
        NumberAnimation { target: root; property: "shakeOffset"; from: 0; to:  14; duration: 45 }
        NumberAnimation { target: root; property: "shakeOffset"; to: -14; duration: 45 }
        NumberAnimation { target: root; property: "shakeOffset"; to:  10; duration: 45 }
        NumberAnimation { target: root; property: "shakeOffset"; to: -10; duration: 45 }
        NumberAnimation { target: root; property: "shakeOffset"; to:   6; duration: 45 }
        NumberAnimation { target: root; property: "shakeOffset"; to:   0; duration: 45 }
    }

    // Grab keyboard focus as soon as the surface appears. Start on the
    // username field when nothing is prefilled (fresh boot / no last-user)
    // so a username can be typed without a mouse; otherwise go straight to
    // the password field, matching the returning-user fast path.
    // Note this is only half the story: on a fresh boot ctx.username is still
    // empty here and the Connections block above applies the returning-user
    // jump once last-user resolves. This branch covers the case where the name
    // is already known by the time a surface appears (a re-show, or a second
    // output mapping late).
    function focusInitial() {
        if (root.ctx.username.length === 0) {
            usernameInput.forceActiveFocus()
        } else {
            root.initialFocusApplied = true
            passwordInput.forceActiveFocus()
        }
    }
    Component.onCompleted: focusInitial()
    onVisibleChanged: if (visible) focusInitial()
}
