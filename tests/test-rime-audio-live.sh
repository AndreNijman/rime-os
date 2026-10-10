#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  The creator audio and MIDI path, on the REAL machine this runs on (P1-042).
#
#      ./tests/test-rime-audio-live.sh
#
#  P1-042's acceptance is "Tablet/stylus/MIDI/pro-audio/USB audio tested", and
#  none of those five can be proved from a fixture: they are claims about a
#  running sound server, a kernel sequencer, and hardware that is either plugged
#  in or is not. So this suite measures the machine it is on and names what that
#  machine cannot answer, rather than asserting against a mock of it.
#
#  ── READ-ONLY, and that is a hard property ──────────────────────────────────
#
#  Every command in here reads. Nothing sets a default sink, changes a volume,
#  writes PipeWire or WirePlumber configuration, loads a module, or creates a
#  udev rule. Two reasons this is a rule and not a preference:
#
#    * it is expected to be run on a machine somebody is using — including over
#      ssh on a box that is playing a game — and a suite that retunes the audio
#      graph to test it is a suite nobody can afford to run;
#    * `wpctl set-default` and `pw-metadata` writes persist through
#      WirePlumber's own state directory, so "put it back afterwards" is not
#      reliably possible. Reading is.
#
#  The one thing it generates is a one-note MIDI file in its own scratch
#  directory, played into the kernel's `Midi Through` loopback — a port that
#  exists to be written to and is connected to nothing that makes sound.
#
#  ── What it can and cannot prove, by leg ────────────────────────────────────
#
#  MIDI          fully. Midi Through is a kernel loopback, so a real round trip
#                needs no instrument.
#  pro-audio     the scheduling half fully — PipeWire's data loops either hold a
#                realtime policy or they do not. The audible half (does a 64
#                frame quantum survive a busy desktop) needs a listener and is
#                not attempted.
#  USB audio     the descriptor half always — the parsing runs a second time
#                against real descriptors read off a machine with two USB
#                interfaces, so the leg is never vacuous. Whether PipeWire TOOK
#                the device needs one attached to the machine under test, and is
#                a named SKIP otherwise.
#  tablet/stylus not at all without a tablet, and there is none in this fleet.
#                Classification from udev properties is fixture-tested in
#                tests/test-rime-input.sh and is not duplicated here.
#
#  A SKIP is printed with its reason and counted, and the floor at the bottom
#  fails a run in which everything skipped — an all-skip suite reporting green
#  is the failure mode this repository has shipped before.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"

pass=0; fail=0; skip=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skp()  { printf 'SKIP  %s\n' "$1"; skip=$((skip + 1)); }
# Measured facts that are not pass/fail. A number this suite must not assert on
# — a priority, a rate, a device name — is still worth printing, because the
# whole point of the item is knowing what this machine actually does.
note() { printf '      %s\n' "$1"; }
section() { printf '\n── %s ──\n' "$1"; }

W="$(mktemp -d)"
cleanup() { rm -rf "$W"; return 0; }
trap cleanup EXIT INT TERM

printf 'rime audio-live: %s, kernel %s\n' "$(uname -n)" "$(uname -r)"

# ─────────────────────────────────────────────────────────────────────────────
#  MIDI
# ─────────────────────────────────────────────────────────────────────────────
# The interesting claim is not "ALSA has a sequencer" — it is that a note
# written to the sequencer comes back out, and that PipeWire is on the bus so a
# DAW's MIDI reaches the same graph as its audio. Those are separate facts and
# both are asserted.
section "MIDI — the ALSA sequencer, round trip"

if ! command -v aconnect >/dev/null 2>&1; then
    skp "aconnect is not installed (alsa-utils); the sequencer cannot be enumerated"
