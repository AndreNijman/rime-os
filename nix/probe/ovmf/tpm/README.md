# TPM lab: PCR 7 before and after the move, TPM-only re-enrolment, the recovery key (p2-gates T1-T3)

PLAN.md D3 "TPM" and S3. OVMF with Secure Boot on, a software TPM 2.0 (swtpm), Fedora 45's Microsoft-signed shim and
Fedora-signed GRUB, one varstore, one TPM state and one LUKS2 data volume that carry over from boot to boot, as on one
machine. Everything is a file under the lab's scratch directory and every key is a THROWAWAY TEST key. Nothing here
touches a host's ESP, NVRAM, TPM or disks.

**Result (2026-11-02): main 10/10 PASS; the mutant scenario FAILS at M4, as it must.**

## Run it

```
nix/probe/ovmf/tpm/run.sh all                       # prep, system, disks, run main
nix/probe/ovmf/tpm/run.sh run mutant-bootc-reenrol  # expected: exit 1, M4 FAIL
```

| Step | What |
|---|---|
| `prep` | builds `rime-ovmf-tpm` (this Containerfile, on the OVMF probe image `../Containerfile`), copies Fedora 45's shim/GRUB and bootupd's static GRUB config from the OVMF probe's artifacts, makes the TEST MOK, signs the lab kernel with a detached signature, makes a Secure Boot varstore with the TEST MOK in MokList (`virt-fw-vars --add-mok`) |
| `system` | builds `systems.nix`: a minimal NixOS with Rime's boot hook (`nix/modules/boot.nix`) and `nix/modules/luks-tpm.nix`, its kernel the TEST-MOK-signed one |
| `disks` | `../boothook/mkdisk.sh bls` (nixos-install runs the Rime boot hook: shim → GRUB → BLS entry → `/rime/nixos/<hash>-…-signed.efi`), then `probes` |
| `probes` | `mkprobe.sh`: the "before" probe disks and the LUKS2 data disk (TEST passphrase + a TEST recovery key made by `systemd-cryptenroll --recovery-key`, the way the installer enrols one) |
| `run main` | `drive.py main`: the boot sequence below |

Defaults: `LAB=/var/lab/rime-nix/scratch/tpmkeymap/lab`, `ART=/var/lab/rime-nix/artifacts/tpmkeymap/tpm`.

## The systems

| Boot | Chain | Kernel verified by |
|---|---|---|
| **P1, P2** "before", Fedora | shim → GRUB → Fedora 45 `kernel-core-7.2.13-300.fc45` vmlinuz, as Fedora signed it, busybox initrd | shim's built-in vendor cert (`fedoraca`) |
| **P3, P4** "before", bootc-Rime shape | the same vmlinuz with Fedora's signature removed and the TEST MOK's attached | MokList (the TEST MOK) |
| **N1-N6** "after", NixOS Rime | shim → GRUB → the Rime boot hook's BLS entry → nixpkgs `linuxPackages_latest` with the TEST MOK's detached signature attached (`kernel.nix`'s signing path), the NixOS systemd initrd, which unlocks the LUKS2 volume (`rd.luks.name=`) | MokList (the TEST MOK) |

bootc Rime's kernels are signed with Rime's MOK (APEX_SB_*) and verified through MokList; NixOS Rime's are signed with
the same MOK by `nix-kernel-sign.yml`. P3 is that shape with a TEST MOK; N1 is NixOS on it. P2 is the Fedora chain for
contrast.

## Results

`$ART/main/results.tsv`, `$ART/main/pcr7.txt` (2026-11-02, 08:05-08:12 AWST):

| Boot | Verdict | PCR 7 (sha256) / what |
|---|---|---|
| P1 | PASS | `078ded20…5c92` warm-up, Fedora chain |
| P2 | PASS | **T1** Fedora chain: `078ded20dcf80290edcf47e166481ab3a97a147524bd8622ae4ebf2431645c92` |
| P3 | PASS | **T1** bootc-Rime chain (TEST MOK): `345554e07a5c993bdbb11df388d7db727aef56994730e294ff839842e297b52c` |
| P4 | PASS | the same again: stable across boots |
| N1 | PASS | **T2** NixOS Rime on the same shim/GRUB/MOK: `345554e0…b52c` **= P3**. Passphrase typed once (no TPM slot yet); a TPM slot is sealed to P3's value (the bootc-era slot) |
| N2 | PASS | the bootc-era slot unseals on NixOS: **0 asks**. Then the slot is re-sealed to P2's value (a slot this chain does not satisfy, as after a Secure Boot key change) |
| N3 | PASS | the stale slot is refused, the passphrase is typed (1 ask); `rime-luks-tpm-reenrol@<uuid>.service` (`rime-luks-enroll --tpm-only`) wipes and re-seals the TPM slot to PCR 7; `kept: unchanged 002ea2bf…`; slot 0 (passphrase) and slot 1 (recovery) are byte-identical before and after (`sha256=47d854cf…`, `a2752f7a…`) |
| N4 | PASS | the re-enrolled slot unseals: 0 asks. Then `tpm2_clear` |
| N5 | PASS | **T3** TPM cleared (`Failed to unseal secret using TPM2: State not recoverable`): the PRE-EXISTING recovery key, made with the disk before any of this, is typed and unlocks (1 ask) |
| N6 | PASS | **T3** no TPM device at all: the pre-existing recovery key unlocks (1 ask) |

Every verdict needs positive markers: SecureBoot enabled (the efivar in the probe, `mokutil --sb-state` on NixOS), the
TEST MOK loaded by the NixOS kernel, the kernel being the boot hook's signed copy, the number of initrd passphrase
asks, `/dev/mapper/cryptdata` open, and a random marker written in N1 read back from the decrypted volume. PCR 7 is
read from sysfs (`/sys/class/tpm/tpm0/pcr-sha256/7`) on every boot, also with `tpm2_pcrread sha256:7` on NixOS
(they must agree), and the firmware event log is saved and replayed by `tpm2_eventlog`: the replayed PCR 7 must equal
the value read, so the decoded log is the one that made it. A timeout is never a pass.

### Why P2 and P3 differ, and why N1 = P3 (T2)

The PCR 7 events (`$ART/main/<boot>.pcr7-events.txt`, sha256 digest prefixes):

```
                                  P2 (Fedora)       P3 (bootc-Rime)   N1 (NixOS Rime)
EV_EFI_VARIABLE_DRIVER_CONFIG     SecureBoot PK KEK db dbx: identical in all three
EV_SEPARATOR                      df3f619804a92fdb  (same)            (same)
EV_EFI_VARIABLE_AUTHORITY db      4d4a8e2c74133bbd  (same)            (same)    Microsoft UEFI CA: firmware verified shim
EV_EFI_VARIABLE_AUTHORITY SbatLevel bcf4d1ff6bf02f94 (same)           (same)    shim's SBAT policy
EV_EFI_VARIABLE_AUTHORITY MokListRT ad5901fd581e6640 (same)           (same)    shim's vendor cert (fedoraca): shim verified GRUB
EV_EFI_VARIABLE_AUTHORITY MokListRT -               6377e7a34d55e3d6  6377e7a34d55e3d6   the TEST MOK: shim verified the kernel
```

shim measures each certificate that authorises an image into PCR 7 once per boot. GRUB is signed by `fedoraca`, so
that certificate is measured on every chain. The Fedora kernel is signed by `fedoraca` too, which is already
measured, so P2 has no event for it. A MOK-signed kernel adds one `EV_EFI_VARIABLE_AUTHORITY` event whose data is the
MOK certificate (its `VariableData` in `P3.eventlog.yaml` holds the DER of "Rime TPM lab TEST MOK"). PCR 7 therefore
records **which key signed the kernel, not the kernel**. NixOS Rime's kernel is signed by the same MOK as bootc
Rime's, so N1's event list matches P3's digest for digest (only the event numbers differ: the NixOS path logs
more events into other PCRs), and PCR 7 is the same value.

