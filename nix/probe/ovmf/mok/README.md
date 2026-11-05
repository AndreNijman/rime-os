# MOK lab: enrolling the real Rime MOK through MokManager, Secure Boot on

PLAN.md D3 + D7 and the open item "MokManager has never been clicked through" (wave 2: every OVMF run had the MOK
pre-seeded with `virt-fw-vars --add-mok`). OVMF with Secure Boot on, Fedora 45's Microsoft-signed shim, Fedora-signed
GRUB and MokManager, one varstore and one disk that carry over from boot to boot, as on one machine. The varstore
starts with Microsoft's keys only (OVMF's secboot varstore: Red Hat PK, Microsoft KEK and db) and **no MOK**.

The certificate enrolled is the **real** Rime MOK, `nix/pkgs/kernel/signing/rime-mok.pem` (public; the private key
exists only in CI). The kernel it admits is a published bootc Rime kernel that CI signed with the real key. The only
private key here is a throwaway TEST MOK made by `mkdisk.sh`, used for the "not enrolled = refused" control. Nothing
touches a host's ESP, NVRAM or disks: every disk and varstore is a file, QEMU runs in the `rime-ovmf-mok` container.

**Result (2026-11-02): `run` 6/6 PASS; the wrong-password mutant FAILS M1, R1 and V, as it must; `split` 3/3 PASS.**
Evidence: `/var/lab/rime-nix/artifacts/secureboot/mok/{run,run-mutant,split}/` (not committed).

**Wave 4 (2026-11-03): `run` 6/6 PASS again; `split` 5/5 PASS (U1-U3 now also assert the microcode names, U4/U5 read
the kernel's early loader over QEMU's gdbstub); `installer` MI1 + V + MI2 PASS, its wrong-password mutant FAILS.**
Evidence: `/var/lab/rime-nix/artifacts/boot2/mok/{run,split,installer,installer-mutant}/` (not committed).

## Run it

```
nix/probe/ovmf/mok/run.sh all                              # prep, disks, run
RIME_MOK_MUTANT=wrong-password nix/probe/ovmf/mok/run.sh run   # expected: exit 1
nix/probe/ovmf/mok/run.sh split                            # after run: early microcode as separate initrd files
nix/probe/ovmf/mok/run.sh realsig DIR                      # after run: the NixOS kernel with the release signature
nix/probe/ovmf/mok/run.sh installer ILAB                   # the NixOS installer's own mokutil path (see below)
RIME_MOK_MUTANT=wrong-password nix/probe/ovmf/mok/run.sh installer ILAB   # expected: exit 1
```