else
    ports="$W/aconnect.txt"
    aconnect -l >"$ports" 2>/dev/null

    # `Midi Through` is snd_seq_dummy's loopback. It is client 14 by convention
    # but the number is not guaranteed, so it is looked up by name — a suite
    # that hardcodes 14 breaks on a machine with a different module order.
    through="$(awk '/^client [0-9]+: .Midi Through./ {gsub(/:/,"",$2); print $2; exit}' "$ports")"
    if [ -n "$through" ]; then
        ok "the kernel offers a Midi Through loopback (client $through)"
        note "snd_seq_dummy: $(lsmod | awk '$1=="snd_seq_dummy"{print "loaded"; f=1} END{if(!f) print "NOT loaded"}')"
    else
        bad "the kernel offers a Midi Through loopback"
    fi

    # PipeWire registering as a sequencer client is what makes MIDI and audio
    # one graph. Without it a DAW's notes go through ALSA while its audio goes
    # through PipeWire, and the two are not sample-synchronous.
    if grep -qE "^client [0-9]+: 'PipeWire" "$ports"; then
        ok "PipeWire is on the ALSA sequencer bus, so MIDI and audio share a graph"
        note "PipeWire sequencer clients: $(grep -cE "^client [0-9]+: 'PipeWire" "$ports")"
        # UMP means MIDI 2.0 capable ports. Reported, not asserted: it depends
        # on the PipeWire version and is not something Rime chooses.
        grep -qE "UMP-MIDI2" "$ports" \
            && note "they advertise UMP-MIDI2 (MIDI 2.0 capable)" \
            || note "they do not advertise UMP-MIDI2"
    else
        bad "PipeWire is on the ALSA sequencer bus, so MIDI and audio share a graph"
    fi

    # ── The round trip ──────────────────────────────────────────────────────
    if [ -z "$through" ]; then
        skp "no loopback client to round-trip a note through"
    elif ! command -v aplaymidi >/dev/null 2>&1 || ! command -v aseqdump >/dev/null 2>&1; then
        skp "aplaymidi or aseqdump is missing; a note cannot be round-tripped"
    else
        # A three-event standard MIDI file, format 0, carrying ONE note: note
        # on, note off, end of track. Written here rather than committed, so
        # the test carries no binary and nothing has to explain what is in it.
        python3 - "$W/note.mid" <<'MIDI'
import struct, sys
# delta-time, event  — C4 (0x3C) on at velocity 64, off after one beat
events = bytes([0x00, 0x90, 0x3C, 0x40,
                0x60, 0x80, 0x3C, 0x40,
                0x00, 0xFF, 0x2F, 0x00])
hdr   = b"MThd" + struct.pack(">IHHH", 6, 0, 1, 96)
track = b"MTrk" + struct.pack(">I", len(events)) + events
open(sys.argv[1], "wb").write(hdr + track)
MIDI
        if [ ! -s "$W/note.mid" ]; then
            bad "the test MIDI file was generated"
        else
            ok "the test MIDI file was generated"

            # aseqdump subscribes to the loopback's read side and prints what
            # arrives. It runs until killed, so it is backgrounded, given a
            # moment to subscribe, and stopped after the note is played.
            ( aseqdump -p "${through}:0" >"$W/dump.txt" 2>"$W/dump.err" ) &
            dump_pid=$!
            for _ in $(seq 1 20); do
                [ -s "$W/dump.txt" ] && break
                kill -0 "$dump_pid" 2>/dev/null || break
                sleep 0.1
            done

            aplaymidi -p "${through}:0" "$W/note.mid" >"$W/play.log" 2>&1
            played=$?
            sleep 0.6
            kill "$dump_pid" 2>/dev/null
            wait "$dump_pid" 2>/dev/null

            [ "$played" -eq 0 ] \
                && ok "aplaymidi delivered the file to the loopback" \
                || bad "aplaymidi delivered the file to the loopback ($(head -1 "$W/play.log"))"

            # THE assertion of this section. Not "a port exists" and not "the
            # tool exited 0" — a note that was written to the sequencer was
            # read back out of it.
            if grep -qiE "note on" "$W/dump.txt"; then
                ok "a Note On written to the sequencer was read back out of it"
                note "$(grep -iE "note on" "$W/dump.txt" | head -1 | tr -s ' ')"
            else
                bad "a Note On written to the sequencer was read back out of it"
                head -5 "$W/dump.txt" "$W/dump.err" 2>/dev/null | sed 's/^/      /'
            fi

            # And the negative half: the same capture must not report a note
            # nobody played. Without this, a dump tool that printed a banner
            # containing the words would pass the assertion above.
            if [ "$(grep -ciE "note on" "$W/dump.txt")" -eq 1 ]; then
                ok "exactly the one note played appears, so the capture is not echoing noise"
            else
                bad "exactly the one note played appears (saw $(grep -ciE "note on" "$W/dump.txt"))"
            fi
        fi
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
#  Pro audio — realtime scheduling
# ─────────────────────────────────────────────────────────────────────────────
# What a creator is buying here is that the audio thread cannot be starved by
# whatever else the desktop is doing. That is a scheduling policy question and
# it is answerable exactly: either PipeWire's data loops hold a realtime policy
# or they are time-sharing with the web browser.
#
# The POLICY is asserted. The PRIORITY NUMBER is reported and deliberately not
# asserted — it is a security-posture decision (a realtime thread is a
# denial-of-service primitive) and pinning it here would redden this suite the
# day somebody deliberately changes it.
section "pro audio — does the audio thread hold a realtime policy"

