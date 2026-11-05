# dev-vm: a persistent Rime NixOS VM for iterating on the desktop, the Shell and the daemons (docs/nixos/dev-vm.md).
# Runs inside the privileged rime-nix container (the host has no Nix and no QEMU). nix/dev/default.nix wraps this
# file with writeShellApplication (errexit, nounset, pipefail; shellcheck at build time) and puts these on PATH:
# coreutils, util-linux (setsid), openssh, python3 (nix/dev/qmp.py), socat, gnugrep, gnused. `nix` is the
# container's own.
#
# Nothing here activates anything in the container: the VM's kernel boots the system's own init, so activation
# happens in the guest only. Processes are stopped by the PID QEMU wrote, after checking that PID is this VM's QEMU,
# never by a name pattern.

DEVVM_QMP=${DEVVM_QMP:?set by nix/dev/default.nix}
DEVVM_LAUNCH=${DEVVM_LAUNCH:?set by nix/dev/default.nix}
ROOT=${RIME_DEVVM_ROOT:-/work/devvm}
NAME=${RIME_DEVVM_NAME:-rime}
FLAKE=${RIME_DEVVM_FLAKE:-$PWD}
MIN_AVAIL_MB=${RIME_DEVVM_MIN_AVAIL_MB:-16384}
MAX_RUNNING=${RIME_DEVVM_MAX:-2}
STOP_WAIT=${RIME_DEVVM_STOP_WAIT:-90}
SYSTEM=x86_64-linux
DESKTOPS="hyprland gnome kde cosmic"

die() { echo "dev-vm: $*" >&2; exit 1; }
usage_error() { echo "dev-vm: $*" >&2; echo "dev-vm help: the commands and options" >&2; exit 2; }
say() { echo "dev-vm: $*"; }
now() { date +%s.%N; }
# seconds since a date +%s.%N stamp, one decimal
since() { python3 -c 'import sys; print("%.1f" % (float(sys.argv[2]) - float(sys.argv[1])))' "$1" "$(now)"; }

usage() {
  cat <<'EOF'
dev-vm: a persistent Rime NixOS VM (docs/nixos/dev-vm.md). Run in the rime-nix container from a rime-os checkout,
or through nix/dev/dev-vm on the host.

  dev-vm start [build options] [--greeter] [--session ID] [--display none|spice|vnc] [--cpus N] [--no-wait]
                          build the system if it is not built, boot it, wait for the login prompt, ssh and the
                          desktop (the Shell's IPC on Hyprland), printing each time from QEMU's start
  dev-vm rebuild [build options] [start options]
                          rebuild the system from the checkout and restart the VM on the same disk (timed)
  dev-vm build [build options]   build only; a running VM keeps running the system it booted
  dev-vm stop             ACPI power off; QEMU is quit after 90 s (RIME_DEVVM_STOP_WAIT) without a power-off
  dev-vm status           running or not, ports, desktop, system, disk
  dev-vm list             every dev VM under the state root
  dev-vm wait [login|ssh|desktop] [--timeout S]
  dev-vm ssh [--root] [command...]   ssh as the TEST user (rime), or root, with the VM's own key
  dev-vm screenshot FILE  the VM's screen as PNG (QMP screendump; works headless)
  dev-vm type TEXT        type TEXT on the VM's keyboard (\n is Return), e.g. into rime-greet
  dev-vm console          the serial console (Ctrl-] leaves)
  dev-vm log [N]          the last N lines of the serial log (default 60)
  dev-vm reset --yes      delete the VM's persistent disk (the VM must be stopped)

Build options:
  --desktop hyprland|gnome|kde|cosmic   the system variant (default: the VM's last, else hyprland)
  --shell DIR             build with rime-shell from DIR (--override-input rime-shell path:DIR); kept for
                          later rebuilds; --shell locked goes back to flake.lock's rime-shell
Global options (before or after the command):
  --name NAME             which dev VM (default rime); state in $RIME_DEVVM_ROOT/NAME (default /work/devvm)
  --flake DIR             the rime-os checkout to build (default: the current directory)

TEST credentials only: user rime, password rime; ssh with the key generated in the state directory.
EOF
}

# ── options ─────────────────────────────────────────────────────────────────────────────────────────────────────
CMD=""
ARGS=()
OPT_DESKTOP=""
OPT_SHELL=""
OPT_SESSION=""
OPT_GREETER=0
OPT_DISPLAY=""
OPT_CPUS=""
OPT_WAIT=1
OPT_ROOT=0
OPT_YES=0
OPT_TIMEOUT=""

need_val() { [ "$#" -ge 2 ] && [ -n "$2" ] || usage_error "$1 needs a value"; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --name) need_val "$@"; NAME=$2; shift 2 ;;
    --flake) need_val "$@"; FLAKE=$2; shift 2 ;;
    --desktop) need_val "$@"; OPT_DESKTOP=$2; shift 2 ;;
    --shell) need_val "$@"; OPT_SHELL=$2; shift 2 ;;
    --session) need_val "$@"; OPT_SESSION=$2; shift 2 ;;
    --greeter) OPT_GREETER=1; shift ;;
    --display) need_val "$@"; OPT_DISPLAY=$2; shift 2 ;;
    --cpus) need_val "$@"; OPT_CPUS=$2; shift 2 ;;
    --timeout) need_val "$@"; OPT_TIMEOUT=$2; shift 2 ;;
    --no-wait) OPT_WAIT=0; shift ;;
    --root) OPT_ROOT=1; shift ;;
    --yes) OPT_YES=1; shift ;;
    -h|--help) CMD=help; shift ;;
    --) shift; ARGS+=("$@"); break ;;
    *)
      if [ -z "$CMD" ]; then CMD=$1; shift
      elif [ "$CMD" = ssh ]; then ARGS+=("$@"); break   # everything after `ssh` is the remote command
      else ARGS+=("$1"); shift; fi ;;
  esac
