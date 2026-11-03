# plymouth: one fix on top of Fedora's build

`plymouth-keyboard-removed-handler.patch` is applied to Fedora's own
`plymouth` SRPM (pinned by NVR and sha256 in `Containerfile.core`, stage
`plymouth-builder`). The packages are marked `.rime1`, and both
`Containerfile.core` and `Containerfile.rime` refuse to build with any
other plymouth. `tests/test-rime-plymouth-keyboard.sh` keeps those pieces
in place.

## The crash

`plymouth-start.service` stayed `failed (Result: core-dump)` on every boot:

```
plymouth-start.service: Main process exited, code=dumped, status=11/SEGV
#0  ply_list_node_get_data (libply.so.5)
#1  ply_keyboard_remove_input_handler (libply-splash-core.so.5)
#2  stop_script_animation (script.so)
#3  hide_splash_screen (script.so)
#4  ply_boot_splash_hide (libply-splash-core.so.5)
#5  hide_splash (plymouthd)
#6  on_boot_splash_idle (plymouthd)
```

`plymouth-quit.service` fails right after, because the daemon it should
stop is already dead.

## Why

1. plymouthd starts in the initramfs on `simpledrm` (the firmware
   framebuffer). It makes a keyboard for that renderer and hands it to the
   splash plugin.
2. A real DRM device appears later. `create_devices_for_terminal_and_renderer_type()`
   frees every simpledrm renderer before it opens the new one, and
   `free_keyboards_for_renderer()` frees their keyboards.
   `free_pixel_displays_for_renderer()` calls `pixel_display_removed_handler`
   for each display; the keyboard path never calls `keyboard_removed_handler`.
3. If the new renderer opens, its keyboard replaces the freed one in the
   plugin and nothing goes wrong. If it does not open (a DRM device with no
   usable display: `Could not get card resources`), the plugin still holds
   the freed keyboard.
4. When the splash hides, the script plugin (Rime's theme is a script
   theme) calls `ply_keyboard_remove_input_handler()` on that freed
   keyboard and plymouthd segfaults. `two-step` has no `unset_keyboard`
   and does not crash this way.

Upstream `main` still has the unpatched function (checked 2026-11-03, at
upstream commit b79b1786 from 2026-10-30).

## The fix

- `free_keyboards_for_renderer()` calls `keyboard_removed_handler` before
  it frees each keyboard, as the pixel-display path already does.
- `ply_boot_splash_unset_keyboard()` forgets the keyboard it unset.
- `on_keyboard_removed()` hands the splash any keyboard that is still
  there, so a passphrase prompt shown afterwards still takes input.

## Proof (VM, KVM, QEMU VGA with `bochs` held out of the initramfs)

The initramfs runs on simpledrm. `vgem` (a DRM device with no display) is
loaded after switch-root, while the splash is still up.

| image | disk layout | result |
|---|---|---|
| published image (`plymouth-…-6.fc45`) | plain, no fstab | `plymouth-start` core-dump, the stack above |
| published image | LUKS, separate `/boot` | the same core-dump |
| published image, nothing loaded late | plain | no crash |
| published image, `bochs` loaded late (opens fine) | plain | no crash |
| test image (`…-6.rime1.fc45`), reached with `bootc switch` | plain / LUKS / plain + `/boot/efi` | no crash, no failed units; the debug log shows `on_keyboard_removed` and then `Could not get card resources` for vgem |

Rebuild the patch from a plymouth git checkout with `git format-patch`; the
build checks that the prepared source has the handler call.