if ! command -v ps >/dev/null 2>&1; then
    skp "ps is not available; thread scheduling cannot be read"
elif ! pgrep -u "$(id -u)" -x pipewire >/dev/null 2>&1; then
    skp "no PipeWire is running for this user; there is no audio graph to measure"
else
    # Every thread of every PipeWire-family process, so a data loop that lost
    # its policy in ONE of them is still caught. pipewire-pulse is included on
    # purpose: it carries the graph for every PulseAudio-API client, which is
    # most of them.
    threads="$W/threads.txt"
    : > "$threads"
    for name in pipewire wireplumber pipewire-pulse; do
        for p in $(pgrep -u "$(id -u)" -x "$name" 2>/dev/null); do
            ps -o tid=,cls=,rtprio=,comm= -T -p "$p" 2>/dev/null \
                | sed "s|\$| ${name}|" >> "$threads"
        done
    done

    loops="$(awk '$4 ~ /^data-loop/' "$threads" | wc -l)"
    if [ "$loops" -eq 0 ]; then
        bad "PipeWire has at least one data loop thread to schedule"
        head -20 "$threads" | sed 's/^/      /'
    else
        ok "PipeWire has data loop threads ($loops of them)"

        # FF is SCHED_FIFO, RR is SCHED_RR. TS is SCHED_OTHER — the ordinary
        # time-sharing class, which is what a data loop must NOT be in.
        ts="$(awk '$4 ~ /^data-loop/ && $2 !~ /^(FF|RR)$/ {print $5 " tid " $1 " is " $2}' "$threads")"
        if [ -z "$ts" ]; then
            ok "every data loop thread holds a realtime policy, not time-sharing"
            note "policies: $(awk '$4 ~ /^data-loop/ {print $5 "=" $2 "/" $3}' "$threads" | tr '\n' ' ')"
        else
            bad "every data loop thread holds a realtime policy, not time-sharing"
            printf '      %s\n' "$ts"
        fi
    fi

    # How the priority was granted, which is the part Rime actually controls.
    # Reported in full because the two paths have different failure modes: an
    # rlimit is applied by pam_limits at login and is invisible to a process
    # started any other way, while rtkit is a D-Bus service that can be down.
    hard_rt="$(ulimit -Hr 2>/dev/null || echo '?')"
    note "this session's RLIMIT_RTPRIO ceiling: ${hard_rt}"
    note "this session's RLIMIT_MEMLOCK: $(ulimit -Hl 2>/dev/null || echo '?') KiB"
    for p in $(pgrep -u "$(id -u)" -x pipewire 2>/dev/null); do
        note "pipewire pid $p: $(awk '/Max realtime priority/ {print "rtprio " $4 "/" $5} /Max locked memory/ {print "memlock " $4}' "/proc/$p/limits" 2>/dev/null | tr '\n' ' ')"
    done

    # rtkit is PipeWire's documented fallback when rlimits give it nothing, so
    # its state is part of the answer either way.
    if command -v systemctl >/dev/null 2>&1; then
        rk="$(systemctl is-active rtkit-daemon 2>/dev/null || true)"
        note "rtkit-daemon: ${rk:-absent}"
        if [ "$rk" = "active" ] && command -v busctl >/dev/null 2>&1; then
            note "rtkit ceiling: $(busctl --system get-property org.freedesktop.RealtimeKit1 \
                /org/freedesktop/RealtimeKit1 org.freedesktop.RealtimeKit1 \
                MaxRealtimePriority 2>/dev/null || echo unreadable)"
        fi
    fi

    # ── The rlimit file Rime itself ships ───────────────────────────────────
    # It exists for `gamescope --rt`, and it is also, on this image, the thing
    # that gives PipeWire its realtime priority at all — the @pipewire group
    # that PipeWire's own limits file targets ships EMPTY on Fedora, so that
    # file never matches anybody. Asserted because the gaming feature silently
    # degrades without it: gamescope logs the failure and carries on at normal
    # priority.
    lim="$root/files/system/limits/30-rime-gaming-rtprio.conf"
    if [ ! -f "$lim" ]; then
        bad "the image ships an rtprio limits file"
    else
        if grep -qE '^\s*@[a-z]+\s+-\s+rtprio\s+[1-9]' "$lim"; then
            ok "the image's limits file grants a non-zero rtprio, so --rt can be acquired"
            note "grants: $(grep -E '^\s*@' "$lim" | tr -s ' ' | tr '\n' ';')"
        else
            bad "the image's limits file grants a non-zero rtprio, so --rt can be acquired"
        fi
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
#  Pro audio — the diagnostics a creator needs when it crackles
# ─────────────────────────────────────────────────────────────────────────────
# Not assertions. Whether these tools ship is a packaging decision, and a red
# test here would just be this suite disagreeing with the image's package list
# every time it runs. It is reported because "my audio drops out" has no
# investigable answer on a machine with none of them.
section "pro audio — latency diagnostics on this image"

