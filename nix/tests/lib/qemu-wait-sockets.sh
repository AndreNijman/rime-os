#!@runtimeShell@
# qemu-system-x86_64 for Rime's NixOS VM tests: wait until every UNIX socket QEMU is told to CONNECT to is
# listening, then exec the real QEMU with the same arguments.
#
# Why (docs/nixos/p1-exit.md, "virtiofsd race"): nixpkgs' run-<node>-vm script (nixos/modules/virtualisation/
# qemu-vm.nix, startVM) starts one virtiofsd per shared directory with `&` and then execs QEMU without waiting for
# their sockets. Under load QEMU can win: it exits 1 with "Failed to connect to '.../shared': No such file or
# directory", and the test driver then blocks for ever reading QEMU's stdout, because the orphaned virtiofsd
# inherited that pipe. A one-line failure becomes a hang until the test's global timeout (3600 s).
#
# The sockets are the `path=` of every `-chardev socket,...` that is not a server (QEMU connects to it): the
# virtiofsd ones, and the driver's own shell socket and swtpm's control socket, which already listen when QEMU is
# started, so waiting on them costs nothing. "Listening" is read from /proc/net/unix (flag __SO_ACCEPTCON), never
# by connecting: virtiofsd serves exactly one connection and exits when it closes.
#
# If a socket is still not listening after RIME_VM_SOCKET_WAIT seconds (default 300), this says which one, kills
# the processes the run script started in the background (its children, now ours: exec keeps the PID), so the
# driver's pipe closes and it reports the failure instead of hanging, and exits 1.
set -u

real=@realQemu@
wait_s=${RIME_VM_SOCKET_WAIT:-300}

sockets=()
prev=
for a in "$@"; do
    if [ "$prev" = -chardev ]; then
        case "$a" in
            socket,*)
                server=0 path=
                IFS=, read -r -a opts <<< "$a"
                for o in "${opts[@]}"; do
                    case "$o" in
                        path=*) path=${o#path=} ;;
                        server|server=on|server=yes) server=1 ;;
                    esac
                done
                [ "$server" = 0 ] && [ -n "$path" ] && sockets+=("$path")
                ;;
        esac
    fi
    prev=$a
done

listening() { # $1 = socket path: is the socket bound to it listening?
    local num ref proto flags rest seen=0
    [ -S "$1" ] || return 1
    [ -r /proc/net/unix ] || return 0 # nothing to read: the socket file existing is all there is
    while read -r num ref proto flags rest; do
        [ "${rest##* }" = "$1" ] || continue
        [ "$flags" = 00010000 ] && return 0
        seen=1
    done < /proc/net/unix
    # Bound under this exact name but not listening yet: wait. Not listed under this name at all (bound through a
    # relative path, or in another network namespace): the socket file existing is all there is to go on.
    [ "$seen" = 0 ]
}

children() { # the PIDs whose parent is this process
    local d ppid
    for d in /proc/[0-9]*; do
        [ -r "$d/stat" ] || continue
        # field 4 of stat, after the parenthesised command name
        ppid=$(@awk@ '{ sub(/^.*\) /, ""); print $2 }' "$d/stat" 2>/dev/null) || continue
        # not this command substitution's own subshell
        [ "$ppid" = "$$" ] && [ "${d#/proc/}" != "$BASHPID" ] && echo "${d#/proc/}"
    done
}

start=$SECONDS
late=()   # the sockets that were not listening yet when QEMU would have started: each one a lost race without this
polls=0
for s in "${sockets[@]}"; do
    listening "$s" || late+=("${s##*/}")
    until listening "$s"; do
        polls=$((polls + 1))
        if [ $((SECONDS - start)) -ge "$wait_s" ]; then
            echo "rime-qemu: $s is not listening after ${wait_s} s; not starting QEMU" >&2
            pids=$(children)
            if [ -n "$pids" ]; then
                echo "rime-qemu: stopping what the run script started: $(echo $pids)" >&2
                # shellcheck disable=SC2086
                kill $pids 2>/dev/null
            fi
            exit 1
        fi
        @sleep@ 0.05
    done
done
if [ "${#late[@]}" -gt 0 ]; then
    echo "rime-qemu: ${#late[@]} of ${#sockets[@]} socket(s) not listening yet (${late[*]}): waited $polls polls, $((SECONDS - start)) s" >&2
fi

exec "$real" "$@"