done
[ -n "$CMD" ] || CMD=help

# A state directory name: no path separators, no dot-dot, nothing a shell or QEMU option would split on.
valid_name() { [[ "$1" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]]; }
valid_name "$NAME" || usage_error "--name '$NAME': use 1-32 of a-z 0-9 - (starting with a letter or digit)"
if [ -n "$OPT_DESKTOP" ]; then
  case " $DESKTOPS " in *" $OPT_DESKTOP "*) ;; *) usage_error "--desktop '$OPT_DESKTOP': one of $DESKTOPS" ;; esac
fi
case "$OPT_DISPLAY" in ""|none|spice|vnc) ;; *) usage_error "--display '$OPT_DISPLAY': none, spice or vnc" ;; esac
[ -z "$OPT_CPUS" ] || [[ "$OPT_CPUS" =~ ^[1-9][0-9]?$ ]] || usage_error "--cpus '$OPT_CPUS': a number"
[ -z "$OPT_TIMEOUT" ] || [[ "$OPT_TIMEOUT" =~ ^[1-9][0-9]*$ ]] || usage_error "--timeout '$OPT_TIMEOUT': seconds"
[ -z "$OPT_SESSION" ] || [[ "$OPT_SESSION" =~ ^[a-z0-9][a-z0-9_.-]*$ ]] || usage_error "--session '$OPT_SESSION': a wayland-sessions id"

DIR=$ROOT/$NAME
SETTINGS=$DIR/settings
PIDFILE=$DIR/qemu.pid
QMP=$DIR/qmp.sock
SERIAL=$DIR/serial.log
KEY=$DIR/id_ed25519
DISK=$DIR/disk.qcow2
TIMINGS=$DIR/timings.log

