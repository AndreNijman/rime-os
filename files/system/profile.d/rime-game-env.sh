# Rime: the game environment for Steam started from the desktop.
#
# shellcheck shell=sh
# No shebang because nothing executes this: /etc/profile.d is SOURCED, by sh,
# bash or zsh, so it is written for sh.
#
# greetd starts every session as `/bin/sh -c '. /etc/profile; … exec <cmd>'`,
# so what this exports is in the compositor's environment and reaches Rime
# Shell, its launcher, every terminal and the Steam (and games) any of them
# start. Gaming Mode applies the same list in /usr/libexec/rime-gamescope-steam.
# The list itself, and why each entry is there, is /usr/libexec/rime-game-env.
#
# Fedora's /etc/bashrc and /etc/zshrc source profile.d again for each new
# interactive shell. The helper prints nothing for an environment it already
# produced, so that costs one short process and changes nothing. Silent: a
# terminal never shows a line from here.
if [ -x "${RIME_GAME_ENV_HELPER:-/usr/libexec/rime-game-env}" ]; then
    while IFS= read -r _rime_game_env; do
        case "$_rime_game_env" in
            [A-Za-z_]*=*) export "${_rime_game_env?}" ;;
        esac
    done <<RIME_GAME_ENV
$("${RIME_GAME_ENV_HELPER:-/usr/libexec/rime-game-env}" 2>/dev/null)
RIME_GAME_ENV
    unset _rime_game_env
fi
