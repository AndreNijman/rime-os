# Rime OS — fastfetch greeting for interactive terminals (rime-logs 33, 14)
#
# shellcheck shell=bash
# No shebang because this is sourced, by /etc/bashrc for bash and by the seeded
# ~/.zshrc for zsh. bash is declared because it is the only one of the two that
# the linter can read, and the two dialects agree on everything in this file:
# there is no $ZSH_VERSION branch here, unlike agent.sh next door.
#
# ONE file, sourced by BOTH shells: /etc/bashrc for bash and the seeded ~/.zshrc
# for zsh. It used to be an inline block appended only to /etc/bashrc — but zsh is
# the DEFAULT LOGIN SHELL on Rime OS (see /etc/default/useradd), so in practice no
# user ever saw the greeting in their own terminal. It only ever appeared if you
# explicitly started bash. Keeping the logic in one place is also why the colour
# handling below cannot drift between the two shells.
#
# Written in POSIX sh on purpose: it is sourced by bash and zsh, and must not
# depend on the syntax of either — so no bash-only test brackets, no arrays, no
# `local`. The build asserts this file is free of bash-only test syntax, which is
# also why that syntax is described here in words rather than written out.
#
# Opt out with RIME_NO_GREETING=1 — set it in ~/.zshrc.local or ~/.bashrc.d, which
# are both read BEFORE this runs.

_rime_greet=1

# Once per shell session. RIME_GREETED is exported, so a nested shell (or a tmux
# pane, or a subshell) does not greet you a second time.
[ -n "${RIME_GREETED:-}" ] && _rime_greet=0

# Explicit user opt-out.
[ -n "${RIME_NO_GREETING:-}" ] && _rime_greet=0
# …under the name it had before the rename, set in a file this image never rewrites.
[ -n "${APEX_NO_GREETING:-}" ] && _rime_greet=0  # rime-rename: keep — opt-out users already set

# Interactive only. Printing a 20-line logo into a non-interactive shell corrupts
# scp/sftp/rsync sessions and anything parsing command output.
case $- in
    *i*) ;;
    *)   _rime_greet=0 ;;
esac

# stdout must be a terminal. An interactive shell can still have its output
# redirected, and a captured greeting is just garbage in a file.
[ -t 1 ] || _rime_greet=0

# Dumb or unset TERM means something like Emacs tramp or a bare pipe on the far
# end of a login: no cursor control, so the logo renders as line noise.
case "${TERM:-}" in
    ""|dumb) _rime_greet=0 ;;
esac

command -v fastfetch >/dev/null 2>&1 || _rime_greet=0

if [ "${_rime_greet}" = 1 ]; then
    RIME_GREETED=1
    export RIME_GREETED
    # The logo is drawn in the foreground colour, so it has to invert with the
    # desktop theme or it disappears against the background on a light scheme.
    _rime_logo_color=white
    case "$(gsettings get org.gnome.desktop.interface color-scheme 2>/dev/null)" in
        *light*) _rime_logo_color=black ;;
    esac
    # Accent colour, so the greeting tracks the desktop palette instead of the
    # static brand chartreuse it used to be built around. Rime Shell's matugen
    # pipeline rewrites this file on every wallpaper change, already formatted as
    # an SGR parameter string (38;2;R;G;B) — the greeting is POSIX sh and has no
    # business doing hex arithmetic to use a colour.
    _rime_accent_file="${XDG_CACHE_HOME:-$HOME/.cache}/rime-shell/accent-ansi"
    _rime_accent=""
    if [ -r "${_rime_accent_file}" ]; then
        read -r _rime_accent < "${_rime_accent_file}" || _rime_accent=""
    fi
    # Only digits and semicolons reach a terminal escape. A truncated or
    # part-written file (matugen rewrites it under the running shell) would
    # otherwise be pasted straight into the output.
    case "${_rime_accent}" in
        ""|*[!0-9\;]*) _rime_accent="" ;;
    esac

    # The accent also replaces the "low" colour of percentages (memory, disk):
    # fastfetch's own green there was the last green left in the greeting.
    # Yellow and red stay, because those are warnings and mean something.
    #
    # No accent yet — a first login before any wallpaper has been picked — means
    # no --color at all, leaving the config's own default rather than forcing
    # some colour that may not suit the scheme. Spelled out as four branches
    # because this file is SOURCED: building the argument list with `set --`
    # would overwrite the calling shell's positional parameters.
    if [ -r /etc/fastfetch/config.jsonc ] && [ -n "${_rime_accent}" ]; then
        fastfetch --config /etc/fastfetch/config.jsonc --logo-color-1 "${_rime_logo_color}" --color "${_rime_accent}" --percent-color-green "${_rime_accent}"
    elif [ -r /etc/fastfetch/config.jsonc ]; then
        fastfetch --config /etc/fastfetch/config.jsonc --logo-color-1 "${_rime_logo_color}"
    elif [ -n "${_rime_accent}" ]; then
        # Config missing (a partial image, or a user deleted it): still greet
        # rather than silently printing nothing at all.
        fastfetch --logo-color-1 "${_rime_logo_color}" --color "${_rime_accent}" --percent-color-green "${_rime_accent}"
    else
        fastfetch --logo-color-1 "${_rime_logo_color}"
    fi
    unset _rime_logo_color _rime_accent _rime_accent_file
fi

unset _rime_greet