Consequences:
- The move to NixOS on katana's chain (Fedora shim + GRUB + Rime's MOK) does **not** change PCR 7, so a bootc-era TPM
  slot keeps unsealing (N2: 0 asks). No re-enrolment is needed for the move itself.
- PCR 7 does change when the Secure Boot policy changes: a different MOK or kernel signer, a new db/dbx/KEK/PK,
  SbatLevel, a new shim vendor cert, Secure Boot off. That is what `rime-luks-tpm-reenrol@` is for (N3), and the
  recovery key is the way in until it runs (N5, N6).
- Not measured here: katana's real PCR 7 (its firmware's db/dbx and the real APEX MOK differ from OVMF and the TEST
  MOK). The lab proves the mechanism; the real value is TODO(handover) for the P3 GO on katana.

## The mutant: bootc's re-enrolment (S3)

`run.sh run mutant-bootc-reenrol` swaps N3's `--tpm-only` for what bootc's `rime-luks-enroll` did on every run: wipe
the recovery slot and write a new key (`--replace-recovery`, the full path). `$ART/mutant-bootc-reenrol/results.tsv`:

| Boot | Verdict | What |
|---|---|---|
| M1 | PASS | passphrase; TPM slot sealed to a PCR 7 value nothing measures |
| M2 | PASS | passphrase; full re-enrolment: `Wiped slot 1.`, new recovery key written |
| M3 | PASS | TPM unlock; `tpm2_clear` |
| **M4** | **FAIL** (expected) | the pre-existing recovery key is typed and **refused**: the initrd asks a second time (`initrd asked 2 times, expected 1`) |

So the lab catches a re-enrolment that loses the owner's recovery key. `run.sh` exits 1 on this scenario; that is
the expected result.

## Traps hit building it

- busybox `base64` ignores `-w 0` and wraps; kernel console messages land inside the base64 too (the probe initrd now
  sets `printk` to 1 and the parser drops `[ secs.usec ] …` lines; the replay check proves the result).
- The console agent's passphrase prompt has no newline, so the test unit's `RIME-ASK n` report lands at the end of
  the prompt's line: never anchor it with `^`.
- `tpm2_pcrread` on NixOS prints a tabrmd D-Bus warning before falling back to `/dev/tpmrm0`; the value still reads.
- `serial-getty@ttyS0` `vhangup()`s ttyS0 when it starts, and every later write of the stage-2 driver to it is lost
  (an independent rerun lost N4's `tpm2_clear` and `DONE` lines and failed N4, although the clear ran: N5 then read
  "State not recoverable"). `rime-tpm-lab.service` is now ordered before the getty.
- The initrd ask watcher polled every 0.2 s and wrote its count only once it saw `/dev/mapper/cryptdata`; when the
  volume opened just before switch-root the count was never written (an independent rerun: N5/N6 "asked none
  times" although the recovery key unlocked). It now writes the count at every ask and scans once more on the TERM
  that switch-root sends.
- P2's PCR 7 is the same on every run (no MOK in the chain); P3/P4/N* depend on the TEST MOK that `prep` generates,
  so a lab with fresh keys reads another value there (independent reruns with new keys: P3 = N1 = `c9d8fd95…d548e`,
  then `a1c21dd1…4ab4`; P2 = `078ded20…5c92` every time; final run 10/10 PASS, mutant M4-only FAIL:
  `/var/lab/rime-nix/artifacts/verify-tpmkeymap/tpm/{main,mutant-bootc-reenrol}`).