for t in pw-top pw-metadata pw-cli pw-dump wpctl pactl pw-jack jack_control \
         aseqdump aplaymidi amidi alsa-info; do
    printf '      %-14s %s\n' "$t" "$(command -v "$t" 2>/dev/null || echo ABSENT)"
done
if command -v pw-metadata >/dev/null 2>&1; then
    note "quantum/rate: $(pw-metadata -n settings 2>/dev/null | tr -s ' \n' ' ' | head -c 200)"
else
    note "quantum and sample rate are NOT readable on this image: pw-metadata is absent,"
    note "and wpctl settings covers WirePlumber's own settings, not PipeWire's clock.*"
fi

# ─────────────────────────────────────────────────────────────────────────────
#  USB audio
# ─────────────────────────────────────────────────────────────────────────────
# A USB audio interface is the one piece of creator hardware most likely to be
# on a creator's desk, and the thing that goes wrong with it is not "no sound".
# It is that the kernel took the device and PipeWire did not, so it is
# invisible to every application the user runs while `aplay -l` insists it is
# there. Those are two separate claims and the leg asserts them separately.
#
# ── Why this runs TWICE ─────────────────────────────────────────────────────
#
# /proc/asound cannot be isolated by HOME or by PATH, so on a machine with
# nothing plugged in this leg can only skip — and it is a skip on the L16 and on
# katana whenever the interfaces are unplugged. A leg that only ever skips is a
# leg nobody notices has broken.
#
# So the descriptor-parsing half is ALSO run against an embedded fixture, every
# time, on every machine. The fixture is not invented: the two `cards` entries
# and both `stream0` files below were read, read-only, off a machine with a FIIO
# KA11 DAC and a Thronmax MDrill One Pro microphone attached. So the parsing is
# exercised against what the kernel really writes — including the awkward parts,
# a device whose Playback status is "Running" with a momentary frequency and a
# sync endpoint, and one that carries both a Playback and a Capture section.
#
# A fixture card is not in the running graph, so the PipeWire half is skipped by
# name in the fixture pass rather than failed.
usb_leg() {
    local root="$1" label="$2" fixture="$3"
    local cards="$root/cards"

    if [ ! -r "$cards" ]; then
        skp "$label: $cards is not readable; no ALSA card list to check"
        return
    fi

    # The USB-Audio marker is on the SAME line as the card index — the line
    # after it is the long name. An earlier version printed the PREVIOUS line
    # with it and took the index from that, which silently read the wrong card
    # number; it went unnoticed because the only machine to hand had no USB
    # interface, so the loop body never ran. Hence the fixture pass.
    local usb
    usb="$(awk '/USB-Audio/ {print}' "$cards" | sed 's/^ *//' | tr -s ' ')"

    if [ -z "$usb" ]; then
        skp "$label: no USB-Audio class device is attached; the live leg needs one plugged in"
        note "cards present: $(awk '/^ *[0-9]+ \[/ {gsub(/[][]/,""); print $2}' "$cards" | tr '\n' ' ')"
        return
    fi

    local line idx name st
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        idx="$(printf '%s' "$line" | awk '{print $1}')"
        name="$(printf '%s' "$line" | sed 's/.*USB-Audio - //')"

        # The index has to be a number, or the parse above drifted and every
        # assertion below would be about a path that does not exist.
        case "$idx" in
            ''|*[!0-9]*) bad "$label: the card index parsed out of $cards is a number (got '$idx')"
                         continue ;;
            *)           ok  "$label: card $idx parsed out of the card list as a number" ;;
        esac
        note "card $idx: $name"

        # stream0 is the kernel's own account of what the interface can do: its
        # formats, its sample rates and its endpoint transfer mode. If the
        # driver bound the device but could not parse its descriptors, this is
        # where that shows.
        st="$root/card${idx}/stream0"
        if [ ! -r "$st" ]; then
            bad "$label: card $idx exposes its stream descriptors, so the driver parsed the device"
            continue
        fi
        ok "$label: card $idx exposes its stream descriptors, so the driver parsed the device"

        # A rate list is the assertion a creator cares about: an interface the
        # driver bound but could not read the rates from offers 44100 and
        # nothing else, and no application can ask for 96k.
        local rates
        rates="$(grep -m1 -E "^ *Rates:" "$st" | sed 's/^ *Rates: *//')"
        if [ -n "$rates" ]; then
            ok "$label: card $idx advertises a sample-rate list"
            note "rates: $rates"
        else
            bad "$label: card $idx advertises a sample-rate list"
        fi

        local fmt
        fmt="$(grep -m1 -E "^ *Format:" "$st" | sed 's/^ *Format: *//')"
        if [ -n "$fmt" ]; then
            ok "$label: card $idx advertises a sample format"
            note "format: $fmt, $(grep -m1 -E "^ *Channels:" "$st" | sed 's/^ *//')"
        else
            bad "$label: card $idx advertises a sample format"
        fi

        # And the half that actually breaks in the field.
        if [ "$fixture" -eq 1 ]; then
            skp "$label: card $idx is a fixture descriptor, so it is not in the running graph"
        elif ! command -v wpctl >/dev/null 2>&1; then
            skp "$label: wpctl is absent; cannot ask PipeWire whether it took card $idx"
        elif ! pgrep -u "$(id -u)" -x pipewire >/dev/null 2>&1; then
            skp "$label: no PipeWire is running for this user; cannot ask about card $idx"
        else
            # Matched on the first word of the kernel's name rather than the
            # whole string: PipeWire's node description is its own text and a
            # full match would be a test of string formatting, not of whether
            # the device was taken.
            local key
            key="$(printf '%s' "$name" | awk '{print $1}')"
            if wpctl status 2>/dev/null | pipe_has -iF "$key"; then
                ok "$label: PipeWire enumerated card $idx, so applications can select it"
            else
                bad "$label: PipeWire enumerated card $idx, so applications can select it"
                note "wpctl status does not mention '$key'"
            fi
        fi
    done <<EOF