| Step | What |
|---|---|
| `prep` | builds `rime-ovmf-probe`, `rime-ovmf-tpm` and this image (`Containerfile`: + mokutil, mtools, gdb-headless); copies Fedora's shim/GRUB/MokManager from the OVMF probe's artifacts, the bootc kernel (`BOOTC_KERNEL`, default the running bootc system's own `vmlinuz`) and its `System.map` (`BOOTC_SYSMAP`), `rime-mok.pem` and nixpkgs' microcode cpios; makes the Microsoft-only varstore |
| `disks` | `mkdisk.sh`: the Fedora kernel (from the image's `kernel-core` RPM), the TEST-MOK-signed control kernel, a busybox initrd carrying `mokutil`, a GPT disk with an ESP (shim as `BOOTX64.EFI` → GRUB → `/stage.cfg`, rewritten before each boot) |
| `run` | `drive.py run`: the boots below |
| `split` | `drive.py split`: three boots of the bootc kernel on the enrolled varstore, the initrd split into Intel cpio + AMD cpio + initrd through GRUB's blscfg (three `initrd` lines) and a plain menuentry (one line, three files), and a one-file control; `retain_initrd` lets the guest compare what GRUB handed the kernel piece by piece and read the microcode file names in it. Then U4/U5: the guest CPU without the hypervisor flag, the kernel stopped at reset, and `get_microcode_blob()` / `find_microcode_in_initrd()` walked with hardware breakpoints: with the menu layout the loader finds `GenuineIntel.bin` (its size, its first patch header read from guest memory), with the initrd alone nothing (docs/nixos/boot-matrix.md, "Early microcode") |
| `installer ILAB` | ILAB = the installer lab's LAB (`nix/installer/lab/run.sh`) after `install plain`, which installed from the NixOS installer ISO on a Microsoft-keys-only varstore with `mokpw` in the answers, so `rime-install-nixos.sh`'s `nixos_mok()` ran `mokutil --import` of the lab's TEST MOK (its log: `secureboot: MOK enrolment queued`). MI1: the installed disk boots with the install's own varstore and shim starts MokManager, driven as M1; V: MokList holds exactly that TEST MOK; MI2: `iso-qualify.py firstboot` boots the installed NixOS (kernel signed by the TEST MOK) on the varstore MokManager wrote, Secure Boot on, to the greeter |
| `realsig DIR` | DIR = `nix-kernel-sign.yml`'s artefact (`<hash>.sig` + `<hash>.json`): refuses unless it names this commit's `rime` image, attaches it as `nix/pkgs/kernel/sign.nix` does (`sbattach`, `sbverify --cert rime-mok.pem`, `require = true`) and boots that NixOS kernel on the enrolled varstore. **Not run yet**: the workflow cannot be dispatched from `feat/nixos` (docs/nixos/boot-matrix.md, "The real Rime MOK") |

Defaults: `LAB=/var/lab/rime-nix/scratch/secureboot/mok`, `ART=/var/lab/rime-nix/artifacts/secureboot/mok`.

## The boots (`run`)

| Boot | What | Pass needs |
|---|---|---|
| C1 | the Rime-MOK-signed bootc kernel, before enrolment | shim refuses it ("bad shim signature"); the kernel never runs |
| I1 | shim → GRUB → Fedora-signed kernel → an initrd that runs `mokutil --import rime-mok.der` with a password hash, then reboots | `mokutil` rc 0, the key listed as pending; the guest writes MokNew + MokAuth itself |
| M1 | shim finds MokNew and starts MokManager; the driver presses keys over QMP (`send-key`) and saves a screenshot (`screendump`) of every screen | each screen seen in turn: press any key, the menu, Enroll MOK, Continue, Yes, the password, Reboot (`M1-01` ... `M1-09-*.png`); the reset happens |
| R1 | the same bootc kernel after enrolment | `SecureBoot enabled`, the Rime MOK in MokListRT and in the kernel's platform keyring, the lockdown state recorded |
| R2 | a kernel signed by the TEST MOK, which is not enrolled | still refused |
| V | `virt-fw-vars` reads the varstore | MokList holds exactly one certificate, the Rime MOK (sha256 of the DER `2fdd2f55...`) |

Every verdict needs a positive marker; a timeout is never a pass. The mutant (`RIME_MOK_MUTANT=wrong-password`) types
a wrong password at MokManager: nothing is enrolled, so M1 never reaches Reboot, R1 never boots and V finds an empty
MokList.

What I1 and R1 also show: Fedora's kernel under Secure Boot prints `Kernel is locked down from EFI Secure Boot mode`
and reads lockdown `[integrity]`; the Rime kernel reads `[none]` (docs/nixos/boot-matrix.md, "Kernel lockdown under
Secure Boot").

## Notes

- `gdb-headless`, not `gdb-minimal`: Fedora's `gdb.minimal` is built without expat, cannot read QEMU's target
  description and refuses the x86-64 register packet (`Remote 'g' packet reply is too long`). The binary is
  `/usr/libexec/gdb` (the `gdb` package adds `/usr/bin/gdb`).
- GRUB pads every initrd file to 4 bytes, the last one too: the image is the sum of the padded sizes.

- MokManager draws on the screen and mirrors its text to the serial console; the driver waits for each screen's
  text on serial ("Press any key to perform MOK management", "Perform MOK management", "Enroll the key(s)?",
  "Password:", "Reboot"), then screenshots it. A screen that never comes is a FAIL with its screenshot saved, not a
  hang.
- The MokManager password is a TEST password (`$LAB/keys/mok-password`, letters and digits only because it is typed
  as QMP key codes). On a real machine the owner types their own one-time password.
- `disk.raw` (170 MB) is scratch; delete it after a run, `run.sh disks` remakes it.