# ── settings (key=value lines this script wrote; read, never sourced) ───────────────────────────────────────────
S_desktop=hyprland S_shell="" S_ssh_port="" S_display=none S_display_port="" S_session="" S_greeter=0 S_cpus=""
load_settings() {
  [ -f "$SETTINGS" ] || return 0
  local k v
  while IFS='=' read -r k v; do
    case "$k" in
      desktop) S_desktop=$v ;; shell) S_shell=$v ;; ssh_port) S_ssh_port=$v ;; display) S_display=$v ;;
      display_port) S_display_port=$v ;; session) S_session=$v ;; greeter) S_greeter=$v ;; cpus) S_cpus=$v ;;
    esac
  done < "$SETTINGS"
}
save_settings() {
  local t=$SETTINGS.new
  {
    echo "desktop=$S_desktop"; echo "shell=$S_shell"; echo "ssh_port=$S_ssh_port"; echo "display=$S_display"
    echo "display_port=$S_display_port"; echo "session=$S_session"; echo "greeter=$S_greeter"; echo "cpus=$S_cpus"
  } > "$t"
  mv -f "$t" "$SETTINGS"
}
apply_build_options() {
  [ -z "$OPT_DESKTOP" ] || S_desktop=$OPT_DESKTOP
  case "$OPT_SHELL" in
    "") ;;
    locked) S_shell="" ;;
    *) [ -d "$OPT_SHELL" ] || die "--shell $OPT_SHELL: not a directory"; S_shell=$(cd "$OPT_SHELL" && pwd) ;;
  esac
}
apply_start_options() {
  [ -z "$OPT_DISPLAY" ] || S_display=$OPT_DISPLAY
  [ -z "$OPT_SESSION" ] || S_session=$OPT_SESSION
  [ -z "$OPT_CPUS" ] || S_cpus=$OPT_CPUS
  S_greeter=$OPT_GREETER
}
load_settings

# ── is it running? ──────────────────────────────────────────────────────────────────────────────────────────────
# The PID in qemu.pid counts only while /proc/PID/cmdline is QEMU AND names this VM's own QMP socket: a stale file
# whose PID now belongs to anything else (another VM, an agent's shell) is never signalled.
vm_pid() {
  local pid
  [ -s "$PIDFILE" ] || return 1
  pid=$(tr -dc 0-9 < "$PIDFILE")
  [ -n "$pid" ] && [ -r "/proc/$pid/cmdline" ] || return 1
  owns_pid "$pid" || return 1
  echo "$pid"
}
owns_pid() {
  local args
  alive "$1" || return 1
  args=$(tr '\0' ' ' < "/proc/$1/cmdline" 2>/dev/null) || return 1
  case "$args" in *qemu-system-*"unix:$QMP,"*) return 0 ;; esac
  return 1
}
# A process that exists and is not a zombie (a zombie keeps its /proc entry, with an empty cmdline).
alive() {
  local st
  st=$(cut -d')' -f2 "/proc/$1/stat" 2>/dev/null | cut -d' ' -f2) || return 1
  [ -n "$st" ] && [ "$st" != Z ] && [ "$st" != X ]
}
running_count() {
  local n=0 d pid args
  for d in "$ROOT"/*/; do
    [ -s "$d/qemu.pid" ] || continue
    pid=$(tr -dc 0-9 < "$d/qemu.pid")
    [ -n "$pid" ] || continue
    alive "$pid" || continue
    args=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null) || continue
    case "$args" in *qemu-system-*"unix:${d%/}/qmp.sock,"*) n=$((n + 1)) ;; esac
  done
  echo "$n"
}

qmp() { python3 "$DEVVM_QMP" "$QMP" "$@"; }

port_free() { ! (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
ports_of_others() {
  local f
  for f in "$ROOT"/*/settings; do
    [ "$f" = "$SETTINGS" ] && continue
    [ -f "$f" ] && grep -E '^(ssh_port|display_port)=' "$f" | cut -d= -f2
  done
  return 0
}
pick_port() { # $1 = preferred (may be empty), $2 = first port of the range to scan
  local p taken
  taken=" $(ports_of_others | tr '\n' ' ') "
  if [ -n "$1" ] && port_free "$1"; then echo "$1"; return; fi
  for p in $(seq "$2" $(( $2 + 99 ))); do
    case "$taken" in *" $p "*) continue ;; esac
    if port_free "$p"; then echo "$p"; return; fi
  done
  die "no free localhost port in $2-$(( $2 + 99 ))"
}