$usb
EOF
}

section "USB audio — the kernel took it, and so did PipeWire (this machine)"
usb_leg "${RIME_ASOUND_ROOT:-/proc/asound}" "live" 0

section "USB audio — the same parsing against real descriptors, always"
FIX="$W/asound"
mkdir -p "$FIX/card0" "$FIX/card1"
cat > "$FIX/cards" <<'CARDS'
 0 [KA11           ]: USB-Audio - FIIO KA11
                      FIIO FIIO KA11 at usb-0000:00:14.0-8.2, high speed
 1 [Pro            ]: USB-Audio - Thronmax MDrill One Pro
                      30102019 Thronmax MDrill One Pro at usb-0000:00:14.0-8.3, full speed
 2 [NVidia         ]: HDA-Intel - HDA NVidia
                      HDA NVidia at 0x82080000 irq 17
 3 [sofhdadsp      ]: sof-hda-dsp - sof-hda-dsp
                      Micro_StarInternationalCo.Ltd.-KatanaGF7612UG-REV1.0-MS_17L3
CARDS
cat > "$FIX/card0/stream0" <<'S0'
FIIO FIIO KA11 at usb-0000:00:14.0-8.2, high speed : USB Audio

Playback:
  Status: Running
    Interface = 2
    Altset = 3
    Packet Size = 72
    Momentary freq = 48000 Hz (0x6.0000)
    Feedback Format = 16.16
  Interface 2
    Altset 1
    Format: S16_LE
    Channels: 2
    Endpoint: 0x03 (3 OUT) (ASYNC)
    Rates: 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000
    Data packet interval: 125 us
    Bits: 16
    Channel map: FL FR
    Sync Endpoint: 0x84 (4 IN)
    Sync EP Interface: 2
    Sync EP Altset: 1
    Implicit Feedback Mode: No
