# When the kernel dies

A kernel panic is the kernel stopping because it can no longer run safely.
Nothing on the machine works after it, not even the keyboard. This page says
what Rime does about one, on bootc and on NixOS alike.

## The short version

1. **The screen says what happened.** It shows "KERNEL PANIC!", "Please reboot
   your computer." and the kernel's one-line reason, on the usual blue panic
   screen. It no longer shows the last lines of the kernel log, which can be
   a wall of one driver's noise.
2. **The machine restarts by itself, 30 seconds later.** That is long enough to
   read the screen or take a photo of it. Before this, a panicked machine waited
   until you held the power button. The screen's "Please reboot your computer."
   is the kernel's fixed text; you do not have to. (The 30 seconds are counted
   by the dying kernel with a busy loop, so expect roughly 30, not exactly.)
3. **The reason is kept.** The kernel writes the end of its log into a few small
   EFI variables as it dies. At the next start systemd-pstore moves them to
   `/var/lib/systemd/pstore` and deletes the variables.
4. **After the restart you are told once**, with a notification naming the
   reason and, when the backtrace names one, the driver involved.

```
rime crash          # the last crash this machine recorded, in plain words
rime crash --json   # the same, for scripts
```

`rime crash` needs no root and changes nothing. Nothing is sent anywhere. The
record stays on the machine, and systemd removes it after 14 days.

## What is set, and where

One file holds the kernel arguments for both systems:
`files/system/kargs.d/30-rime-crash.toml`. bootc ships it as
`/usr/lib/bootc/kargs.d/30-rime-crash.toml`. On NixOS, `nix/modules/crash.nix`
reads the same file (`rime.crash.enable`, on by default).

| Argument | What it does |
|---|---|
| `panic=30` | Restart 30 s after a panic. Rime's kernel is built with `CONFIG_PANIC_TIMEOUT=0`, which means wait forever. |
| `drm.panic_screen=user` | The panic screen shows the reason, not the log tail (`kmsg`, the kernel config's default). |
| `efi_pstore.pstore_disable=0` | Keep the panic's log in EFI variables. The kernel config builds efi-pstore in but leaves it off (`CONFIG_EFI_VARS_PSTORE_DEFAULT_DISABLE=y`). |

Two small pieces read the record:

- `rime-crash-record.service` is a root oneshot that runs at every start, after
  systemd-pstore. It writes `/var/lib/rime/crash/last` with these fields: the
  time, the kernel release, the kernel's own panic, `BUG:` or `Oops:` line, and
  the driver named by the backtrace nearest the panic. It only reads records
  collected during the current boot, so an old record in the archive is not
  reported as new.
- `rime-crash-notice.timer` is a user unit. One minute into the session it
  sends one notification per crash. It only does this in the boot that
  collected the crash, never at every login.

There is no daemon. Both pieces run once and exit.

## What is left alone, on purpose

These kernel switches turn more failures into a panic, and so into a restart.
All of them stay off:

- **`panic_on_oops`**: an oops kills the task that hit it, and the rest of the
  machine usually keeps running. That includes unsaved work in other windows.
  Turning an oops into a restart trades a working session for a clean one.
  That choice belongs to a server, not to a desktop. An oops is still recorded:
  efi-pstore keeps oopses too, and `rime crash` labels the record "Last kernel
  error recorded (it did not stop the machine by itself)".
- **`softlockup_panic`**: a CPU stuck for 20+ seconds is often a driver busy in
  a loop. A log flood is exactly that, and so is a slow storage device. Many of
  these clear up by themselves. Panicking on the first one would restart
  machines that were about to recover.
- **`hung_task_panic`**: a task blocked for two minutes is usually waiting for
  a disk or a network share. Restarting the machine is the wrong answer to a
  sleeping NFS mount.
- **`hardlockup_panic`**: this is already the kernel's own call on the hardware
  that supports it. Rime adds nothing.

Any of these can be turned on per machine with an extra kernel argument.
`panic=30` then also restarts after them.

## The EFI variables: space and wear

At a panic, efi-pstore writes the end of the log until it has stored 10 KiB
of compressed data (`CONFIG_PSTORE_DEFAULT_KMSG_BYTES=10240`), in chunks of
at most 1 KiB, one EFI variable per chunk, newest log first. Flooded log
compresses well, so a chunk covers a lot of it: in the VM test, with 5,000
flood lines before the panic, the dump was 46 variables holding 10,444 bytes
(about 73 KB of log, 620 of the flood lines). A quiet log needs fewer
variables. The panic message and its backtrace are in the first variables
written (in the VM test: the panic line at the end of Part2, the rest of the
backtrace in Part1), however much a driver flooded the log before it. On x86 the kernel
refuses an EFI write that would eat into the firmware's reserve, so a full
variable store means no record, not a machine that cannot boot.

The next start reads them and deletes them: systemd-pstore's default is
`Unlink=yes`, and Rime keeps it. The VM test starts the machine once more and
checks that the firmware no longer has any of them. Each crash therefore
costs one write and one delete of about 10 KiB of variables. An oops costs the
same. Firmware variable stores are flash with wear levelling and are written
on every boot-order change anyway; a crash is not a frequent event, and Rime
never writes these variables otherwise.

## Log floods

A driver that prints thousands of lines a second is what started this. The
case: an rtl8xxxu USB Wi-Fi dongle printing its `C2H content:` hex dumps, then
soft lockups. Honestly, not much at the log level helps:

- The dongle's lines come from `print_hex_dump()` at `KERN_INFO`, with no rate
  limit. `printk_ratelimit` and `kernel.printk_ratelimit*` only cover callers
  that ask for rate limiting, and this one does not. `printk.devkmsg` only
  limits programs writing to `/dev/kmsg`, not drivers.
- Lowering the console log level (`loglevel=`) keeps those lines off the
  screen and the serial console. They still go into the log buffer, and the
  CPU still spends its time producing them.

What protects the crash itself is the two settings above. The panic screen no
longer shows the log tail, and the pstore record is written newest first, so
the panic is in it even after a flood. The driver is not blocked: blocking a
Wi-Fi driver would take Wi-Fi away from every machine that uses it to work.

## Later: the QR panic screen

The kernel can also draw the panic screen as a QR code holding the whole
panic log (`drm.panic_screen=qr_code`). A phone can then read the full report
off the screen. That needs `CONFIG_DRM_PANIC_SCREEN_QR_CODE=y`, which is written
in Rust. Rime's kernel is built without Rust (`-d RUST` in
`nix/pkgs/kernel/config.nix`, because the Fedora build root has no rustc). So
this is a kernel build change, a separate proposal, and not part of this page.