# ── build ───────────────────────────────────────────────────────────────────────────────────────────────────────
do_build() {
  [ -f "$FLAKE/flake.nix" ] || die "$FLAKE has no flake.nix: run from a rime-os checkout or pass --flake DIR"
  mkdir -p "$DIR"
  local args=(build "path:$(cd "$FLAKE" && pwd)#legacyPackages.$SYSTEM.dev-vm.$S_desktop" --out-link "$DIR/vm")
  [ -z "$S_shell" ] || args+=(--override-input rime-shell "path:$S_shell")
  # shellcheck disable=SC2206 # NIX_BUILD_FLAGS is a list of words on purpose
  [ -z "${NIX_BUILD_FLAGS:-}" ] || args+=($NIX_BUILD_FLAGS)
  say "building the $S_desktop system${S_shell:+ with rime-shell from $S_shell}"
  local t0
  t0=$(now)
  nix "${args[@]}" || die "build failed (the running VM, if any, is untouched)"
  BUILD_SECONDS=$(since "$t0")
  # The out-link is a GC root (nix registers it in gcroots/auto and replaces it only after a successful build), so
  # the built system's closure survives the coordinator's garbage collection.
  save_settings
  say "built in ${BUILD_SECONDS} s: $(readlink "$DIR/vm")"
}

# ── start ───────────────────────────────────────────────────────────────────────────────────────────────────────
ensure_key() {
  [ -f "$KEY" ] && return 0
  # A fresh TEST key for this VM only; the comment names the VM, not the machine it was made on.
  ssh-keygen -q -t ed25519 -N "" -C "dev-vm-$NAME" -f "$KEY"
}