S0
cat > "$FIX/card1/stream0" <<'S1'
30102019 Thronmax MDrill One Pro at usb-0000:00:14.0-8.3, full speed : USB Audio

Playback:
  Status: Stop
  Interface 1
    Altset 1
    Format: S16_LE
    Channels: 2
    Endpoint: 0x01 (1 OUT) (SYNC)
    Rates: 8000, 11025, 16000, 22050, 32000, 44100, 48000
    Bits: 16
    Channel map: FL FR

Capture:
  Status: Stop
  Interface 2
    Altset 1
    Format: S16_LE
    Channels: 2
    Endpoint: 0x82 (2 IN) (SYNC)
    Rates: 8000, 11025, 16000, 22050, 32000, 44100, 48000
    Bits: 16
    Channel map: FL FR
S1
usb_leg "$FIX" "fixture" 1

# The fixture must name the two USB cards and NOT the two that are not USB —
# otherwise the leg would happily report an HDA card as a USB interface, and
# the "no USB device attached" skip on a machine with an HDA card would never
# be reachable.
if [ "$(awk '/USB-Audio/ {print}' "$FIX/cards" | wc -l)" -eq 2 ]; then
    ok "fixture: the card list holds exactly the two USB interfaces, not the HDA ones"
else
    bad "fixture: the card list holds exactly the two USB interfaces, not the HDA ones"
fi


# ─────────────────────────────────────────────────────────────────────────────
section "tablet and stylus"

if ! command -v libwacom-list-local-devices >/dev/null 2>&1; then
    skp "libwacom-list-local-devices is absent; a tablet could not be identified"
else
    tablets="$(libwacom-list-local-devices 2>/dev/null | grep -cE "^ *- name:" || true)"
    kernel_tablets="$(grep -clE "tablet|stylus|wacom" /proc/bus/input/devices 2>/dev/null || echo 0)"
    if [ "${tablets:-0}" -gt 0 ]; then
        ok "libwacom identifies $tablets tablet(s) on this machine"
        libwacom-list-local-devices 2>/dev/null | sed 's/^/      /' | head -20
    else
        # Not a failure. There is no tablet in this fleet, and a suite that
        # reddened for absent hardware would be red forever on both machines.
        skp "no tablet or stylus is attached to this machine; the leg needs one plugged in"
        note "libwacom knows of none, and the kernel lists $kernel_tablets input device(s) naming a tablet or stylus"
        note "udev-property classification is fixture-tested in tests/test-rime-input.sh"
    fi
fi

# The engine's tablet handling is asserted whether or not a tablet is here,
# because losing it is a silent regression: a tablet whose type falls through to
# "other" gets no settings at all, and nothing on screen says so.
GEN="${RIME_INPUT_GEN:-${root}/files/system/libexec/rime-input-apply}"
if [ ! -f "$GEN" ]; then
    bad "the input engine is present to check its tablet handling"
else
    grep -q '"ID_INPUT_TABLET", "tablet"' "$GEN" \
        && ok "the input engine classifies a tablet from its udev property" \
        || bad "the input engine classifies a tablet from its udev property"
    grep -qE '^\s*DEVICE_TYPES\s*=.*"tablet"' "$GEN" \
        && ok "tablet is one of the engine's device types, so it can carry settings" \
        || bad "tablet is one of the engine's device types, so it can carry settings"
    # Reported, not asserted — it is the state of the feature, not a defect in
    # the code. A stylus on a two-monitor desk needs its area mapped to ONE
    # output or it spans both, and no Rime control expresses that.
    note "tablet settings the engine expresses: $(grep -oE '"tablet":\s*\([^)]*\)' "$GEN" | head -1)"
    note "there is no output-area mapping control, so a stylus spans the whole desk"
fi

# ─────────────────────────────────────────────────────────────────────────────
#  The floor
# ─────────────────────────────────────────────────────────────────────────────
# Four of the five legs can skip for want of hardware, so "0 failed" on its own
# is not evidence of anything. A run in which nothing was asserted is a failed
# run, and the two legs that need no hardware beyond a sound server must have
# produced assertions.
section "the run itself"

if [ "$((pass + fail))" -eq 0 ]; then
    bad "the suite asserted something (every leg skipped, so this run proves nothing)"
else
    ok "the suite asserted something ($((pass + fail)) assertions reached)"
fi

printf '\nrime audio-live: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