do_start() {
  local pid
  if pid=$(vm_pid); then say "already running (pid $pid)"; show_access; return 0; fi
  [ -c /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ] || die "/dev/kvm is not usable here: dev-vm never falls back to TCG"
  local avail
  avail=$(grep -E '^MemAvailable:' /proc/meminfo | tr -dc 0-9)
  avail=$(( avail / 1024 ))
  [ "$avail" -ge "$MIN_AVAIL_MB" ] || die "MemAvailable is ${avail} MB, below ${MIN_AVAIL_MB} MB (RIME_DEVVM_MIN_AVAIL_MB)"
  local n
  n=$(running_count)
  [ "$n" -lt "$MAX_RUNNING" ] || die "$n dev VMs are running already (RIME_DEVVM_MAX=$MAX_RUNNING): dev-vm list, dev-vm --name N stop"
  [ -L "$DIR/vm" ] && [ -x "$DIR/vm/bin/run-rime-dev-vm-vm" ] || do_build
  ensure_key

  S_ssh_port=$(pick_port "$S_ssh_port" 2222)
  local display=(-display none)
  case "$S_display" in
    spice) S_display_port=$(pick_port "$S_display_port" 5930)
           display=(-display none -spice "port=$S_display_port,addr=127.0.0.1,disable-ticketing=on") ;;
    vnc) S_display_port=$(pick_port "$S_display_port" 5900)
         display=(-display none -vnc "127.0.0.1:$(( S_display_port - 5900 ))") ;;
    *) S_display_port="" ;;
  esac
  save_settings

  local kparams=""
  [ -z "$S_session" ] || kparams="rime.devvm.session=$S_session"
  [ "$S_greeter" != 1 ] || kparams="$kparams rime.devvm.greeter"

  rm -rf "$DIR/tmp"; mkdir -p "$DIR/tmp"
  rm -f "$PIDFILE" "$QMP" "$DIR/serial.sock"
  local cred
  cred=$(base64 -w0 < "$KEY.pub")
  local opts=(
    "${display[@]}"
    -qmp "unix:$QMP,server=on,wait=off"
    -chardev "socket,id=ser0,path=$DIR/serial.sock,server=on,wait=off,logfile=$SERIAL,logappend=on"
    -serial chardev:ser0
    -pidfile "$PIDFILE"
    -smbios "type=11,value=io.systemd.credential.binary:devvm.authorized_keys=$cred"
  )
  [ -z "$S_cpus" ] || opts+=(-smp "$S_cpus")

  touch "$SERIAL"
  SERIAL_OFFSET=$(stat -c %s "$SERIAL")
  echo "==== dev-vm start $(date -u +%FT%TZ) $(readlink "$DIR/vm")" >> "$DIR/qemu.log"
  # The system this boot runs gets its own root: a later `build` moves `vm`, and the running guest still reads its
  # store paths over virtiofs.
  nix-store --realise "$(readlink -f "$DIR/vm")" --add-root "$DIR/booted" > /dev/null || die "cannot root the booted system"
  START_T0=$(now)
  # The run script starts virtiofsd for the shared store, then execs QEMU; setsid detaches it from this shell (and
  # from the podman exec session that ran it), launch.py reaps what it starts (the container's PID 1 never does).
  NIX_DISK_IMAGE=$DISK TMPDIR=$DIR/tmp USE_TMPDIR=1 \
    QEMU_NET_OPTS="hostfwd=tcp:127.0.0.1:$S_ssh_port-:22" \
    QEMU_KERNEL_PARAMS="$kparams" QEMU_OPTS="${opts[*]}" \
    setsid -f python3 "$DEVVM_LAUNCH" "$DIR/booted/bin/run-rime-dev-vm-vm" >> "$DIR/qemu.log" 2>&1 < /dev/null

  local i
  for i in $(seq 1 300); do
    vm_pid > /dev/null && [ -S "$QMP" ] && break
    [ "$i" -lt 300 ] || { tail -n 30 "$DIR/qemu.log" >&2; die "QEMU did not start (log: $DIR/qemu.log)"; }
    sleep 0.1
  done
  local kvm
  kvm=$(qmp kvm) || die "QMP did not answer"
  if [ "$kvm" != "enabled present" ]; then
    qmp quit || true
    die "QEMU is not running on KVM ($kvm): stopped it"
  fi
  say "started (pid $(vm_pid), KVM ${kvm%% *}) from $(readlink "$DIR/vm")"
  show_access
  BOOT_TIMES=""
  if [ "$OPT_WAIT" = 1 ]; then
    wait_login 300
    BOOT_TIMES="login_prompt_s=$WAITED"
    wait_ssh 300
    BOOT_TIMES="$BOOT_TIMES ssh_s=$WAITED"
    if [ "$S_greeter" != 1 ]; then
      wait_desktop 300
      BOOT_TIMES="$BOOT_TIMES desktop_s=$WAITED"
    fi
    echo "$(date -u +%FT%TZ) $CMD desktop=$S_desktop${BUILD_SECONDS:+ build_s=$BUILD_SECONDS}${STOP_SECONDS:+ stop_s=$STOP_SECONDS} $BOOT_TIMES" >> "$TIMINGS"
  fi
}

show_access() {
  echo "  ssh:      dev-vm${NAME:+ --name $NAME} ssh   (port $S_ssh_port on 127.0.0.1, key $KEY)"
  case "$S_display" in
    spice) echo "  display:  spice://127.0.0.1:$S_display_port (remote-viewer on the host)" ;;
    vnc) echo "  display:  vnc://127.0.0.1:$S_display_port" ;;
    *) echo "  display:  headless: dev-vm screenshot FILE" ;;
  esac
  echo "  serial:   $SERIAL"
  echo "  login:    rime / rime (TEST account)"
}

# ── waiting ─────────────────────────────────────────────────────────────────────────────────────────────────────
WAITED=""
serial_since_start() { tail -c "+$(( ${SERIAL_OFFSET:-0} + 1 ))" "$SERIAL" 2>/dev/null; }
wait_login() { # $1 = timeout s. Measured from START_T0 when this run started the VM.
  local t0=${START_T0:-$(now)} deadline=$((SECONDS + $1))
  until serial_since_start | grep -a -E 'login: ' > /dev/null; do
    vm_pid > /dev/null || die "the VM stopped before the login prompt (serial: $SERIAL, QEMU: $DIR/qemu.log)"
    [ "$SECONDS" -lt "$deadline" ] || die "no login prompt on the serial console within $1 s ($SERIAL)"
    sleep 0.2
  done
  WAITED=$(since "$t0")
  say "login prompt after ${WAITED} s"
}
ssh_base() {
  local u=rime
  [ "$OPT_ROOT" = 0 ] || u=root
  SSH_CMD=(ssh -i "$KEY" -p "$S_ssh_port" -o StrictHostKeyChecking=no -o "UserKnownHostsFile=$DIR/known_hosts"
    -o LogLevel=ERROR -o ConnectTimeout=5 -o BatchMode=yes "$u@127.0.0.1")
}
wait_ssh() {
  local t0=${START_T0:-$(now)} deadline=$((SECONDS + $1))
  ssh_base
  until "${SSH_CMD[@]}" true 2> /dev/null; do
    vm_pid > /dev/null || die "the VM stopped"
    [ "$SECONDS" -lt "$deadline" ] || die "ssh did not answer within $1 s"
    sleep 1
  done
  WAITED=$(since "$t0")
  say "ssh answers after ${WAITED} s"
}
# The desktop is up when the Shell answers its IPC (in a Hyprland session) or, in any other session, when the user's
# compositor socket exists and logind names the session's display. Runs in the guest, so nothing in it expands here.
# shellcheck disable=SC2016
DESKTOP_PROBE='export XDG_RUNTIME_DIR=/run/user/$(id -u)
if ls "$XDG_RUNTIME_DIR"/hypr/*/.socket.sock >/dev/null 2>&1; then
  WAYLAND_DISPLAY=$(cd "$XDG_RUNTIME_DIR" && ls wayland-? | head -n1) || exit 1
  export WAYLAND_DISPLAY
  qs -c /run/current-system/sw/share/rime-shell ipc show 2>/dev/null | grep dashboard-home >/dev/null
else
  ls "$XDG_RUNTIME_DIR"/wayland-? >/dev/null 2>&1 && loginctl show-user "$(id -un)" -p Display --value | grep . >/dev/null
fi'
wait_desktop() {
  local t0=${START_T0:-$(now)} deadline=$((SECONDS + $1))
  ssh_base
  until "${SSH_CMD[@]}" "$DESKTOP_PROBE" 2> /dev/null; do
    vm_pid > /dev/null || die "the VM stopped"
    [ "$SECONDS" -lt "$deadline" ] || die "the desktop was not up within $1 s"
    sleep 1
  done
  WAITED=$(since "$t0")
  say "desktop up after ${WAITED} s"
}

# ── stop ────────────────────────────────────────────────────────────────────────────────────────────────────────
do_stop() {
  local pid i
  if ! pid=$(vm_pid); then
    say "not running"
    rm -f "$PIDFILE"
    return 0
  fi
  local t0
  t0=$(now)
  qmp powerdown > /dev/null 2>&1 || true
  local polls=$(( STOP_WAIT * 10 ))
  for i in $(seq 1 "$polls"); do
    alive "$pid" || break
    [ "$i" -lt "$polls" ] || { say "no power-off after $STOP_WAIT s: quitting QEMU"; qmp quit > /dev/null 2>&1 || true; }
    sleep 0.1
  done
  for i in $(seq 1 50); do alive "$pid" || break; sleep 0.1; done
  if owns_pid "$pid"; then
    kill -TERM "$pid"
    sleep 2
  fi
  ! alive "$pid" || die "QEMU pid $pid is still running"
  rm -f "$PIDFILE"
  STOP_SECONDS=$(since "$t0")
  say "stopped in ${STOP_SECONDS} s"
}

# ── commands ────────────────────────────────────────────────────────────────────────────────────────────────────
case "$CMD" in
  help) usage ;;
  build)
    apply_build_options
    do_build ;;
  start)
    was="$S_desktop|$S_shell"
    apply_build_options
    apply_start_options
    # A changed desktop or Shell source needs its system built before it can boot.
    if [ -L "$DIR/vm" ] && [ "$was" != "$S_desktop|$S_shell" ] && ! vm_pid > /dev/null; then do_build; fi
    do_start ;;
  rebuild)
    apply_build_options
    apply_start_options
    do_build
    if vm_pid > /dev/null; then do_stop; fi
    do_start
    say "rebuild: build ${BUILD_SECONDS} s, stop ${STOP_SECONDS:-0} s, boot: ${BOOT_TIMES:-not waited for} (timings: $TIMINGS)" ;;
  stop) do_stop ;;
  status)
    if pid=$(vm_pid); then state="running (pid $pid)"; else state=stopped; fi
    echo "$NAME: $state"
    echo "  desktop:  $S_desktop${S_session:+ (session $S_session)}${S_shell:+, rime-shell from $S_shell}"
    if [ -L "$DIR/vm" ]; then echo "  system:   $(readlink "$DIR/vm")"; else echo "  system:   not built"; fi
    if [ -f "$DISK" ]; then echo "  disk:     $DISK ($(du -h "$DISK" | cut -f1) used)"; else echo "  disk:     none yet (made at first start)"; fi
    [ -z "$S_ssh_port" ] || show_access ;;
  list)
    for d in "$ROOT"/*/; do
      [ -d "$d" ] || continue
      n=$(basename "$d")
      valid_name "$n" || continue
      # sed reads to the end: head would close the pipe early, and under pipefail the SIGPIPE ends this loop
      RIME_DEVVM_ROOT=$ROOT "$0" --name "$n" status | sed -n 1,2p
    done ;;
  wait)
    what=${ARGS[0]:-ssh}
    vm_pid > /dev/null || die "not running"
    case "$what" in
      login) wait_login "${OPT_TIMEOUT:-300}" ;;
      ssh) wait_ssh "${OPT_TIMEOUT:-300}" ;;
      desktop) wait_desktop "${OPT_TIMEOUT:-300}" ;;
      *) usage_error "wait: login, ssh or desktop" ;;
    esac ;;
  ssh)
    vm_pid > /dev/null || die "not running: dev-vm start"
    ssh_base
    if [ -t 0 ] && [ "${#ARGS[@]}" -eq 0 ]; then exec "${SSH_CMD[@]/BatchMode=yes/BatchMode=no}"; fi
    exec "${SSH_CMD[@]}" "${ARGS[@]}" ;;
  screenshot)
    [ "${#ARGS[@]}" -ge 1 ] || usage_error "screenshot FILE"
    vm_pid > /dev/null || die "not running"
    out=${ARGS[0]}
    case "$out" in /*) ;; *) out=$PWD/$out ;; esac
    tmp=$DIR/tmp/screendump-$$.png
    qmp screendump "$tmp" || die "screendump failed"
    mkdir -p "$(dirname "$out")"
    mv -f "$tmp" "$out"
    say "$out" ;;
  type)
    [ "${#ARGS[@]}" -ge 1 ] || usage_error "type TEXT"
    vm_pid > /dev/null || die "not running"
    qmp type "${ARGS[*]}" ;;
  console)
    vm_pid > /dev/null || die "not running"
    say "serial console (Ctrl-] leaves)"
    exec socat -,raw,echo=0,escape=0x1d "UNIX-CONNECT:$DIR/serial.sock" ;;
  log)
    n=${ARGS[0]:-60}
    [[ "$n" =~ ^[0-9]+$ ]] || usage_error "log N"
    tail -n "$n" "$SERIAL" ;;
  reset)
    [ "$OPT_YES" = 1 ] || die "reset deletes $DISK (the VM's home directories and state): dev-vm --name $NAME reset --yes"
    if vm_pid > /dev/null; then die "running: dev-vm --name $NAME stop first"; fi
    rm -f "$DISK" "$DIR/known_hosts"
    say "deleted $DISK; the next start formats a fresh disk" ;;
  *) usage_error "unknown command '$CMD'" ;;
esac
